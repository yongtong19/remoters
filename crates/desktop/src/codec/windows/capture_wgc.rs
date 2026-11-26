//! Decoupled Windows Graphics Capture (WGC) worker with a 3-slot D3D11 staging
//! ring and latest-frame overwrite semantics so slow encode frames never back
//! up DWM capture pools.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

use anyhow::{Context, Result, anyhow};
use gpui::{ID3D11Device, ID3D11Texture2D};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Graphics::{
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
                ID3D11DeviceContext,
            },
            Dxgi::{
                Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                IDXGIDevice,
            },
            Gdi::HMONITOR,
        },
        System::{
            Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
                WaitForMultipleObjects,
            },
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
    },
};
use windows_core::{IInspectable, Interface, factory};

use super::{
    capture::{AcquiredInput, InputLease},
    common::{
        CaptureWake, ComThreadGuard, DXGI_FORMAT_R16G16B16A16_FLOAT, OwnedHandle, VideoDevice,
        enable_per_monitor_dpi_v2, query_qpc_100ns,
    },
};

struct WgcSlotState {
    slots: [ID3D11Texture2D; 3],
    ring_gen: u64,
    tex_size: (u32, u32),
    content_size: (u32, u32),
    slot_format: DXGI_FORMAT,
    latest_idx: usize,
    reading_idx: Option<usize>,
    has_desktop: bool,
    frame_seq: u64,
    capture_stamp: i64,
    present_100ns: i64,
    arrived_100ns: i64,
    error: Option<anyhow::Error>,
}

pub(super) struct WgcSlotLease {
    state: Arc<Mutex<WgcSlotState>>,
    ring_gen: u64,
}

impl Drop for WgcSlotLease {
    fn drop(&mut self) {
        let mut guard = self.state.lock().unwrap();
        if guard.ring_gen == self.ring_gen {
            guard.reading_idx = None;
        }
    }
}

fn create_staging_ring(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<[ID3D11Texture2D; 3]> {
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
        ..Default::default()
    };
    let make_one = || -> Result<ID3D11Texture2D> {
        let mut tex = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }?;
        tex.context("Could not allocate WGC staging texture.")
    };
    Ok([make_one()?, make_one()?, make_one()?])
}

pub(super) struct WgcCapture {
    stop: Arc<AtomicBool>,
    stop_event: Arc<OwnedHandle>,
    state: Arc<Mutex<WgcSlotState>>,
    worker: Option<JoinHandle<()>>,
    last_seq: u64,
}

