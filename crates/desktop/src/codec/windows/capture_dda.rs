//! Decoupled DXGI Desktop Duplication (DDA) capture worker with a 3-slot
//! cross-device keyed-mutex staging ring and latest-frame overwrite semantics.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gpui::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::{
    Graphics::{
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
            D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_TEXTURE2D_DESC, ID3D11DeviceContext,
        },
        Dxgi::{
            Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC},
            DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_INVALID_CALL, DXGI_ERROR_WAIT_TIMEOUT,
            DXGI_OUTDUPL_FRAME_INFO, IDXGIKeyedMutex, IDXGIOutput1, IDXGIOutputDuplication,
            IDXGIResource,
        },
    },
    System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST},
};
use windows_core::Interface;

use super::{
    capture::{AcquiredInput, InputLease},
    common::{
        CaptureWake, ComThreadGuard, enable_per_monitor_dpi_v2, qpc_to_100ns, query_qpc_100ns,
    },
};

struct SharedSlot {
    capture_tex: ID3D11Texture2D,
    capture_mutex: IDXGIKeyedMutex,
    gpu_tex: ID3D11Texture2D,
    gpu_mutex: IDXGIKeyedMutex,
}

fn create_shared_texture_pair(
    capture_device: &ID3D11Device,
    encode_device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<SharedSlot> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width.max(1),
        Height: height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
        ..Default::default()
    };
    let mut cap_tex = None;
    unsafe { capture_device.CreateTexture2D(&desc, None, Some(&mut cap_tex)) }?;
    let cap_tex: ID3D11Texture2D = cap_tex.context("Could not allocate shared capture texture.")?;
    let cap_mutex: IDXGIKeyedMutex = cap_tex.cast()?;
    let dxgi_res: IDXGIResource = cap_tex.cast()?;
    let handle = unsafe { dxgi_res.GetSharedHandle() }?;
    let mut enc_tex: Option<ID3D11Texture2D> = None;
    unsafe { encode_device.OpenSharedResource(handle, &mut enc_tex) }?;
    let enc_tex = enc_tex.context("Could not open shared capture texture on encode device.")?;
    let enc_mutex: IDXGIKeyedMutex = enc_tex.cast()?;
    Ok(SharedSlot {
        capture_tex: cap_tex,
        capture_mutex: cap_mutex,
        gpu_tex: enc_tex,
        gpu_mutex: enc_mutex,
    })
}

