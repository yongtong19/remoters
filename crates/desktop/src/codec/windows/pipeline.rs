//! Windows desktop capture & hardware encode pipeline orchestrating DDA/WGC
//! latest-frame handoff, monotonic PTS clamping, static-screen keepalive frames,
//! and direct NVENC (with transparent Media Foundation MFT fallback).

use std::{
    mem::ManuallyDrop,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, ensure};
use gpui::ID3D11Texture2D;
use remoters_core::video::{AccessUnit, Codec, Format, MAX_ACCESS_UNIT, nal_units, obu};
use tokio::sync::mpsc;
use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
        Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
        Dxgi::{
            Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12},
            IDXGIAdapter, IDXGIDevice,
        },
        Gdi::HMONITOR,
    },
    Media::MediaFoundation::{
        CODECAPI_AVEncVideoForceKeyFrame, IMF2DBuffer, IMFMediaBuffer, METransformHaveOutput,
        METransformNeedInput, MF_MT_MPEG_SEQUENCE_HEADER, MFCreateAlignedMemoryBuffer,
        MFCreateDXGISurfaceBuffer, MFCreateSample, MFSampleExtension_CleanPoint,
        MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_INFO,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    },
    System::{
        Com::CoTaskMemFree,
        Threading::{
            CancelWaitableTimer, GetCurrentThread, SetThreadPriority, SetWaitableTimer,
            THREAD_PRIORITY_HIGHEST, WaitForMultipleObjects, WaitForSingleObject,
        },
    },
};
use windows_core::Interface;

use crate::codec::{
    BitrateControl, decode_bitrate_override, effective_bitrate, encode_bitrate_override,
};

use super::{
    capture::CaptureBackend,
    capture_dda::DdaCapture,
    capture_wgc::WgcCapture,
    common::{
        CaptureWake, DXGI_FORMAT_P010, DXGI_FORMAT_R16G16B16A16_FLOAT, OwnedHandle,
        TimerResolutionGuard, VideoDevice, enable_per_monitor_dpi_v2, find_monitor_output,
        query_qpc_100ns, remoters_nudge_monitor, remoters_nudge_pointer, resolve_monitor_handle,
    },
    d3d_convert::SurfaceConverter,
    mft_encoder::{MftBackend, apply_bitrate_settings, set_codec_u32},
    nvenc::NvencEncoder,
    nvenc_ffi::{WinEncoderPreference, env_encoder_preference},
};

/// Idle interval for static-screen keepalive P-frames (~5 fps) so encoder VBV,
/// GPU P-state, and QUIC link stay warm during prolonged desktop stillness.
pub(super) const STATIC_KEEPALIVE_INTERVAL: Duration = Duration::from_millis(200);

/// Advances the monotonic encoder timestamp (in 100ns units) while clamping
/// inter-frame jumps to at most `2 * frame_duration` so multi-second static
/// gaps never trigger an MFT/NVENC rate-control burst.
pub(super) fn next_clamped_sample_time(
    capture_stamp: i64,
    last_capture_stamp: Option<i64>,
    last_time: i64,
    frame_duration: i64,
) -> i64 {
    if last_time < 0 {
        return 0;
    }
    let step = frame_duration.max(1_000);
    let Some(prev_stamp) = last_capture_stamp else {
        return last_time.saturating_add(step);
    };
    let raw_delta = capture_stamp.saturating_sub(prev_stamp);
    let min_step = (step / 4).max(1_000);
    let max_step = step.saturating_mul(2);
    let clamped_delta = if raw_delta <= 0 {
        step
    } else {
        raw_delta.clamp(min_step, max_step)
    };
    last_time.saturating_add(clamped_delta)
}

#[derive(Clone)]
pub struct KeyframeTrigger {
    request: Arc<AtomicBool>,
    wake: Arc<CaptureWake>,
}

impl KeyframeTrigger {
    pub fn request(&self) {
        self.request.store(true, Ordering::Release);
        self.wake.signal();
    }
}

pub struct Encoder {
    request: Arc<AtomicBool>,
    wake: Arc<CaptureWake>,
    bitrate_override: Arc<AtomicU32>,
    pub units: mpsc::Receiver<Result<AccessUnit>>,
}

impl Encoder {
    #[allow(dead_code)]
    pub fn start(format: Format, monitor: &xcap::Monitor) -> Result<Self> {
        Self::start_with_bitrate(format, monitor, None)
    }