impl WgcCapture {
    pub(super) fn start(
        monitor: HMONITOR,
        gpu: &VideoDevice,
        wake: &Arc<CaptureWake>,
        hdr: bool,
    ) -> Result<Self> {
        let initial_format = if hdr {
            DXGI_FORMAT_R16G16B16A16_FLOAT
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        };
        let slots = create_staging_ring(&gpu.device, 2, 2, initial_format)?;
        let state = Arc::new(Mutex::new(WgcSlotState {
            slots,
            ring_gen: 0,
            tex_size: (2, 2),
            content_size: (2, 2),
            slot_format: initial_format,
            latest_idx: 0,
            reading_idx: None,
            has_desktop: false,
            frame_seq: 0,
            capture_stamp: 0,
            present_100ns: 0,
            arrived_100ns: 0,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let stop_event = Arc::new(OwnedHandle::new_event()?);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let monitor_raw = monitor.0 as usize;
        let encode_device = gpu.device.clone();
        let encode_context = gpu.context.clone();
        let worker_state = state.clone();
        let worker_stop = stop.clone();
        let worker_stop_event = stop_event.clone();
        let worker_wake = wake.clone();
        let worker = thread::Builder::new()
            .name("wgc-capture".into())
            .spawn(move || {
                wgc_worker(
                    monitor_raw,
                    encode_device,
                    encode_context,
                    hdr,
                    worker_state,
                    worker_stop,
                    worker_stop_event,
                    worker_wake,
                    ready_tx,
                );
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("WGC capture thread exited during initialization."))??;
        Ok(Self {
            stop,
            stop_event,
            state,
            worker: Some(worker),
            last_seq: 0,
        })
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
        let ring_gen = state.ring_gen;
        let texture = state.slots[idx].clone();
        let (w, h) = state.content_size;
        let capture_stamp = state.capture_stamp;
        let present_100ns = state.present_100ns;
        let arrived_100ns = state.arrived_100ns;
        self.last_seq = state.frame_seq;
        drop(state);
        Ok(Some(AcquiredInput {
            texture,
            source_width: w as i32,
            source_height: h as i32,
            capture_stamp,
            present_100ns,
            arrived_100ns,
            _lease: InputLease::Wgc(WgcSlotLease {
                state: self.state.clone(),
                ring_gen,
            }),
        }))
    }
}

impl Drop for WgcCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.stop_event.signal();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn wgc_worker(
    monitor_raw: usize,
    encode_device: ID3D11Device,
    encode_context: ID3D11DeviceContext,
    hdr: bool,
    state: Arc<Mutex<WgcSlotState>>,
    stop: Arc<AtomicBool>,
    stop_event: Arc<OwnedHandle>,
    wake: Arc<CaptureWake>,
    ready_tx: std::sync::mpsc::SyncSender<Result<()>>,
) {
    enable_per_monitor_dpi_v2();
    let _com = ComThreadGuard::new();
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };

    let setup = (|| -> Result<(
        GraphicsCaptureSession,
        Direct3D11CaptureFramePool,
        i64,
        IDirect3DDevice,
        DirectXPixelFormat,
        SizeInt32,
        Arc<OwnedHandle>,
    )> {
        let monitor = HMONITOR(monitor_raw as *mut std::ffi::c_void);
        let interop: IGraphicsCaptureItemInterop = factory::<GraphicsCaptureItem, _>()
            .context("Could not get GraphicsCaptureItem interop factory.")?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }
            .context("Could not create GraphicsCaptureItem for monitor.")?;

        let dxgi: IDXGIDevice = encode_device.cast()?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .context("Could not wrap D3D11 device for Windows Graphics Capture.")?;
        let capture_device: IDirect3DDevice = inspectable.cast()?;
        let capture_size = item.Size()?;
        let (pool, pixel_format) = if hdr {
            match Direct3D11CaptureFramePool::CreateFreeThreaded(
                &capture_device,
                DirectXPixelFormat::R16G16B16A16Float,
                2,
                capture_size,
            ) {
                Ok(pool) => (pool, DirectXPixelFormat::R16G16B16A16Float),
                Err(err) => {
                    eprintln!(
                        "[remoters:win] WGC R16G16B16A16Float rejected ({err:#}); falling back to B8G8R8A8UIntNormalized"
                    );
                    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                        &capture_device,
                        DirectXPixelFormat::B8G8R8A8UIntNormalized,
                        2,
                        capture_size,
                    )
                    .context("Could not create WGC frame pool.")?;
                    (pool, DirectXPixelFormat::B8G8R8A8UIntNormalized)
                }
            }
        } else {
            let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                &capture_device,
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                2,
                capture_size,
            )
            .context("Could not create WGC frame pool.")?;
            (pool, DirectXPixelFormat::B8G8R8A8UIntNormalized)
        };

        let arrived = Arc::new(OwnedHandle::new_event()?);
        let arrived_for_cb = arrived.clone();
        let token = pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new(move |_, _| {
            arrived_for_cb.signal();
            Ok(())
        }))?;
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        session.StartCapture()?;
        Ok((
            session,
            pool,
            token,
            capture_device,
            pixel_format,
            capture_size,
            arrived,
        ))
    })();

    let (session, pool, token, capture_device, pixel_format, mut capture_size, arrived) =
        match setup {
            Ok(parts) => {
                let _ = ready_tx.send(Ok(()));
                parts
            }
            Err(err) => {
                let _ = ready_tx.send(Err(err));
                return;
            }
        };

    let mut last_capture = 0i64;
    let mut has_desktop_yet = false;

    while !stop.load(Ordering::Acquire) {
        let step = (|| -> Result<bool> {
            let mut newest: Option<Direct3D11CaptureFrame> = None;
            let mut arrived_100ns = 0i64;
            while let Ok(next) = pool.TryGetNextFrame() {
                let size = next.ContentSize()?;
                if size.Width != capture_size.Width || size.Height != capture_size.Height {
                    let _ = next.Close();
                    if let Some(old) = newest.take() {
                        let _ = old.Close();
                    }
                    capture_size = size;
                    pool.Recreate(&capture_device, pixel_format, 2, capture_size)?;
                    return Ok(false);
                }
                arrived_100ns = query_qpc_100ns();
                if let Some(old) = newest.replace(next) {
                    let _ = old.Close();
                }
            }
            let Some(frame) = newest else {
                return Ok(false);
            };
            let captured = frame.SystemRelativeTime()?.Duration;
            if has_desktop_yet && captured <= last_capture {
                let _ = frame.Close();
                return Ok(false);
            }
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let src_tex: ID3D11Texture2D = unsafe { access.GetInterface() }?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { src_tex.GetDesc(&mut desc) };
            let content_w = u32::try_from(capture_size.Width.max(1))
                .unwrap_or(desc.Width)
                .min(desc.Width);
            let content_h = u32::try_from(capture_size.Height.max(1))
                .unwrap_or(desc.Height)
                .min(desc.Height);

            let (write_idx, target_tex, ring_gen) = {
                let mut guard = state.lock().unwrap();
                if guard.tex_size != (desc.Width, desc.Height) || guard.slot_format != desc.Format {
                    guard.slots =
                        create_staging_ring(&encode_device, desc.Width, desc.Height, desc.Format)?;
                    guard.reading_idx = None;
                    guard.ring_gen = guard.ring_gen.wrapping_add(1);
                    guard.tex_size = (desc.Width, desc.Height);
                    guard.slot_format = desc.Format;
                }
                let idx = (0..3)
                    .find(|&i| i != guard.latest_idx && Some(i) != guard.reading_idx)
                    .unwrap_or(0);
                (idx, guard.slots[idx].clone(), guard.ring_gen)
            };

            unsafe {
                encode_context.CopyResource(&target_tex, &src_tex);
                encode_context.Flush();
            }
            let _ = frame.Close();

            last_capture = captured;
            has_desktop_yet = true;
            let mut guard = state.lock().unwrap();
            if guard.ring_gen == ring_gen {
                guard.latest_idx = write_idx;
                guard.content_size = (content_w, content_h);
            }
            guard.has_desktop = true;
            guard.capture_stamp = captured;
            guard.present_100ns = captured;
            guard.arrived_100ns = arrived_100ns;
            guard.frame_seq = guard.frame_seq.wrapping_add(1);
            Ok(true)
        })();

        match step {
            Ok(true) => wake.signal(),
            Ok(false) => {
                let handles = [arrived.0, stop_event.0];
                unsafe {
                    let _ = WaitForMultipleObjects(&handles, false, 100);
                }
            }
            Err(err) => {
                state.lock().unwrap().error = Some(err);
                wake.signal();
                break;
            }
        }
    }

    let _ = pool.RemoveFrameArrived(token);
    let _ = session.Close();
    let _ = pool.Close();
}