fn create_shared_ring(
    capture_device: &ID3D11Device,
    encode_device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<[SharedSlot; 3]> {
    Ok([
        create_shared_texture_pair(capture_device, encode_device, width, height, format)?,
        create_shared_texture_pair(capture_device, encode_device, width, height, format)?,
        create_shared_texture_pair(capture_device, encode_device, width, height, format)?,
    ])
}

struct DdaState {
    slots: [SharedSlot; 3],
    slot_keys: [u64; 3],
    ring_gen: u64,
    slot_size: (u32, u32),
    slot_format: DXGI_FORMAT,
    latest_idx: usize,
    reading_idx: Option<usize>,
    has_desktop: bool,
    frame_seq: u64,
    present_100ns: i64,
    arrived_100ns: i64,
    error: Option<anyhow::Error>,
}

pub(super) struct DdaSlotLease {
    state: Arc<Mutex<DdaState>>,
    gpu_mutex: IDXGIKeyedMutex,
    idx: usize,
    next_key: u64,
    ring_gen: u64,
}

impl Drop for DdaSlotLease {
    fn drop(&mut self) {
        let _ = unsafe { self.gpu_mutex.ReleaseSync(self.next_key) };
        let mut guard = self.state.lock().unwrap();
        if guard.ring_gen == self.ring_gen {
            guard.slot_keys[self.idx] = self.next_key;
            guard.reading_idx = None;
        }
    }
}

pub(super) struct DdaCapture {
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<DdaState>>,
    worker: Option<JoinHandle<()>>,
    last_seq: u64,
}

impl DdaCapture {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        capture_device: ID3D11Device,
        capture_context: ID3D11DeviceContext,
        encode_device: ID3D11Device,
        output1: IDXGIOutput1,
        duplication: IDXGIOutputDuplication,
        desktop_size: (u32, u32),
        initial_format: DXGI_FORMAT,
        wake: Arc<CaptureWake>,
    ) -> Result<Self> {
        let slots = create_shared_ring(
            &capture_device,
            &encode_device,
            desktop_size.0,
            desktop_size.1,
            initial_format,
        )?;
        let state = Arc::new(Mutex::new(DdaState {
            slots,
            slot_keys: [0, 0, 0],
            ring_gen: 0,
            slot_size: desktop_size,
            slot_format: initial_format,
            latest_idx: 0,
            reading_idx: None,
            has_desktop: false,
            frame_seq: 0,
            present_100ns: 0,
            arrived_100ns: 0,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_state = state.clone();
        let worker = thread::Builder::new()
            .name("dda-capture".into())
            .spawn(move || {
                dda_worker(
                    capture_device,
                    capture_context,
                    encode_device,
                    output1,
                    duplication,
                    worker_state,
                    worker_stop,
                    wake,
                );
            })?;
        Ok(Self {
            stop,
            state,
            worker: Some(worker),
            last_seq: 0,
        })
    }

    pub(super) fn wait_initial_frame(&self, wake: &CaptureWake, limit: Duration) -> Result<bool> {
        let deadline = Instant::now() + limit;
        loop {
            let seen = wake.snapshot();
            {
                let mut state = self.state.lock().unwrap();
                if let Some(error) = state.error.take() {
                    return Err(error);
                }
                if state.has_desktop {
                    return Ok(true);
                }
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(false);
            };
            wake.wait_timeout(seen, remaining.min(Duration::from_millis(20)));
        }
    }

    pub(super) fn acquire(&mut self) -> Result<Option<AcquiredInput>> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.error.take() {
            return Err(error);
        }
        if !state.has_desktop || state.frame_seq == self.last_seq {
            return Ok(None);
        }
        let idx = state.latest_idx;
        state.reading_idx = Some(idx);
        let key = state.slot_keys[idx];
        let ring_gen = state.ring_gen;
        let texture = state.slots[idx].gpu_tex.clone();
        let gpu_mutex = state.slots[idx].gpu_mutex.clone();
        let (w, h) = state.slot_size;
        let present_100ns = state.present_100ns;
        let arrived_100ns = state.arrived_100ns;
        self.last_seq = state.frame_seq;
        drop(state);
        if let Err(err) = unsafe { gpu_mutex.AcquireSync(key, u32::MAX) } {
            let mut state = self.state.lock().unwrap();
            if state.ring_gen == ring_gen {
                state.reading_idx = None;
            }
            return Err(err.into());
        }
        Ok(Some(AcquiredInput {
            texture,
            source_width: w as i32,
            source_height: h as i32,
            capture_stamp: arrived_100ns.max(1),
            present_100ns,
            arrived_100ns,
            _lease: InputLease::Dda(DdaSlotLease {
                state: self.state.clone(),
                gpu_mutex,
                idx,
                next_key: key + 1,
                ring_gen,
            }),
        }))
    }
}

impl Drop for DdaCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn dda_worker(
    capture_device: ID3D11Device,
    capture_context: ID3D11DeviceContext,
    encode_device: ID3D11Device,
    output1: IDXGIOutput1,
    mut duplication: IDXGIOutputDuplication,
    state: Arc<Mutex<DdaState>>,
    stop: Arc<AtomicBool>,
    wake: Arc<CaptureWake>,
) {
    enable_per_monitor_dpi_v2();
    let _com = ComThreadGuard::new();
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    let log_enabled = crate::metrics::enabled();
    let mut has_desktop_yet = false;
    let mut logged_err = false;

    while !stop.load(Ordering::Acquire) {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        let acquired = unsafe { duplication.AcquireNextFrame(100, &mut info, &mut resource) };
        if let Err(ref err) = acquired
            && log_enabled
            && !logged_err
        {
            logged_err = true;
            eprintln!(
                "[remoters:win] dda_acquire_err=0x{:08x}",
                err.code().0 as u32
            );
        }
        match acquired {
            Ok(()) => {
                if !has_desktop_yet && log_enabled {
                    eprintln!(
                        "[remoters:win] dda_first_acquire frames={} resource={}",
                        info.AccumulatedFrames,
                        resource.is_some()
                    );
                }
                let arrived_100ns = query_qpc_100ns();
                let step = (|| -> Result<bool> {
                    let mut updated = false;
                    if (info.AccumulatedFrames > 0 || !has_desktop_yet)
                        && let Some(resource) = resource
                    {
                        let tex: ID3D11Texture2D = resource.cast()?;
                        let mut desc = D3D11_TEXTURE2D_DESC::default();
                        unsafe { tex.GetDesc(&mut desc) };
                        let (write_idx, target_tex, cap_mutex, key, ring_gen) = {
                            let mut guard = state.lock().unwrap();
                            if guard.slot_size != (desc.Width, desc.Height)
                                || guard.slot_format != desc.Format
                            {
                                guard.slots = create_shared_ring(
                                    &capture_device,
                                    &encode_device,
                                    desc.Width,
                                    desc.Height,
                                    desc.Format,
                                )?;
                                guard.slot_keys = [0, 0, 0];
                                guard.reading_idx = None;
                                guard.ring_gen = guard.ring_gen.wrapping_add(1);
                                guard.slot_size = (desc.Width, desc.Height);
                                guard.slot_format = desc.Format;
                            }
                            let idx = (0..3)
                                .find(|&i| i != guard.latest_idx && Some(i) != guard.reading_idx)
                                .unwrap_or(0);
                            (
                                idx,
                                guard.slots[idx].capture_tex.clone(),
                                guard.slots[idx].capture_mutex.clone(),
                                guard.slot_keys[idx],
                                guard.ring_gen,
                            )
                        };
                        unsafe {
                            cap_mutex.AcquireSync(key, u32::MAX)?;
                            capture_context.CopyResource(&target_tex, &tex);
                            cap_mutex.ReleaseSync(key + 1)?;
                        }
                        let mut guard = state.lock().unwrap();
                        if guard.ring_gen == ring_gen {
                            guard.slot_keys[write_idx] = key + 1;
                            guard.latest_idx = write_idx;
                        }
                        guard.has_desktop = true;
                        has_desktop_yet = true;
                        updated = true;
                    }

                    if updated && has_desktop_yet {
                        let present_100ns =
                            if info.AccumulatedFrames > 0 && info.LastPresentTime > 0 {
                                qpc_to_100ns(info.LastPresentTime)
                            } else {
                                arrived_100ns
                            };
                        let mut guard = state.lock().unwrap();
                        guard.present_100ns = present_100ns;
                        guard.arrived_100ns = arrived_100ns;
                        guard.frame_seq = guard.frame_seq.wrapping_add(1);
                        return Ok(true);
                    }
                    Ok(false)
                })();
                let _ = unsafe { duplication.ReleaseFrame() };
                match step {
                    Ok(true) => wake.signal(),
                    Ok(false) => {}
                    Err(err) => {
                        state.lock().unwrap().error = Some(err);
                        wake.signal();
                        break;
                    }
                }
            }
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
            Err(err)
                if err.code() == DXGI_ERROR_ACCESS_LOST
                    || err.code() == DXGI_ERROR_INVALID_CALL =>
            {
                drop(duplication);
                let mut rebuilt = None;
                for _ in 0..100 {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(20));
                    if let Ok(next) = unsafe { output1.DuplicateOutput(&capture_device) } {
                        rebuilt = Some(next);
                        break;
                    }
                }
                match rebuilt {
                    Some(next) => {
                        duplication = next;
                        has_desktop_yet = false;
                    }
                    None => {
                        state.lock().unwrap().error = Some(err.into());
                        wake.signal();
                        break;
                    }
                }
            }
            Err(err) => {
                state.lock().unwrap().error = Some(err.into());
                wake.signal();
                break;
            }
        }
    }
}