    pub fn start_with_bitrate(
        format: Format,
        monitor: &xcap::Monitor,
        bitrate_mbps: Option<u16>,
    ) -> Result<Self> {
        ensure!(
            matches!(format.codec, Codec::H264 | Codec::Hevc | Codec::Av1),
            "The Windows hardware encoder requires H.264, HEVC, or AV1."
        );
        let monitor_id = monitor.id().ok();
        let center = (
            monitor.x()? + i32::try_from(monitor.width()? / 2)?,
            monitor.y()? + i32::try_from(monitor.height()? / 2)?,
        );
        let request = Arc::new(AtomicBool::new(true));
        let wake = Arc::new(CaptureWake::new()?);
        let bitrate_override = Arc::new(AtomicU32::new(encode_bitrate_override(bitrate_mbps)));
        let worker_request = request.clone();
        let worker_wake = wake.clone();
        let worker_bitrate = bitrate_override.clone();
        let (output, units) = mpsc::channel(2);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        thread::Builder::new()
            .name("windows-screen-encoder".into())
            .spawn(move || {
                let mut ready = Some(ready_tx);
                if let Err(error) = capture(
                    format,
                    monitor_id,
                    center,
                    &worker_request,
                    &worker_wake,
                    bitrate_mbps,
                    &worker_bitrate,
                    &output,
                    &mut ready,
                ) {
                    if let Some(ready_tx) = ready.take() {
                        let _ = ready_tx.send(Err(error));
                    } else {
                        let _ = output.blocking_send(Err(error));
                    }
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow!("Windows encoder initialization stopped."))??;
        Ok(Self {
            request,
            wake,
            bitrate_override,
            units,
        })
    }

    pub fn keyframe_trigger(&self) -> KeyframeTrigger {
        KeyframeTrigger {
            request: self.request.clone(),
            wake: self.wake.clone(),
        }
    }

    #[allow(dead_code)]
    pub fn bitrate_override(&self) -> Arc<AtomicU32> {
        self.bitrate_override.clone()
    }

    pub fn bitrate_control(&self) -> BitrateControl {
        BitrateControl::from_raw(self.bitrate_override.clone())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.units.close();
        self.wake.signal();
    }
}

#[derive(Default)]
struct StageStats {
    cap_age_sum_ms: f64,
    queue_sum_ms: f64,
    conv_sum_ms: f64,
    enc_sum_ms: f64,
    total_sum_ms: f64,
    total_max_ms: f64,
    count: u32,
}

struct PreparedFrame {
    surface_idx: usize,
    time: i64,
    cap_age_ms: f64,
    queue_ms: f64,
    conv_ms: f64,
    present_100ns: i64,
    is_keepalive: bool,
}

enum EncoderBackend {
    Nvenc(NvencEncoder),
    Mft(MftBackend),
}

impl EncoderBackend {
    fn name(&self) -> &'static str {
        match self {
            Self::Nvenc(_) => "nvenc",
            Self::Mft(_) => "mft",
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn create_encoder_backend(
    gpu: &VideoDevice,
    codec_kind: Codec,
    hdr: bool,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    wake: &Arc<CaptureWake>,
    log_enabled: bool,
) -> Result<(EncoderBackend, bool)> {
    match env_encoder_preference() {
        WinEncoderPreference::Mft => {
            let (mft, use_p010) = MftBackend::new(
                gpu,
                codec_kind,
                hdr,
                width,
                height,
                fps,
                bitrate,
                wake,
                log_enabled,
            )?;
            Ok((EncoderBackend::Mft(mft), use_p010))
        }
        WinEncoderPreference::Nvenc => {
            let nv = NvencEncoder::new(
                &gpu.device,
                codec_kind,
                hdr,
                width,
                height,
                fps,
                bitrate,
                log_enabled,
            )?;
            let use_p010 = nv.uses_p010();
            Ok((EncoderBackend::Nvenc(nv), use_p010))
        }
        WinEncoderPreference::Auto => {
            match NvencEncoder::new(
                &gpu.device,
                codec_kind,
                hdr,
                width,
                height,
                fps,
                bitrate,
                log_enabled,
            ) {
                Ok(nv) => {
                    let use_p010 = nv.uses_p010();
                    Ok((EncoderBackend::Nvenc(nv), use_p010))
                }
                Err(err) => {
                    if log_enabled {
                        eprintln!(
                            "[remoters:win] direct nvenc unavailable ({err:#}); falling back to Media Foundation MFT"
                        );
                    }
                    let (mft, use_p010) = MftBackend::new(
                        gpu,
                        codec_kind,
                        hdr,
                        width,
                        height,
                        fps,
                        bitrate,
                        wake,
                        log_enabled,
                    )?;
                    Ok((EncoderBackend::Mft(mft), use_p010))
                }
            }
        }
    }
}

struct NativeEncoder {
    // Field order matters: capture/backend/surface objects drop BEFORE `gpu` (which runs MFShutdown/CoUninitialize).
    capture: CaptureBackend,
    backend: EncoderBackend,
    converter: SurfaceConverter,
    gpu: VideoDevice,
    wake: Arc<CaptureWake>,
    pace_timer: Option<OwnedHandle>,
    _timer_resolution: TimerResolutionGuard,
    format: Format,
    use_p010: bool,
    bitrate_override: Arc<AtomicU32>,
    active_bitrate: u32,
    duration: i64,
    last_capture_stamp: Option<i64>,
    last_time: i64,
    submit_at: Instant,
    last_submit: Instant,
    pending_cap_age_ms: f64,
    pending_queue_ms: f64,
    pending_conv_ms: f64,
    pending_present_100ns: i64,
    pending_is_keepalive: bool,
    stats: StageStats,
    log_enabled: bool,
    keyframe: bool,
    has_encoded: bool,
}

impl Drop for NativeEncoder {
    fn drop(&mut self) {
        if let EncoderBackend::Nvenc(nv) = &mut self.backend {
            nv.clear_registered_surfaces();
        }
        self.converter.clear();
    }
}

#[allow(clippy::too_many_arguments)]
fn try_init_dda(
    monitor: HMONITOR,
    codec_kind: Codec,
    hdr: bool,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    wake: &Arc<CaptureWake>,
    log_enabled: bool,
) -> Result<(VideoDevice, EncoderBackend, bool, DdaCapture)> {
    let (adapter, output1) = find_monitor_output(monitor)?;
    let gpu = VideoDevice::new_with_adapter(None, Some(&adapter))?;

    // Use a dedicated D3D11 device for Desktop Duplication so AcquireNextFrame's
    // internal device lock never contends with VideoProcessorBlt or NVENC/MFT.
    let dxgi_adapter: IDXGIAdapter = adapter.cast()?;
    let mut cap_device = None;
    let mut cap_context = None;
    unsafe {
        D3D11CreateDevice(
            Some(&dxgi_adapter),
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut cap_device),
            None,
            Some(&mut cap_context),
        )
    }
    .context("Could not create dedicated D3D11 device for Desktop Duplication.")?;
    let cap_device = cap_device.context("Missing capture D3D11 device.")?;
    let cap_context = cap_context.context("Missing capture D3D11 context.")?;
    if let Ok(dxgi) = cap_device.cast::<IDXGIDevice>() {
        let _ = unsafe { dxgi.SetGPUThreadPriority(7) };
    }

    let duplication = unsafe { output1.DuplicateOutput(&cap_device) }
        .context("Could not initialize DXGI Desktop Duplication.")?;
    let desc = unsafe { output1.GetDesc() }?;
    let dw = u32::try_from(desc.DesktopCoordinates.right - desc.DesktopCoordinates.left)?;
    let dh = u32::try_from(desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top)?;
    if log_enabled {
        let adapter_name = unsafe { adapter.GetDesc1() }
            .map(|d| {
                let end = d
                    .Description
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(d.Description.len());
                String::from_utf16_lossy(&d.Description[..end])
            })
            .unwrap_or_default();
        eprintln!(
            "[remoters:win] dda_adapter={adapter_name} dda_output=({}, {}, {}, {}) size={}x{}",
            desc.DesktopCoordinates.left,
            desc.DesktopCoordinates.top,
            desc.DesktopCoordinates.right,
            desc.DesktopCoordinates.bottom,
            dw,
            dh
        );
    }
    ensure!(dw > 0 && dh > 0, "Invalid DXGI output dimensions.");
    if (dw != width || dh != height) && log_enabled {
        eprintln!(
            "[remoters:win] capture_size={dw}x{dh} differs from encode_size={width}x{height}; scaling full desktop via VideoProcessor"
        );
    }
    let initial_format = if hdr {
        DXGI_FORMAT_R16G16B16A16_FLOAT
    } else {
        DXGI_FORMAT_B8G8R8A8_UNORM
    };
    let dda = DdaCapture::start(
        cap_device,
        cap_context,
        gpu.device.clone(),
        output1,
        duplication,
        (dw, dh),
        initial_format,
        wake.clone(),
    )?;
    let nudge_pos = (desc.DesktopCoordinates.left, desc.DesktopCoordinates.top);
    let center_pos = (
        desc.DesktopCoordinates.left.saturating_add(dw as i32 / 2),
        desc.DesktopCoordinates.top.saturating_add(dh as i32 / 2),
    );
    unsafe { remoters_nudge_monitor(nudge_pos.0, nudge_pos.1) };
    let (backend, use_p010) = create_encoder_backend(
        &gpu,
        codec_kind,
        hdr,
        width,
        height,
        fps,
        bitrate,
        wake,
        log_enabled,
    )?;
    if !dda.wait_initial_frame(wake, Duration::from_millis(120))? {
        // Re-nudge window and briefly touch the target monitor pointer if an idle or
        // newly attached virtual monitor still has not presented an initial DXGI frame.
        unsafe {
            remoters_nudge_monitor(nudge_pos.0, nudge_pos.1);
            remoters_nudge_pointer(center_pos.0, center_pos.1);
        }
        let _ = dda.wait_initial_frame(wake, Duration::from_millis(250))?;
    }
    Ok((gpu, backend, use_p010, dda))
}

impl NativeEncoder {
    fn new(
        format: Format,
        bitrate: u32,
        bitrate_override: Arc<AtomicU32>,
        monitor_id: Option<u32>,
        center: (i32, i32),
        wake: Arc<CaptureWake>,
    ) -> Result<Self> {
        let width = u32::from(format.width);
        let height = u32::from(format.height);
        let fps = u32::from(format.frames_per_second);
        let timer_resolution = TimerResolutionGuard::new();
        let pace_timer = OwnedHandle::new_high_res_timer();
        let log_enabled = crate::metrics::enabled();

        let monitor = resolve_monitor_handle(monitor_id, center)?;

        let force_wgc = std::env::var("REMOTERS_WIN_CAPTURE")
            .ok()
            .is_some_and(|v| v.eq_ignore_ascii_case("wgc"));

        let (gpu, backend, use_p010, capture) = if !force_wgc {
            match try_init_dda(
                monitor,
                format.codec,
                format.hdr,
                width,
                height,
                fps,
                bitrate,
                &wake,
                log_enabled,
            ) {
                Ok((gpu, backend, use_p010, dda)) => {
                    (gpu, backend, use_p010, CaptureBackend::Dda(dda))
                }
                Err(err) => {
                    if log_enabled {
                        eprintln!("[remoters:win] dda_unavailable={err:#}; falling back to wgc");
                    }
                    let gpu = VideoDevice::new(None)?;
                    let (backend, use_p010) = create_encoder_backend(
                        &gpu,
                        format.codec,
                        format.hdr,
                        width,
                        height,
                        fps,
                        bitrate,
                        &wake,
                        log_enabled,
                    )?;
                    let wgc = WgcCapture::start(monitor, &gpu, &wake, format.hdr)?;
                    (gpu, backend, use_p010, CaptureBackend::Wgc(wgc))
                }
            }
        } else {
            let gpu = VideoDevice::new(None)?;
            let (backend, use_p010) = create_encoder_backend(
                &gpu,
                format.codec,
                format.hdr,
                width,
                height,
                fps,
                bitrate,
                &wake,
                log_enabled,
            )?;
            let wgc = WgcCapture::start(monitor, &gpu, &wake, format.hdr)?;
            (gpu, backend, use_p010, CaptureBackend::Wgc(wgc))
        };

        if log_enabled {
            eprintln!(
                "[remoters:win] capture_backend={} encoder_backend={} codec={:?} hdr_requested={} p010={}",
                capture.name(),
                backend.name(),
                format.codec,
                format.hdr,
                use_p010
            );
        }

        let now = Instant::now();
        Ok(Self {
            capture,
            backend,
            converter: SurfaceConverter::new(width, height, use_p010),
            gpu,
            wake,
            pace_timer,
            _timer_resolution: timer_resolution,
            format,
            use_p010,
            bitrate_override,
            active_bitrate: bitrate,
            duration: 10_000_000 / i64::from(fps.max(1)),
            last_capture_stamp: None,
            last_time: -1,
            submit_at: now,
            last_submit: now,
            pending_cap_age_ms: 0.0,
            pending_queue_ms: 0.0,
            pending_conv_ms: 0.0,
            pending_present_100ns: 0,
            pending_is_keepalive: false,
            stats: StageStats::default(),
            log_enabled,
            keyframe: true,
            has_encoded: false,
        })
    }

    fn record_frame_stats(&mut self, enc_ms: f64, total_ms: f64) {
        if !self.log_enabled {
            return;
        }
        self.stats.cap_age_sum_ms += self.pending_cap_age_ms;
        self.stats.queue_sum_ms += self.pending_queue_ms;
        self.stats.conv_sum_ms += self.pending_conv_ms;
        self.stats.enc_sum_ms += enc_ms;
        self.stats.total_sum_ms += total_ms;
        self.stats.total_max_ms = self.stats.total_max_ms.max(total_ms);
        self.stats.count += 1;
        if self.stats.count >= 60 {
            let n = f64::from(self.stats.count);
            eprintln!(
                "[remoters:win] backend={}/{} cap_age={:.2}ms queue={:.2}ms conv={:.2}ms enc={:.2}ms total={:.2}ms max={:.2}ms (n={})",
                self.capture.name(),
                self.backend.name(),
                self.stats.cap_age_sum_ms / n,
                self.stats.queue_sum_ms / n,
                self.stats.conv_sum_ms / n,
                self.stats.enc_sum_ms / n,
                self.stats.total_sum_ms / n,
                self.stats.total_max_ms,
                self.stats.count,
            );
            self.stats = StageStats::default();
        }
    }

    fn prepare_frame(
        &mut self,
        pace_remaining: &mut Option<Duration>,
    ) -> Result<Option<PreparedFrame>> {
        *pace_remaining = None;
        if let Some(acquired) = self.capture.acquire()? {
            let submit_start_100ns = query_qpc_100ns();
            let cap_age_ms =
                ((acquired.arrived_100ns - acquired.present_100ns) as f64 / 10_000.0).max(0.0);
            let queue_ms =
                ((submit_start_100ns - acquired.arrived_100ns) as f64 / 10_000.0).max(0.0);
            let time = next_clamped_sample_time(
                acquired.capture_stamp,
                self.last_capture_stamp,
                self.last_time,
                self.duration,
            );
            let conv_start = Instant::now();
            let backend = &mut self.backend;
            let idx = self.converter.convert(
                &self.gpu,
                &acquired.texture,
                acquired.source_width,
                acquired.source_height,
                || {
                    if let EncoderBackend::Nvenc(nv) = backend {
                        nv.clear_registered_surfaces();
                    }
                },
            )?;
            let conv_ms = conv_start.elapsed().as_secs_f64() * 1000.0;
            let present_100ns = acquired.present_100ns;
            let capture_stamp = acquired.capture_stamp;
            drop(acquired);
            self.converter.last_surface_idx = Some(idx);
            self.last_capture_stamp = Some(capture_stamp);
            return Ok(Some(PreparedFrame {
                surface_idx: idx,
                time,
                cap_age_ms,
                queue_ms,
                conv_ms,
                present_100ns,
                is_keepalive: false,
            }));
        }

        if self.has_encoded
            && let Some(idx) = self.converter.last_surface_idx
        {
            let required = if self.keyframe {
                Duration::from_nanos(self.duration.max(1) as u64 * 100)
            } else {
                STATIC_KEEPALIVE_INTERVAL
            };
            let elapsed = self.last_submit.elapsed();
            if elapsed < required {
                *pace_remaining = Some(required - elapsed);
                return Ok(None);
            }
            let time = self.last_time.saturating_add(self.duration);
            let now_100ns = query_qpc_100ns();
            self.last_capture_stamp = Some(now_100ns);
            return Ok(Some(PreparedFrame {
                surface_idx: idx,
                time,
                cap_age_ms: 0.0,
                queue_ms: 0.0,
                conv_ms: 0.0,
                present_100ns: now_100ns,
                is_keepalive: !self.keyframe,
            }));
        }

        Ok(None)
    }

    fn poll(&mut self, force: bool, wait_ms: u32, state: &mut EncodedOutput<'_>) -> Result<()> {
        self.keyframe |= force;
        match self.backend {
            EncoderBackend::Nvenc(_) => self.poll_nvenc(wait_ms, state),
            EncoderBackend::Mft(_) => self.poll_mft(wait_ms, state),
        }
    }

    fn encode_nvenc_frame(
        &mut self,
        frame: PreparedFrame,
        out: &mut EncodedOutput<'_>,
    ) -> Result<()> {
        let target_bitrate = effective_bitrate(
            self.format,
            decode_bitrate_override(self.bitrate_override.load(Ordering::Acquire)),
        );
        let tex = self.converter.texture(frame.surface_idx);
        let dxgi_fmt = if self.use_p010 {
            DXGI_FORMAT_P010.0
        } else {
            DXGI_FORMAT_NV12.0
        };
        let force_keyframe = self.keyframe;
        let duration = self.duration;
        self.pending_cap_age_ms = frame.cap_age_ms;
        self.pending_queue_ms = frame.queue_ms;
        self.pending_conv_ms = frame.conv_ms;
        self.pending_present_100ns = frame.present_100ns;
        self.pending_is_keepalive = frame.is_keepalive;

        let submit_at = Instant::now();
        let mut emitted = false;
        let mut encode_elapsed = Duration::ZERO;
        {
            let EncoderBackend::Nvenc(nv) = &mut self.backend else {
                unreachable!()
            };
            if target_bitrate != self.active_bitrate {
                if let Err(err) = nv.set_bitrate(target_bitrate)
                    && self.log_enabled
                {
                    eprintln!("[remoters:win] nvenc set_bitrate failed: {err:#}");
                }
                self.active_bitrate = target_bitrate;
            }
            nv.encode_texture(
                &tex,
                dxgi_fmt,
                frame.time,
                duration,
                force_keyframe,
                |slice, out_time| {
                    encode_elapsed = submit_at.elapsed();
                    let encode_us = encode_elapsed.as_micros().min(u128::from(u32::MAX)) as u32;
                    if !slice.is_empty() {
                        out.emit(slice, &[], out_time, encode_us);
                        emitted = true;
                    }
                },
            )?;
        }

        self.keyframe = false;
        self.has_encoded = true;
        self.last_time = frame.time;
        self.submit_at = submit_at;
        self.last_submit = submit_at;
        if emitted && !self.pending_is_keepalive {
            let enc_ms = encode_elapsed.as_secs_f64() * 1000.0;
            let total_ms =
                ((query_qpc_100ns() - self.pending_present_100ns) as f64 / 10_000.0).max(0.0);
            self.record_frame_stats(enc_ms, total_ms);
        }
        Ok(())
    }

    fn poll_nvenc(&mut self, wait_ms: u32, out: &mut EncodedOutput<'_>) -> Result<()> {
        let wake_seen = self.wake.snapshot();
        let mut pace_remaining = None;
        if let Some(frame) = self.prepare_frame(&mut pace_remaining)? {
            return self.encode_nvenc_frame(frame, out);
        }

        if wait_ms > 0 {
            let mut wait_dur = Duration::from_millis(u64::from(wait_ms));
            if let Some(rem) = pace_remaining {
                wait_dur = wait_dur.min(rem);
            }
            self.wake.wait_timeout(wake_seen, wait_dur);
            let mut ignored = None;
            if let Some(frame) = self.prepare_frame(&mut ignored)? {
                return self.encode_nvenc_frame(frame, out);
            }
        }
        Ok(())
    }

    fn poll_mft(&mut self, wait_ms: u32, state: &mut EncodedOutput<'_>) -> Result<()> {
        let mut produced = false;
        self.update_mft_events(state, &mut produced)?;
        let mut pace_remaining = None;
        self.try_submit_mft(&mut pace_remaining)?;
        if produced {
            return Ok(());
        }

        let has_pending = match &self.backend {
            EncoderBackend::Mft(mft) => mft.has_pending_events(),
            EncoderBackend::Nvenc(_) => false,
        };
        if wait_ms > 0 && !has_pending {
            let wake_event = self.wake.raw_event();
            if let (Some(rem), Some(timer)) = (pace_remaining, &self.pace_timer) {
                let remaining_100ns = (rem.as_nanos() / 100).max(1) as i64;
                let due = -remaining_100ns;
                unsafe {
                    let _ = SetWaitableTimer(timer.0, &due, 0, None, None, false);
                    let handles = [wake_event, timer.0];
                    let _ = WaitForMultipleObjects(&handles, false, wait_ms);
                    let _ = CancelWaitableTimer(timer.0);
                }
            } else {
                unsafe {
                    let _ = WaitForSingleObject(wake_event, wait_ms);
                }
            }
            self.update_mft_events(state, &mut produced)?;
            let mut ignored = None;
            self.try_submit_mft(&mut ignored)?;
        }
        Ok(())
    }

    fn update_mft_events(
        &mut self,
        out: &mut EncodedOutput<'_>,
        produced: &mut bool,
    ) -> Result<()> {
        let (batch, async_error) = {
            let EncoderBackend::Mft(mft) = &mut self.backend else {
                return Ok(());
            };
            let mut state = mft.event_queue.state.lock().unwrap();
            (std::mem::take(&mut state.pending), state.error)
        };
        if let Some(hr) = async_error {
            return Err(windows_core::Error::from(hr).into());
        }
        for event in batch {
            let status = unsafe { event.GetStatus() }?;
            status.ok()?;
            let kind = unsafe { event.GetType() }?;
            if kind == METransformNeedInput.0 as u32
                && let EncoderBackend::Mft(mft) = &mut self.backend
            {
                mft.wants_input = true;
            }
            if kind == METransformHaveOutput.0 as u32 {
                self.receive_mft(out)?;
                if let EncoderBackend::Mft(mft) = &mut self.backend {
                    mft.waiting_for_output = false;
                }
                *produced = true;
            }
        }
        Ok(())
    }

    fn try_submit_mft(&mut self, pace_remaining: &mut Option<Duration>) -> Result<()> {
        *pace_remaining = None;
        {
            let EncoderBackend::Mft(mft) = &self.backend else {
                return Ok(());
            };
            if !mft.wants_input || mft.waiting_for_output {
                return Ok(());
            }
        }

        let Some(frame) = self.prepare_frame(pace_remaining)? else {
            return Ok(());
        };
        let surface = self.converter.texture(frame.surface_idx);
        let target_bitrate = effective_bitrate(
            self.format,
            decode_bitrate_override(self.bitrate_override.load(Ordering::Acquire)),
        );
        let fps = u32::from(self.format.frames_per_second);
        let force_keyframe = self.keyframe;
        let duration = self.duration;

        let buffer =
            unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &surface, 0, false) }?;
        let texture_buffer: IMF2DBuffer = buffer.cast()?;
        let length = unsafe { texture_buffer.GetContiguousLength() }?;
        unsafe { buffer.SetCurrentLength(length) }?;
        let sample = unsafe { MFCreateSample() }?;
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(frame.time)?;
            sample.SetSampleDuration(duration)?;
        }

        let submit_at = Instant::now();
        {
            let EncoderBackend::Mft(mft) = &mut self.backend else {
                unreachable!()
            };
            if target_bitrate != self.active_bitrate {
                apply_bitrate_settings(&mft.codec, target_bitrate, fps);
                self.active_bitrate = target_bitrate;
            }
            if force_keyframe {
                set_codec_u32(&mft.codec, &CODECAPI_AVEncVideoForceKeyFrame, 1)?;
            }
            unsafe { mft.transform.ProcessInput(mft.input_id, &sample, 0) }?;
            mft.wants_input = false;
            mft.waiting_for_output = true;
        }

        self.pending_cap_age_ms = frame.cap_age_ms;
        self.pending_queue_ms = frame.queue_ms;
        self.pending_conv_ms = frame.conv_ms;
        self.pending_present_100ns = frame.present_100ns;
        self.pending_is_keepalive = frame.is_keepalive;
        self.submit_at = submit_at;
        self.keyframe = false;
        self.has_encoded = true;
        self.last_time = frame.time;
        self.last_submit = submit_at;
        Ok(())
    }

