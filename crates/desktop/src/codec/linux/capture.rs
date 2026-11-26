//! Linux Wayland (PipeWire) and X11 (`x11grab` / `xcap`) capture-to-encode pipelines.

use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use remoters_core::video::{AccessUnit, Format};
use tokio::sync::mpsc;

use super::{
    encoder::{ActiveEncoder, EncoderProcess},
    ffmpeg_args::{EncoderArgsConfig, LinuxVideoBackend, probe_nvenc_codec},
    pipewire::{
        LatestFrameSlot, PwAcquireResult, PwCaptureHandle, fourcc_to_pix_fmt, remoters_pw_start,
    },
};

const ENCODER_STARTUP_GRACE: Duration = Duration::from_secs(5);
/// Keepalive cadence (~5 fps) sent when the Wayland screen is completely static,
/// preventing rate-control/transport stalls without burning CPU at 60 fps.
const WAYLAND_KEEPALIVE_INTERVAL: Duration = Duration::from_millis(200);

#[allow(clippy::too_many_arguments)]
pub(super) fn wayland_capture_loop(
    format: Format,
    portal: Arc<crate::platform::wayland_portal::WaylandPortalSession>,
    initial_bitrate_mbps: Option<u16>,
    bitrate_override: &AtomicU32,
    request: &AtomicBool,
    stop: &AtomicBool,
    output: &mpsc::Sender<Result<AccessUnit>>,
    ready: &mut Option<std_mpsc::SyncSender<Result<()>>>,
) -> Result<()> {
    let pw_fd: OwnedFd = portal.clone_pipewire_fd()?;
    let mut pw_w = 0u32;
    let mut pw_h = 0u32;
    let mut pw_fourcc = 0u32;
    let stream_ptr = unsafe {
        remoters_pw_start(
            pw_fd.as_raw_fd(),
            portal.node_id(),
            &mut pw_w,
            &mut pw_h,
            &mut pw_fourcc,
        )
    };
    drop(pw_fd);
    ensure!(
        !stream_ptr.is_null() && pw_w >= 2 && pw_h >= 2,
        "PipeWire Wayland screen capture stream failed to start."
    );
    let capture = PwCaptureHandle(stream_ptr);

    let mut last_seq = 0u64;
    let initial_frame = match capture.acquire_frame(&mut last_seq, 1000, false) {
        PwAcquireResult::Frame(frame) => frame,
        _ => bail!("Failed to read initial PipeWire frame."),
    };
    pw_w = initial_frame.width;
    pw_h = initial_frame.height;
    let mut pix_fmt = fourcc_to_pix_fmt(initial_frame.fourcc);

    let mut backend = LinuxVideoBackend::from_env_for_codec(format.codec, format.hdr);
    let initial_bitrate = crate::codec::effective_bitrate(format, initial_bitrate_mbps);
    let started_at = Instant::now();
    let sequence_base = Arc::new(AtomicU64::new(0));
    let child_stop = Arc::new(AtomicBool::new(false));

    let make_config =
        |w: u32, h: u32, bps: u32, bk: LinuxVideoBackend, fmt: &'static str| EncoderArgsConfig {
            codec: format.codec,
            input_width: w,
            input_height: h,
            output_width: format.width,
            output_height: format.height,
            fps: format.frames_per_second,
            bitrate_bps: bps,
            hdr: format.hdr,
            backend: bk,
            input_pix_fmt: fmt,
        };

    let (mut encoder, active_backend) = ActiveEncoder::start(
        &make_config(pw_w, pw_h, initial_bitrate, backend, pix_fmt),
        output.clone(),
        started_at,
        sequence_base.clone(),
        child_stop.clone(),
    )?;
    backend = active_backend;

    {
        let initial_len = usize::try_from(pw_w)? * usize::try_from(pw_h)? * 4;
        let initial_captured_at = initial_frame.captured_at();
        if let Err(err) = encoder.encode_frame(
            &initial_frame.pixels()[..initial_len],
            initial_captured_at,
            true,
        ) {
            if matches!(
                backend,
                LinuxVideoBackend::NvencDirect
                    | LinuxVideoBackend::Nvenc
                    | LinuxVideoBackend::Vaapi
            ) {
                eprintln!(
                    "Linux Wayland hardware encoder ({backend:?}) failed on initial frame ({err:#}); falling back"
                );
                encoder.shutdown(&child_stop);
                let fallback_backend = if backend == LinuxVideoBackend::NvencDirect
                    && probe_nvenc_codec(format.codec, format.hdr)
                {
                    LinuxVideoBackend::Nvenc
                } else {
                    LinuxVideoBackend::Software
                };
                let (next_enc, next_bk) = ActiveEncoder::start(
                    &make_config(pw_w, pw_h, initial_bitrate, fallback_backend, pix_fmt),
                    output.clone(),
                    started_at,
                    sequence_base.clone(),
                    child_stop.clone(),
                )?;
                encoder = next_enc;
                backend = next_bk;
                encoder.encode_frame(
                    &initial_frame.pixels()[..initial_len],
                    initial_captured_at,
                    true,
                )?;
            } else {
                encoder.shutdown(&child_stop);
                return Err(err);
            }
        }
    }
    drop(initial_frame);

    if let Some(ready_tx) = ready.take() {
        let _ = ready_tx.send(Ok(()));
    }

    let mut last_write = Instant::now();
    let read_timeout_ms = (1000 / u32::from(format.frames_per_second.max(1))).clamp(8, 33);

    while !stop.load(Ordering::Acquire) && !output.is_closed() {
        let keepalive_due = last_write.elapsed() >= WAYLAND_KEEPALIVE_INTERVAL;
        let keyframe_requested = request.load(Ordering::Acquire);
        let frame = match capture.acquire_frame(
            &mut last_seq,
            read_timeout_ms,
            keepalive_due || keyframe_requested,
        ) {
            PwAcquireResult::Frame(frame) => frame,
            PwAcquireResult::Timeout => continue,
            PwAcquireResult::Closed => break,
        };

        pw_w = frame.width;
        pw_h = frame.height;
        pix_fmt = fourcc_to_pix_fmt(frame.fourcc);
        let captured_at = frame.captured_at();

        let target_bitrate = crate::codec::effective_bitrate(
            format,
            crate::codec::decode_bitrate_override(bitrate_override.load(Ordering::Acquire)),
        );

        let geometry_or_fmt_changed = pw_w != encoder.input_width()
            || pw_h != encoder.input_height()
            || pix_fmt != encoder.input_pix_fmt();
        let mut needs_restart = geometry_or_fmt_changed;
        if !needs_restart && target_bitrate != encoder.bitrate_bps() {
            match encoder.set_bitrate_live(target_bitrate) {
                Ok(true) => {}
                Ok(false) => needs_restart = true,
                Err(err) => {
                    eprintln!(
                        "Live NVENC bitrate reconfigure failed ({err:#}); restarting encoder"
                    );
                    needs_restart = true;
                }
            }
        }

        if needs_restart {
            encoder.shutdown(&child_stop);
            let (next_enc, next_bk) = ActiveEncoder::start(
                &make_config(pw_w, pw_h, target_bitrate, backend, pix_fmt),
                output.clone(),
                started_at,
                sequence_base.clone(),
                child_stop.clone(),
            )?;
            encoder = next_enc;
            backend = next_bk;
        }

        let frame_len = usize::try_from(pw_w)? * usize::try_from(pw_h)? * 4;
        let pixels = frame.pixels();
        if pixels.len() >= frame_len {
            let force_idr = request.swap(false, Ordering::AcqRel);
            if let Err(err) = encoder.encode_frame(&pixels[..frame_len], captured_at, force_idr) {
                drop(frame);
                encoder.shutdown(&child_stop);
                return Err(err);
            }
            last_write = Instant::now();
        }
    }

    encoder.shutdown(&child_stop);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn capture_loop(
    format: Format,
    monitor_id: u32,
    initial_bitrate_mbps: Option<u16>,
    bitrate_override: &AtomicU32,
    request: &AtomicBool,
    stop: &AtomicBool,
    output: &mpsc::Sender<Result<AccessUnit>>,
    ready: &mut Option<std_mpsc::SyncSender<Result<()>>>,
) -> Result<()> {
    let monitor = crate::platform::display::monitor_by_id(monitor_id)?;
    let mut backend = LinuxVideoBackend::from_env_for_codec(format.codec, format.hdr);
    let initial_bitrate = crate::codec::effective_bitrate(format, initial_bitrate_mbps);
    let started_at = Instant::now();
    let sequence_base = Arc::new(AtomicU64::new(0));
    let child_stop = Arc::new(AtomicBool::new(false));

    let make_config =
        |w: u32, h: u32, bps: u32, bk: LinuxVideoBackend, fmt: &'static str| EncoderArgsConfig {
            codec: format.codec,
            input_width: w,
            input_height: h,
            output_width: format.width,
            output_height: format.height,
            fps: format.frames_per_second,
            bitrate_bps: bps,
            hdr: format.hdr,
            backend: bk,
            input_pix_fmt: fmt,
        };

    // Fast path on X11 when using FFmpeg backend (e.g. REMOTERS_LINUX_ENCODER=ffmpeg|nvenc|vaapi
    // or HDR/downscaling): capture directly inside ffmpeg via x11grab with zero pipe copies.
    let prefer_x11grab = backend != LinuxVideoBackend::NvencDirect || format.hdr;
    if prefer_x11grab
        && let Ok((origin_x, origin_y, in_w, in_h)) =
            crate::platform::linux::monitor_physical_bounds(&monitor)
        && in_w >= 2
        && in_h >= 2
    {
        let display_env = std::env::var("DISPLAY")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| ":0".to_owned());
        let x11_input = format!("{display_env}+{origin_x},{origin_y}");
        let mut x11_cfg = make_config(in_w, in_h, initial_bitrate, backend, "bgr0");
        if let Ok(mut proc_state) = EncoderProcess::start_x11grab(
            &x11_cfg,
            &x11_input,
            output.clone(),
            started_at,
            sequence_base.clone(),
            child_stop.clone(),
        ) {
            let probe_deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < probe_deadline && !stop.load(Ordering::Acquire) {
                if proc_state.packets_seen.load(Ordering::Acquire) > 0 {
                    break;
                }
                if let Ok(Some(_)) = proc_state.child.try_wait() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            if proc_state.packets_seen.load(Ordering::Acquire) > 0 {
                if let Some(ready_tx) = ready.take() {
                    let _ = ready_tx.send(Ok(()));
                }
                while !stop.load(Ordering::Acquire) && !output.is_closed() {
                    thread::sleep(Duration::from_millis(150));
                    let Ok((_, _, cur_w, cur_h)) =
                        crate::platform::linux::monitor_physical_bounds(&monitor)
                    else {
                        break;
                    };
                    let target_bitrate = crate::codec::effective_bitrate(
                        format,
                        crate::codec::decode_bitrate_override(
                            bitrate_override.load(Ordering::Acquire),
                        ),
                    );
                    if cur_w != proc_state.input_width
                        || cur_h != proc_state.input_height
                        || target_bitrate != proc_state.bitrate_bps
                    {
                        child_stop.store(true, Ordering::Release);
                        proc_state.shutdown();
                        child_stop.store(false, Ordering::Release);
                        x11_cfg = make_config(cur_w, cur_h, target_bitrate, backend, "bgr0");
                        proc_state = EncoderProcess::start_x11grab(
                            &x11_cfg,
                            &x11_input,
                            output.clone(),
                            started_at,
                            sequence_base.clone(),
                            child_stop.clone(),
                        )?;
                    } else if let Ok(Some(_)) = proc_state.child.try_wait() {
                        break;
                    }
                }
                child_stop.store(true, Ordering::Release);
                proc_state.shutdown();
                return Ok(());
            }
            child_stop.store(true, Ordering::Release);
            proc_state.shutdown();
            child_stop.store(false, Ordering::Release);
        }
    }

    let initial_captured_at = Instant::now();
    let initial_image = monitor
        .capture_image()
        .context("Could not capture the selected Linux display.")?;
    let (input_width, input_height) = initial_image.dimensions();
    ensure!(
        input_width >= 2 && input_height >= 2,
        "Captured Linux display has invalid dimensions."
    );

    let (mut encoder, active_backend) = ActiveEncoder::start(
        &make_config(input_width, input_height, initial_bitrate, backend, "rgba"),
        output.clone(),
        started_at,
        sequence_base.clone(),
        child_stop.clone(),
    )?;
    backend = active_backend;

    if let Err(err) = encoder.encode_frame(initial_image.as_raw(), initial_captured_at, true) {
        if matches!(
            backend,
            LinuxVideoBackend::NvencDirect | LinuxVideoBackend::Nvenc | LinuxVideoBackend::Vaapi
        ) {
            eprintln!(
                "Linux hardware encoder ({backend:?}) failed on initial frame ({err:#}); falling back"
            );
            encoder.shutdown(&child_stop);
            let fallback_backend = if backend == LinuxVideoBackend::NvencDirect
                && probe_nvenc_codec(format.codec, format.hdr)
            {
                LinuxVideoBackend::Nvenc
            } else {
                LinuxVideoBackend::Software
            };
            let (next_enc, next_bk) = ActiveEncoder::start(
                &make_config(
                    input_width,
                    input_height,
                    initial_bitrate,
                    fallback_backend,
                    "rgba",
                ),
                output.clone(),
                started_at,
                sequence_base.clone(),
                child_stop.clone(),
            )?;
            encoder = next_enc;
            backend = next_bk;
            encoder.encode_frame(initial_image.as_raw(), initial_captured_at, true)?;
        } else {
            encoder.shutdown(&child_stop);
            return Err(err);
        }
    }

    if let Some(ready_tx) = ready.take() {
        let _ = ready_tx.send(Ok(()));
    }

    // Decoupled capture producer -> double-buffered LatestFrameSlot -> encoder consumer
    // so screen capture never blocks on encoding or pipe writes.
    let slot = Arc::new(LatestFrameSlot::new());
    let producer_slot = slot.clone();
    let producer_stop = Arc::new(AtomicBool::new(false));
    let producer_stop_flag = producer_stop.clone();
    let frame_interval = Duration::from_secs_f64(1.0 / f64::from(format.frames_per_second.max(1)));

    let producer = thread::Builder::new()
        .name("linux-x11-capture".into())
        .spawn(move || {
            let Ok(cap_monitor) = crate::platform::display::monitor_by_id(monitor_id) else {
                producer_slot.close();
                return;
            };
            let mut next_tick = Instant::now() + frame_interval;
            let mut staging = Vec::new();
            while !producer_stop_flag.load(Ordering::Acquire) {
                let now = Instant::now();
                if now < next_tick {
                    thread::sleep((next_tick - now).min(Duration::from_millis(4)));
                    continue;
                }
                next_tick = (next_tick + frame_interval).max(Instant::now());
                let captured_at = Instant::now();
                match cap_monitor.capture_image() {
                    Ok(img) => {
                        let (w, h) = img.dimensions();
                        staging.clear();
                        staging.extend_from_slice(img.as_raw());
                        producer_slot.publish_swap(&mut staging, w, h, captured_at);
                    }
                    Err(_) if started_at.elapsed() >= ENCODER_STARTUP_GRACE => {
                        break;
                    }
                    Err(_) => {}
                }
            }
            producer_slot.close();
        })?;

    let mut last_seq = 0u64;
    let mut encode_buf = Vec::new();
    let wait_timeout = frame_interval.clamp(Duration::from_millis(8), Duration::from_millis(33));
    let mut encoder = Some(encoder);

    let loop_result = (|| -> Result<()> {
        while !stop.load(Ordering::Acquire) && !output.is_closed() {
            let Some(meta) = slot.take_latest(&mut encode_buf, &mut last_seq, wait_timeout) else {
                if producer.is_finished() {
                    break;
                }
                continue;
            };

            let target_bitrate = crate::codec::effective_bitrate(
                format,
                crate::codec::decode_bitrate_override(bitrate_override.load(Ordering::Acquire)),
            );

            let active = encoder.as_mut().expect("active encoder");
            let mut needs_restart =
                meta.width != active.input_width() || meta.height != active.input_height();
            if !needs_restart && target_bitrate != active.bitrate_bps() {
                match active.set_bitrate_live(target_bitrate) {
                    Ok(true) => {}
                    Ok(false) => needs_restart = true,
                    Err(err) => {
                        eprintln!(
                            "Live NVENC bitrate reconfigure failed ({err:#}); restarting encoder"
                        );
                        needs_restart = true;
                    }
                }
            }

            if needs_restart {
                if let Some(prev) = encoder.take() {
                    prev.shutdown(&child_stop);
                }
                let (next_enc, next_bk) = ActiveEncoder::start(
                    &make_config(meta.width, meta.height, target_bitrate, backend, "rgba"),
                    output.clone(),
                    started_at,
                    sequence_base.clone(),
                    child_stop.clone(),
                )?;
                encoder = Some(next_enc);
                backend = next_bk;
            }

            let force_idr = request.swap(false, Ordering::AcqRel);
            encoder.as_mut().expect("active encoder").encode_frame(
                &encode_buf,
                meta.captured_at,
                force_idr,
            )?;
        }
        Ok(())
    })();

    producer_stop.store(true, Ordering::Release);
    slot.close();
    let _ = producer.join();
    if let Some(enc) = encoder {
        enc.shutdown(&child_stop);
    }
    loop_result
}
