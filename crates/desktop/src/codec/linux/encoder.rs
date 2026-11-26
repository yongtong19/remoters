//! Linux host `Encoder` facade, FFmpeg `EncoderProcess`, and `ActiveEncoder`
//! dispatcher (preferring direct NVENC with transparent FFmpeg fallback).

use std::{
    collections::VecDeque,
    io::{Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, ensure};
use remoters_core::video::{AccessUnit, AnnexBReader, Codec, Format};
use tokio::sync::mpsc;

use super::{
    capture::{capture_loop, wayland_capture_loop},
    ffmpeg_args::{
        EncoderArgsConfig, LinuxVideoBackend, build_encoder_args_with_input, probe_nvenc_codec,
    },
    ivf::IvfReader,
    nvenc,
};

#[derive(Clone)]
pub struct KeyframeTrigger {
    request: Arc<AtomicBool>,
}

impl KeyframeTrigger {
    pub fn request(&self) {
        self.request.store(true, Ordering::Release);
    }
}

pub struct Encoder {
    request: Arc<AtomicBool>,
    bitrate_override: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    pub units: mpsc::Receiver<Result<AccessUnit>>,
    worker: Option<JoinHandle<()>>,
}

impl Encoder {
    #[allow(dead_code)]
    pub fn start(format: Format, monitor: &xcap::Monitor) -> Result<Self> {
        Self::start_with_bitrate(format, monitor, None)
    }

    #[allow(dead_code)]
    pub fn start_with_bitrate(
        format: Format,
        monitor: &xcap::Monitor,
        bitrate_mbps: Option<u16>,
    ) -> Result<Self> {
        ensure!(
            matches!(format.codec, Codec::H264 | Codec::Hevc | Codec::Av1),
            "The Linux desktop encoder requires H.264, HEVC, or AV1."
        );
        let monitor_id = monitor.id()?;
        let request = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let bitrate_override = Arc::new(AtomicU32::new(crate::codec::encode_bitrate_override(
            bitrate_mbps,
        )));
        let worker_request = request.clone();
        let worker_stop = stop.clone();
        let worker_bitrate = bitrate_override.clone();
        let (output, units) = mpsc::channel(8);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("linux-screen-encoder".into())
            .spawn(move || {
                let mut ready = Some(ready_tx);
                if let Err(error) = capture_loop(
                    format,
                    monitor_id,
                    bitrate_mbps,
                    &worker_bitrate,
                    &worker_request,
                    &worker_stop,
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
            .map_err(|_| anyhow!("Linux encoder initialization stopped."))??;
        Ok(Self {
            request,
            bitrate_override,
            stop,
            units,
            worker: Some(worker),
        })
    }

    pub fn start_wayland(
        format: Format,
        portal: Arc<crate::platform::wayland_portal::WaylandPortalSession>,
        bitrate_mbps: Option<u16>,
    ) -> Result<Self> {
        ensure!(
            matches!(format.codec, Codec::H264 | Codec::Hevc | Codec::Av1),
            "The Linux desktop encoder requires H.264, HEVC, or AV1."
        );
        let request = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let bitrate_override = Arc::new(AtomicU32::new(crate::codec::encode_bitrate_override(
            bitrate_mbps,
        )));
        let worker_request = request.clone();
        let worker_stop = stop.clone();
        let worker_bitrate = bitrate_override.clone();
        let (output, units) = mpsc::channel(8);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("linux-wayland-encoder".into())
            .spawn(move || {
                let mut ready = Some(ready_tx);
                if let Err(error) = wayland_capture_loop(
                    format,
                    portal,
                    bitrate_mbps,
                    &worker_bitrate,
                    &worker_request,
                    &worker_stop,
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
            .map_err(|_| anyhow!("Linux Wayland encoder initialization stopped."))??;
        Ok(Self {
            request,
            bitrate_override,
            stop,
            units,
            worker: Some(worker),
        })
    }

    pub fn keyframe_trigger(&self) -> KeyframeTrigger {
        KeyframeTrigger {
            request: self.request.clone(),
        }
    }

    pub fn bitrate_control(&self) -> crate::codec::BitrateControl {
        crate::codec::BitrateControl::from_raw(self.bitrate_override.clone())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.units.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) struct EncoderProcess {
    pub(super) child: Child,
    stdin: Option<ChildStdin>,
    reader: Option<JoinHandle<()>>,
    pub(super) input_width: u32,
    pub(super) input_height: u32,
    input_pix_fmt: &'static str,
    pub(super) bitrate_bps: u32,
    timestamps: Option<Arc<Mutex<VecDeque<Instant>>>>,
    pub(super) packets_seen: Arc<AtomicU64>,
}

impl EncoderProcess {
    pub(super) fn start(
        config: &EncoderArgsConfig,
        output: mpsc::Sender<Result<AccessUnit>>,
        started_at: Instant,
        sequence_base: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::start_with_input(config, None, true, output, started_at, sequence_base, stop)
    }

    pub(super) fn start_x11grab(
        config: &EncoderArgsConfig,
        display_input: &str,
        output: mpsc::Sender<Result<AccessUnit>>,
        started_at: Instant,
        sequence_base: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        Self::start_with_input(
            config,
            Some(display_input),
            false,
            output,
            started_at,
            sequence_base,
            stop,
        )
    }

    fn start_with_input(
        config: &EncoderArgsConfig,
        x11grab_input: Option<&str>,
        track_timestamps: bool,
        output: mpsc::Sender<Result<AccessUnit>>,
        started_at: Instant,
        sequence_base: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let args = build_encoder_args_with_input(config, x11grab_input);
        let mut cmd = Command::new("ffmpeg");
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if x11grab_input.is_some() {
            cmd.stdin(Stdio::null());
        } else {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                anyhow!("ffmpeg is required for desktop video encoding on Linux.")
            } else {
                anyhow!("Failed to start ffmpeg for desktop video encoding: {err}")
            }
        })?;
        let stdin = if x11grab_input.is_some() {
            None
        } else {
            Some(
                child
                    .stdin
                    .take()
                    .context("Missing ffmpeg encoder stdin.")?,
            )
        };
        let stdout = child
            .stdout
            .take()
            .context("Missing ffmpeg encoder stdout.")?;
        let stderr = child.stderr.take();
        let codec = config.codec;
        let timestamps =
            track_timestamps.then(|| Arc::new(Mutex::new(VecDeque::<Instant>::with_capacity(8))));
        let reader_timestamps = timestamps.clone();
        let packets_seen = Arc::new(AtomicU64::new(0));
        let reader_packets = packets_seen.clone();

        let reader = thread::Builder::new()
            .name("linux-ffmpeg-encode".into())
            .spawn(move || {
                enum PacketReader<R> {
                    AnnexB(AnnexBReader<R>),
                    Ivf(IvfReader<R>),
                }
                impl<R: Read> PacketReader<R> {
                    fn next_unit(&mut self) -> Result<Option<AccessUnit>> {
                        match self {
                            Self::AnnexB(r) => r.next_unit(),
                            Self::Ivf(r) => r.next_unit(),
                        }
                    }
                }
                let mut reader = if codec == Codec::Av1 {
                    PacketReader::Ivf(IvfReader::new(stdout))
                } else {
                    PacketReader::AnnexB(AnnexBReader::new(stdout, codec))
                };
                let mut stats = crate::metrics::EncodeStats::new();
                let mut previous_time: Option<u64> = None;
                loop {
                    if stop.load(Ordering::Acquire) || output.is_closed() {
                        break;
                    }
                    match reader.next_unit() {
                        Ok(Some(mut unit)) => {
                            reader_packets.fetch_add(1, Ordering::Release);
                            let seq = sequence_base.fetch_add(1, Ordering::Relaxed);
                            unit.sequence = seq;
                            let elapsed_us = started_at.elapsed().as_micros();
                            let ticks = u64::try_from(elapsed_us * 90 / 1000).unwrap_or(u64::MAX);
                            let timestamp = previous_time.map_or(ticks, |last| ticks.max(last + 1));
                            previous_time = Some(timestamp);
                            let encode_us = reader_timestamps
                                .as_ref()
                                .and_then(|queue| queue.lock().unwrap().pop_front())
                                .map_or(0, |captured_at| captured_at.elapsed().as_micros() as u64);
                            let unit = unit
                                .with_rtp_timestamp(timestamp)
                                .with_host_latency_us(encode_us.min(u64::from(u32::MAX)) as u32);
                            match output.try_send(Ok(unit)) {
                                Ok(()) => {
                                    stats.record(encode_us, false);
                                }
                                Err(mpsc::error::TrySendError::Full(_)) => {
                                    stats.record(encode_us, true);
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => break,
                            }
                        }
                        Ok(None) => {
                            if let Some(mut err_pipe) = stderr {
                                let mut msg = String::new();
                                let _ = err_pipe.read_to_string(&mut msg);
                                let trimmed = msg.trim();
                                if !trimmed.is_empty() && !stop.load(Ordering::Acquire) {
                                    let _ = output.blocking_send(Err(anyhow!(
                                        "ffmpeg encoder exited: {trimmed}"
                                    )));
                                }
                            }
                            break;
                        }
                        Err(err) => {
                            if !stop.load(Ordering::Acquire) {
                                let _ = output.blocking_send(Err(err));
                            }
                            break;
                        }
                    }
                }
            })?;

        Ok(Self {
            child,
            stdin,
            reader: Some(reader),
            input_width: config.input_width,
            input_height: config.input_height,
            input_pix_fmt: config.input_pix_fmt,
            bitrate_bps: config.bitrate_bps,
            timestamps,
            packets_seen,
        })
    }

    fn write_frame_at(&mut self, pixels: &[u8], captured_at: Instant) -> Result<()> {
        if let Some(queue) = &self.timestamps {
            let mut guard = queue.lock().unwrap();
            if guard.len() >= 16 {
                guard.pop_front();
            }
            guard.push_back(captured_at);
        }
        let stdin = self
            .stdin
            .as_mut()
            .context("ffmpeg encoder input pipe closed.")?;
        stdin
            .write_all(pixels)
            .and_then(|()| stdin.flush())
            .map_err(|err| anyhow!("Failed to write raw frame to ffmpeg encoder: {err}"))
    }

    pub(super) fn shutdown(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_millis(200);
        while let Ok(None) = self.child.try_wait() {
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

pub(super) enum ActiveEncoder {
    Direct {
        nvenc: nvenc::DirectNvencEncoder,
        output: mpsc::Sender<Result<AccessUnit>>,
        started_at: Instant,
        sequence_base: Arc<AtomicU64>,
        previous_time: Option<u64>,
        stats: crate::metrics::EncodeStats,
    },
    Ffmpeg(EncoderProcess),
}

impl ActiveEncoder {
    pub(super) fn start(
        config: &EncoderArgsConfig,
        output: mpsc::Sender<Result<AccessUnit>>,
        started_at: Instant,
        sequence_base: Arc<AtomicU64>,
        child_stop: Arc<AtomicBool>,
    ) -> Result<(Self, LinuxVideoBackend)> {
        let mut backend = config.backend;
        let can_direct = backend == LinuxVideoBackend::NvencDirect
            && !config.hdr
            && config.input_width == u32::from(config.output_width)
            && config.input_height == u32::from(config.output_height);
        if can_direct {
            match nvenc::DirectNvencEncoder::new(
                config.codec,
                config.input_width,
                config.input_height,
                config.fps,
                config.bitrate_bps,
                config.input_pix_fmt,
            ) {
                Ok(nvenc) => {
                    return Ok((
                        Self::Direct {
                            nvenc,
                            output,
                            started_at,
                            sequence_base,
                            previous_time: None,
                            stats: crate::metrics::EncodeStats::new(),
                        },
                        LinuxVideoBackend::NvencDirect,
                    ));
                }
                Err(err) => {
                    eprintln!(
                        "Linux direct NVENC unavailable ({err:#}); falling back to FFmpeg encoder"
                    );
                    backend = if probe_nvenc_codec(config.codec, config.hdr) {
                        LinuxVideoBackend::Nvenc
                    } else {
                        LinuxVideoBackend::Software
                    };
                }
            }
        } else if backend == LinuxVideoBackend::NvencDirect {
            backend = if probe_nvenc_codec(config.codec, config.hdr) {
                LinuxVideoBackend::Nvenc
            } else {
                LinuxVideoBackend::Software
            };
        }

        let mut ffmpeg_cfg = *config;
        ffmpeg_cfg.backend = backend;
        match EncoderProcess::start(
            &ffmpeg_cfg,
            output.clone(),
            started_at,
            sequence_base.clone(),
            child_stop.clone(),
        ) {
            Ok(proc_state) => Ok((Self::Ffmpeg(proc_state), backend)),
            Err(err) if matches!(backend, LinuxVideoBackend::Nvenc | LinuxVideoBackend::Vaapi) => {
                eprintln!(
                    "Linux hardware encoder ({backend:?}) unavailable ({err:#}); falling back to software"
                );
                ffmpeg_cfg.backend = LinuxVideoBackend::Software;
                let proc_state = EncoderProcess::start(
                    &ffmpeg_cfg,
                    output,
                    started_at,
                    sequence_base,
                    child_stop,
                )?;
                Ok((Self::Ffmpeg(proc_state), LinuxVideoBackend::Software))
            }
            Err(err) => Err(err),
        }
    }

    pub(super) fn input_width(&self) -> u32 {
        match self {
            Self::Direct { nvenc, .. } => nvenc.width(),
            Self::Ffmpeg(p) => p.input_width,
        }
    }

    pub(super) fn input_height(&self) -> u32 {
        match self {
            Self::Direct { nvenc, .. } => nvenc.height(),
            Self::Ffmpeg(p) => p.input_height,
        }
    }

    pub(super) fn input_pix_fmt(&self) -> &'static str {
        match self {
            Self::Direct { nvenc, .. } => nvenc.input_pix_fmt(),
            Self::Ffmpeg(p) => p.input_pix_fmt,
        }
    }

    pub(super) fn bitrate_bps(&self) -> u32 {
        match self {
            Self::Direct { nvenc, .. } => nvenc.bitrate_bps(),
            Self::Ffmpeg(p) => p.bitrate_bps,
        }
    }

    pub(super) fn set_bitrate_live(&mut self, bitrate_bps: u32) -> Result<bool> {
        match self {
            Self::Direct { nvenc, .. } => {
                nvenc.set_bitrate(bitrate_bps)?;
                Ok(true)
            }
            Self::Ffmpeg(_) => Ok(false),
        }
    }

    pub(super) fn encode_frame(
        &mut self,
        pixels: &[u8],
        captured_at: Instant,
        force_keyframe: bool,
    ) -> Result<()> {
        match self {
            Self::Direct {
                nvenc,
                output,
                started_at,
                sequence_base,
                previous_time,
                stats,
            } => {
                let bytes = nvenc.encode_frame(pixels, force_keyframe)?;
                if bytes.is_empty() {
                    return Ok(());
                }
                let seq = sequence_base.fetch_add(1, Ordering::Relaxed);
                let elapsed_us = started_at.elapsed().as_micros();
                let ticks = u64::try_from(elapsed_us * 90 / 1000).unwrap_or(u64::MAX);
                let timestamp = previous_time.map_or(ticks, |last| ticks.max(last + 1));
                *previous_time = Some(timestamp);
                let encode_us = captured_at.elapsed().as_micros() as u64;
                let unit = AccessUnit::from_annex_b(nvenc.codec(), seq, bytes.into())?
                    .with_rtp_timestamp(timestamp)
                    .with_host_latency_us(encode_us.min(u64::from(u32::MAX)) as u32);
                match output.try_send(Ok(unit)) {
                    Ok(()) => stats.record(encode_us, false),
                    Err(mpsc::error::TrySendError::Full(_)) => stats.record(encode_us, true),
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
                Ok(())
            }
            Self::Ffmpeg(proc_state) => proc_state.write_frame_at(pixels, captured_at),
        }
    }

    pub(super) fn shutdown(self, child_stop: &AtomicBool) {
        match self {
            Self::Direct { .. } => {}
            Self::Ffmpeg(proc_state) => {
                child_stop.store(true, Ordering::Release);
                proc_state.shutdown();
                child_stop.store(false, Ordering::Release);
            }
        }
    }
}