    fn receive_mft(&mut self, out: &mut EncodedOutput<'_>) -> Result<()> {
        let (transform, output_id) = match &self.backend {
            EncoderBackend::Mft(mft) => (mft.transform.clone(), mft.output_id),
            EncoderBackend::Nvenc(_) => return Ok(()),
        };
        let info: MFT_OUTPUT_STREAM_INFO = unsafe { transform.GetOutputStreamInfo(output_id) }?;
        let provides =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        let mut output = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: output_id,
            ..Default::default()
        }];
        if (info.dwFlags & provides) == 0 {
            let align = info.cbAlignment.saturating_sub(1);
            let buffer = unsafe { MFCreateAlignedMemoryBuffer(info.cbSize, align) }?;
            let storage = unsafe { MFCreateSample() }?;
            unsafe { storage.AddBuffer(&buffer) }?;
            output[0].pSample = ManuallyDrop::new(Some(storage));
        }
        let mut flags = 0u32;
        let result = unsafe { transform.ProcessOutput(0, &mut output, &mut flags) };
        let storage = unsafe { ManuallyDrop::take(&mut output[0].pSample) };
        let _events = unsafe { ManuallyDrop::take(&mut output[0].pEvents) };
        result?;
        let Some(storage) = storage else {
            return Ok(());
        };

        let encode_elapsed = self.submit_at.elapsed();
        let encode_us = encode_elapsed.as_micros().min(u128::from(u32::MAX)) as u32;
        let time = unsafe { storage.GetSampleTime() }?;
        if self.log_enabled {
            if time != self.last_time {
                eprintln!(
                    "[remoters:win] MFT_REORDER_LAG out_time={time} last_in_time={} diff_ms={:.2}",
                    self.last_time,
                    (self.last_time - time) as f64 / 10_000.0
                );
            }
            if !self.pending_is_keepalive {
                let enc_ms = encode_elapsed.as_secs_f64() * 1000.0;
                let total_ms =
                    ((query_qpc_100ns() - self.pending_present_100ns) as f64 / 10_000.0).max(0.0);
                self.record_frame_stats(enc_ms, total_ms);
            }
        }

        let buffer: IMFMediaBuffer = unsafe { storage.ConvertToContiguousBuffer() }?;
        let key = unsafe { storage.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0);
        let mut header_bytes = Vec::new();
        if key != 0
            && let Ok(media) = unsafe { transform.GetOutputCurrentType(output_id) }
        {
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            if unsafe { media.GetAllocatedBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut ptr, &mut len) }
                .is_ok()
                && !ptr.is_null()
            {
                if (1..=4096).contains(&len) {
                    header_bytes.extend_from_slice(unsafe {
                        std::slice::from_raw_parts(ptr, len as usize)
                    });
                }
                unsafe { CoTaskMemFree(Some(ptr.cast())) };
            }
        }

        let mut bytes_ptr = std::ptr::null_mut();
        let mut length = 0u32;
        unsafe { buffer.Lock(&mut bytes_ptr, None, Some(&mut length)) }?;
        let slice = if !bytes_ptr.is_null() && length > 0 {
            unsafe { std::slice::from_raw_parts(bytes_ptr, length as usize) }
        } else {
            &[]
        };
        out.emit(slice, &header_bytes, time, encode_us);
        unsafe { buffer.Unlock() }?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn capture(
    format: Format,
    monitor_id: Option<u32>,
    center: (i32, i32),
    request: &AtomicBool,
    wake: &Arc<CaptureWake>,
    bitrate_mbps: Option<u16>,
    bitrate_override: &Arc<AtomicU32>,
    output: &mpsc::Sender<Result<AccessUnit>>,
    ready: &mut Option<std::sync::mpsc::SyncSender<Result<()>>>,
) -> Result<()> {
    enable_per_monitor_dpi_v2();
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    let bitrate = effective_bitrate(format, bitrate_mbps);
    let mut encoder = NativeEncoder::new(
        format,
        bitrate,
        bitrate_override.clone(),
        monitor_id,
        center,
        wake.clone(),
    )
    .context("Could not start the Windows hardware encoder.")?;
    if let Some(ready_tx) = ready.take() {
        let _ = ready_tx.send(Ok(()));
    }
    let started = Instant::now();
    let mut state = EncodedOutput {
        codec: format.codec,
        output,
        request,
        sequence: 0,
        synchronized: false,
        stats: crate::metrics::EncodeStats::new(),
        error: None,
    };
    while !output.is_closed() {
        encoder
            .poll(request.swap(false, Ordering::AcqRel), 50, &mut state)
            .context("Windows desktop capture or encoding failed.")?;
        if let Some(error) = state.error.take() {
            return Err(error);
        }
        ensure!(
            state.synchronized || started.elapsed() < Duration::from_secs(8) || state.sequence != 0,
            "The Windows encoder did not produce a desktop frame.",
        );
    }
    Ok(())
}

struct EncodedOutput<'a> {
    codec: Codec,
    output: &'a mpsc::Sender<Result<AccessUnit>>,
    request: &'a AtomicBool,
    sequence: u64,
    synchronized: bool,
    stats: crate::metrics::EncodeStats,
    error: Option<anyhow::Error>,
}

impl EncodedOutput<'_> {
    fn emit(&mut self, picture: &[u8], header: &[u8], time: i64, encode_us: u32) {
        if self.error.is_some() {
            return;
        }
        let unit = (|| -> Result<AccessUnit> {
            ensure!(
                !picture.is_empty() && picture.len() <= MAX_ACCESS_UNIT,
                "Invalid hardware encoder access unit size.",
            );
            let mut packet = Vec::new();
            if self.codec == Codec::Av1 {
                if obu::extract_sequence_header(picture).is_none()
                    && !header.is_empty()
                    && let Some(seq_obu) = obu::extract_sequence_header(header)
                {
                    packet.extend_from_slice(seq_obu);
                }
                packet.extend_from_slice(picture);
            } else {
                let mut parameters = 0;
                let mut keyframe = false;
                let (full_mask, param_nals) = match self.codec {
                    Codec::H264 => (3u8, 7u8..=8u8),
                    Codec::Hevc => (7u8, 32u8..=34u8),
                    Codec::Av1 => unreachable!(),
                };
                for nal in nal_units(picture) {
                    match self.codec {
                        Codec::H264 => match nal[0] & 31 {
                            5 => keyframe = true,
                            7 => parameters |= 1,
                            8 => parameters |= 2,
                            _ => {}
                        },
                        Codec::Hevc => match (nal[0] >> 1) & 0x3f {
                            19 | 20 | 21 => keyframe = true,
                            32 => parameters |= 1,
                            33 => parameters |= 2,
                            34 => parameters |= 4,
                            _ => {}
                        },
                        Codec::Av1 => unreachable!(),
                    }
                }

                if keyframe && parameters != full_mask && !header.is_empty() {
                    // MF_MT_MPEG_SEQUENCE_HEADER stores parameter sets in Annex B form.
                    for nal in nal_units(header) {
                        let kind = match self.codec {
                            Codec::H264 => nal[0] & 31,
                            Codec::Hevc => (nal[0] >> 1) & 0x3f,
                            Codec::Av1 => unreachable!(),
                        };
                        if param_nals.contains(&kind) {
                            packet.extend_from_slice(&[0, 0, 0, 1]);
                            packet.extend_from_slice(nal);
                        }
                    }
                }
                packet.extend_from_slice(picture);
            }
            let timestamp = u64::try_from(time)? * 9 / 1_000;
            Ok(
                AccessUnit::from_annex_b(self.codec, self.sequence, packet.into())?
                    .with_rtp_timestamp(timestamp)
                    .with_host_latency_us(encode_us),
            )
        })();
        match unit {
            Ok(unit) => {
                self.sequence += 1;
                if !self.synchronized && !unit.is_random_access() {
                    self.request.store(true, Ordering::Release);
                    self.stats.record(u64::from(encode_us), true);
                    return;
                }
                match self.output.try_send(Ok(unit)) {
                    Ok(()) => {
                        self.synchronized = true;
                        self.stats.record(u64::from(encode_us), false);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        self.synchronized = false;
                        self.request.store(true, Ordering::Release);
                        self.stats.record(u64::from(encode_us), true);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
            Err(error) => self.error = Some(error),
        }
    }
}
