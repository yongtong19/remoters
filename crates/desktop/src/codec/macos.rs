use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc::SyncSender,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use remoters_core::{
    session::CursorShape,
    video::{AccessUnit, Codec, Format},
};
use screencapturekit::{
    cm::{CMSampleBufferExt, SCFrameStatus},
    cv::CVPixelBuffer,
    prelude::*,
};
use tokio::sync::mpsc;

use super::apple::HardwareEncoder;
use crate::audio::AudioChunker;

unsafe extern "C" {
    fn remoters_macos_cursor_shape() -> u8;
    fn remoters_macos_extract_audio(
        sample_buffer: *mut std::ffi::c_void,
        callback: unsafe extern "C" fn(*mut std::ffi::c_void, *const i16, usize),
        ctx: *mut std::ffi::c_void,
    ) -> bool;
}

pub struct CursorDetector;

impl CursorDetector {
    pub fn new() -> Self {
        Self
    }

    pub fn poll(&self) -> CursorShape {
        match unsafe { remoters_macos_cursor_shape() } {
            0 => CursorShape::Arrow,
            1 => CursorShape::IBeam,
            2 => CursorShape::Crosshair,
            3 => CursorShape::ClosedHand,
            4 => CursorShape::OpenHand,
            5 => CursorShape::PointingHand,
            6 => CursorShape::ResizeLeft,
            7 => CursorShape::ResizeRight,
            8 => CursorShape::ResizeLeftRight,
            9 => CursorShape::ResizeUp,
            10 => CursorShape::ResizeDown,
            11 => CursorShape::ResizeUpDown,
            12 => CursorShape::ResizeUpLeftDownRight,
            13 => CursorShape::ResizeUpRightDownLeft,
            14 => CursorShape::IBeamVertical,
            15 => CursorShape::OperationNotAllowed,
            16 => CursorShape::DragLink,
            17 => CursorShape::DragCopy,
            18 => CursorShape::ContextualMenu,
            19 => CursorShape::Hidden,
            _ => CursorShape::Arrow,
        }
    }
}

/// Static-screen keepalive interval (~5 fps) to keep VideoToolbox VBV, Apple Media Engine,
/// and QUIC warm when ScreenCaptureKit stops emitting frames on an idle screen.
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(200);
/// Maximum wait for the initial ScreenCaptureKit frame before failing capture startup.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum condvar wait slice while waiting for the first frame so shutdown stays responsive.
const INITIAL_WAIT_SLICE: Duration = Duration::from_millis(250);

/// RAII guard owning a retained (`CVPixelBufferRetain` / `CFRetain`) pixel buffer.
///
/// Releasing or replacing this guard invokes `CVPixelBufferRelease` / `CFRelease`
/// automatically via `Drop`, allowing the encoder thread to safely retain the latest
/// ScreenCaptureKit IOSurface for static-screen keepalives and on-demand keyframes.
#[derive(Clone)]
struct RetainedPixelBuffer(CVPixelBuffer);

impl RetainedPixelBuffer {
    fn new(surface: CVPixelBuffer) -> Self {
        Self(surface)
    }

    fn as_pixel_buffer(&self) -> &CVPixelBuffer {
        &self.0
    }
}

struct MailboxFrame<T> {
    frame: T,
    captured_at: Instant,
}

struct MailboxState<T> {
    pending: Option<MailboxFrame<T>>,
    keyframe: bool,
    closed: bool,
}

impl<T> Default for MailboxState<T> {
    fn default() -> Self {
        Self {
            pending: None,
            keyframe: false,
            closed: false,
        }
    }
}

enum MailboxEvent<T> {
    Frame {
        frame: T,
        captured_at: Instant,
        force_keyframe: bool,
    },
    KeyframeOnly,
    Timeout,
    Closed,
}

/// 1-slot latest-frame mailbox decoupling ScreenCaptureKit's callback thread from the
/// dedicated VideoToolbox encoder thread.
///
/// If a newer frame arrives while the encoder is busy, the pending slot is overwritten
/// and the superseded frame is returned so the producer can release it outside the lock.
struct LatestFrameMailbox<T> {
    state: Mutex<MailboxState<T>>,
    changed: Condvar,
}

impl<T> Default for LatestFrameMailbox<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(MailboxState::default()),
            changed: Condvar::new(),
        }
    }
}

impl<T> LatestFrameMailbox<T> {
    fn push(&self, frame: T, captured_at: Instant) -> Option<MailboxFrame<T>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let superseded = state.pending.replace(MailboxFrame { frame, captured_at });
        drop(state);
        self.changed.notify_one();
        superseded
    }

    fn request_keyframe(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.keyframe = true;
        drop(state);
        self.changed.notify_one();
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        let dropped = state.pending.take();
        drop(state);
        drop(dropped);
        self.changed.notify_all();
    }

    fn wait_next(&self, timeout: Duration, has_retained: bool) -> MailboxEvent<T> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                !state.closed && state.pending.is_none() && !(state.keyframe && has_retained)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return MailboxEvent::Closed;
        }
        if let Some(next) = state.pending.take() {
            let force_keyframe = std::mem::take(&mut state.keyframe);
            return MailboxEvent::Frame {
                frame: next.frame,
                captured_at: next.captured_at,
                force_keyframe,
            };
        }
        if has_retained && std::mem::take(&mut state.keyframe) {
            return MailboxEvent::KeyframeOnly;
        }
        MailboxEvent::Timeout
    }
}

/// Paces static-screen keepalive re-submissions (~5 fps / 200ms idle interval).
#[derive(Debug, Default, Clone, Copy)]
struct KeepalivePacer {
    last_emitted_at: Option<Instant>,
}

impl KeepalivePacer {
    fn wait_duration(&self, now: Instant, started_at: Instant) -> Duration {
        match self.last_emitted_at {
            Some(last) => KEEPALIVE_INTERVAL.saturating_sub(now.saturating_duration_since(last)),
            None => FIRST_FRAME_TIMEOUT
                .saturating_sub(now.saturating_duration_since(started_at))
                .min(INITIAL_WAIT_SLICE),
        }
    }

    fn should_emit_keepalive(&self, now: Instant) -> bool {
        self.last_emitted_at
            .is_some_and(|last| now.saturating_duration_since(last) >= KEEPALIVE_INTERVAL)
    }

    fn record_emission(&mut self, at: Instant) {
        self.last_emitted_at = Some(at);
    }
}

type Feed = LatestFrameMailbox<RetainedPixelBuffer>;

#[derive(Clone)]
pub struct KeyframeTrigger {
    feed: Arc<Feed>,
}

impl KeyframeTrigger {
    pub fn request(&self) {
        self.feed.request_keyframe();
    }
}

pub struct Encoder {
    feed: Arc<Feed>,
    bitrate_override: Arc<AtomicU32>,
    pub units: mpsc::Receiver<Result<AccessUnit>>,
}

impl Encoder {
    #[allow(dead_code)]
    pub fn start(format: Format, monitor: &xcap::Monitor) -> Result<Self> {
        Self::start_with_stream(format, monitor, None, None)
    }

    #[allow(dead_code)]
    pub fn start_with_audio(
        format: Format,
        monitor: &xcap::Monitor,
        audio_tx: Option<SyncSender<Vec<i16>>>,
    ) -> Result<Self> {
        Self::start_with_stream(format, monitor, audio_tx, None)
    }

    pub fn start_with_stream(
        format: Format,
        monitor: &xcap::Monitor,
        audio_tx: Option<SyncSender<Vec<i16>>>,
        bitrate_mbps: Option<u16>,
    ) -> Result<Self> {
        let display_id = monitor.id()?;
        let feed = Arc::new(Feed::default());
        let capture_feed = feed.clone();
        let bitrate_override =
            Arc::new(AtomicU32::new(super::encode_bitrate_override(bitrate_mbps)));
        let capture_bitrate = bitrate_override.clone();
        let (output, units) = mpsc::channel(2);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        thread::Builder::new()
            .name("macos-screen-encoder".into())
            .spawn(move || {
                let mut ready = Some(ready_tx);
                if let Err(error) = capture(
                    format,
                    display_id,
                    capture_feed,
                    &output,
                    audio_tx,
                    bitrate_mbps,
                    capture_bitrate,
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
            .map_err(|_| anyhow::anyhow!("macOS encoder initialization stopped."))??;
        Ok(Self {
            feed,
            bitrate_override,
            units,
        })
    }

    pub fn keyframe_trigger(&self) -> KeyframeTrigger {
        KeyframeTrigger {
            feed: self.feed.clone(),
        }
    }

    pub fn bitrate_control(&self) -> super::BitrateControl {
        super::BitrateControl::from_raw(self.bitrate_override.clone())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.units.close();
        self.feed.close();
    }
}

struct MacosAudioHandlerState {
    chunker: AudioChunker,
    sender: SyncSender<Vec<i16>>,
}

impl MacosAudioHandlerState {
    fn push_samples(&mut self, samples: &[i16]) {
        self.chunker.push(samples, |chunk| {
            let _ = self.sender.try_send(chunk);
        });
    }
}

unsafe extern "C" fn on_extracted_pcm(
    ctx: *mut std::ffi::c_void,
    samples: *const i16,
    sample_count: usize,
) {
    if ctx.is_null() || samples.is_null() || sample_count == 0 {
        return;
    }
    let state = unsafe { &mut *ctx.cast::<MacosAudioHandlerState>() };
    let slice = unsafe { std::slice::from_raw_parts(samples, sample_count) };
    state.push_samples(slice);
}

fn attach_screen_handler(stream: &mut SCStream, feed: Arc<Feed>) -> Result<()> {
    stream.add_output_handler(
        move |sample: CMSampleBuffer, kind| {
            if kind != SCStreamOutputType::Screen
                || sample.frame_status() != Some(SCFrameStatus::Complete)
            {
                return;
            }
            if let Some(surface) = sample.pixel_buffer() {
                let superseded = feed.push(RetainedPixelBuffer::new(surface), Instant::now());
                // Release any superseded CVPixelBuffer outside the mailbox lock.
                drop(superseded);
            }
        },
        SCStreamOutputType::Screen,
    )?;
    Ok(())
}

fn attach_audio_handler(stream: &mut SCStream, sender: SyncSender<Vec<i16>>) -> Result<()> {
    let state = Mutex::new(MacosAudioHandlerState {
        chunker: AudioChunker::new(),
        sender,
    });
    stream.add_output_handler(
        move |sample: CMSampleBuffer, kind| {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            let mut guard = match state.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            let ctx = (&mut *guard as *mut MacosAudioHandlerState).cast::<std::ffi::c_void>();
            unsafe {
                remoters_macos_extract_audio(sample.as_ptr(), on_extracted_pcm, ctx);
            }
        },
        SCStreamOutputType::Audio,
    )?;
    Ok(())
}

fn build_stream_config(format: Format, with_audio: bool) -> SCStreamConfiguration {
    let mut config = SCStreamConfiguration::new()
        .with_width(u32::from(format.width))
        .with_height(u32::from(format.height))
        .with_shows_cursor(false)
        .with_pixel_format(if format.hdr {
            PixelFormat::l10r
        } else {
            PixelFormat::YCbCr_420f
        })
        .with_minimum_frame_interval(&CMTime::new(1, i32::from(format.frames_per_second)));
    if format.hdr {
        let _ = config.set_color_space_name(
            screencapturekit::stream::configuration::color_space::ITUR_2100_PQ,
        );
    }
    if with_audio {
        config
            .with_captures_audio(true)
            .with_excludes_current_process_audio(true)
            .with_sample_rate(48_000)
            .with_channel_count(2)
    } else {
        config
    }
}

fn build_display_content_filter(
    display_id: u32,
    wait_for_display: bool,
) -> Result<(SCContentFilter, u64)> {
    let ready_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let filter_gen = crate::privacy::privacy_filter_generation();
        let excluded_ids = crate::privacy::active_privacy_window_ids();
        let content = SCShareableContent::get()?;
        if let Some(display) = content
            .displays()
            .into_iter()
            .find(|display| display.display_id() == display_id)
        {
            let windows = content.windows();
            let excluded: Vec<&SCWindow> = if excluded_ids.is_empty() {
                Vec::new()
            } else {
                windows
                    .iter()
                    .filter(|window| excluded_ids.contains(&window.window_id()))
                    .collect()
            };
            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&excluded)
                .build()?;
            return Ok((filter, filter_gen));
        }
        ensure!(
            wait_for_display && Instant::now() < ready_deadline,
            "The selected display is unavailable to ScreenCaptureKit."
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn maybe_refresh_privacy_filter(
    stream: &SCStream,
    display_id: u32,
    applied_filter_gen: &mut u64,
    recover: &mut bool,
) {
    let current_filter_gen = crate::privacy::privacy_filter_generation();
    if current_filter_gen == *applied_filter_gen {
        return;
    }
    match build_display_content_filter(display_id, false) {
        Ok((new_filter, rebuilt_gen)) => {
            if let Err(err) = stream.update_content_filter(&new_filter) {
                eprintln!("macOS ScreenCaptureKit privacy filter update failed: {err}");
            } else {
                *applied_filter_gen = rebuilt_gen;
                *recover = true;
            }
        }
        Err(err) => {
            eprintln!("macOS ScreenCaptureKit privacy filter rebuild failed: {err:#}");
            *applied_filter_gen = current_filter_gen;
        }
    }
}

fn capture(
    format: Format,
    display_id: u32,
    feed: Arc<Feed>,
    output: &mpsc::Sender<Result<AccessUnit>>,
    audio_tx: Option<SyncSender<Vec<i16>>>,
    bitrate_mbps: Option<u16>,
    bitrate_override: Arc<AtomicU32>,
    ready: &mut Option<std::sync::mpsc::SyncSender<Result<()>>>,
) -> Result<()> {
    ensure!(
        matches!(format.codec, Codec::H264 | Codec::Hevc),
        "The macOS desktop encoder requires H.264 or HEVC."
    );
    let (filter, initial_filter_gen) = build_display_content_filter(display_id, true)?;
    let initial_bitrate = super::effective_bitrate(format, bitrate_mbps);
    let mut encoder = HardwareEncoder::new(format, initial_bitrate)?;

    let stream = if let Some(sender) = audio_tx {
        let config = build_stream_config(format, true);
        let mut with_audio_stream = SCStream::new(&filter, &config)?;
        attach_screen_handler(&mut with_audio_stream, feed.clone())?;
        if let Err(error) = attach_audio_handler(&mut with_audio_stream, sender) {
            eprintln!("macOS system audio handler failed; continuing video only: {error}");
        }
        match with_audio_stream.start_capture() {
            Ok(()) => with_audio_stream,
            Err(error) => {
                eprintln!(
                    "macOS system audio stream failed to start; retrying video only: {error}"
                );
                let video_only = build_stream_config(format, false);
                let mut fallback_stream = SCStream::new(&filter, &video_only)?;
                attach_screen_handler(&mut fallback_stream, feed.clone())?;
                fallback_stream.start_capture()?;
                fallback_stream
            }
        }
    } else {
        let config = build_stream_config(format, false);
        let mut video_stream = SCStream::new(&filter, &config)?;
        attach_screen_handler(&mut video_stream, feed.clone())?;
        video_stream.start_capture()?;
        video_stream
    };

    if let Some(ready_tx) = ready.take() {
        let _ = ready_tx.send(Ok(()));
    }
    let result = send_frames(
        &mut encoder,
        &stream,
        display_id,
        initial_filter_gen,
        format,
        initial_bitrate,
        &bitrate_override,
        &feed,
        output,
    );
    let _ = stream.stop_capture();
    result
}

#[allow(clippy::too_many_arguments)]
fn send_frames(
    encoder: &mut HardwareEncoder,
    stream: &SCStream,
    display_id: u32,
    initial_filter_gen: u64,
    format: Format,
    initial_bitrate: u32,
    bitrate_override: &AtomicU32,
    feed: &Feed,
    output: &mpsc::Sender<Result<AccessUnit>>,
) -> Result<()> {
    let mut retained = None::<RetainedPixelBuffer>;
    let started = Instant::now();
    let mut pacer = KeepalivePacer::default();
    let mut sequence = 0;
    let mut previous_time = None::<u64>;
    let mut recover = true;
    let mut active_bitrate = initial_bitrate;
    let mut applied_filter_gen = initial_filter_gen;
    let mut stats = crate::metrics::EncodeStats::new();
    maybe_refresh_privacy_filter(stream, display_id, &mut applied_filter_gen, &mut recover);
    while !output.is_closed() {
        let wait = pacer.wait_duration(Instant::now(), started);
        let (submitted_at, force) = match feed.wait_next(wait, retained.is_some()) {
            MailboxEvent::Closed => break,
            MailboxEvent::Frame {
                frame,
                captured_at,
                force_keyframe,
            } => {
                retained = Some(frame);
                (captured_at, force_keyframe)
            }
            MailboxEvent::KeyframeOnly => (Instant::now(), true),
            MailboxEvent::Timeout => {
                maybe_refresh_privacy_filter(
                    stream,
                    display_id,
                    &mut applied_filter_gen,
                    &mut recover,
                );
                let now = Instant::now();
                if retained.is_some() {
                    if !pacer.should_emit_keepalive(now) {
                        continue;
                    }
                    // Static-screen keepalive (~5 fps): re-submit retained CVPixelBuffer
                    // as a tiny skip-P-frame with a fresh submission instant.
                    (now, false)
                } else {
                    ensure!(
                        now.saturating_duration_since(started) < FIRST_FRAME_TIMEOUT,
                        "ScreenCaptureKit did not deliver a desktop frame."
                    );
                    continue;
                }
            }
        };
        maybe_refresh_privacy_filter(stream, display_id, &mut applied_filter_gen, &mut recover);
        if output.is_closed() {
            break;
        }
        let Some(surface) = retained.as_ref().map(RetainedPixelBuffer::as_pixel_buffer) else {
            continue;
        };

        let target_bitrate = super::effective_bitrate(
            format,
            super::decode_bitrate_override(bitrate_override.load(Ordering::Acquire)),
        );
        if target_bitrate != active_bitrate {
            if let Err(err) = encoder.set_bitrate(target_bitrate) {
                eprintln!("macOS hardware encoder bitrate update failed: {err:#}");
            } else {
                active_bitrate = target_bitrate;
            }
        }

        let ticks =
            u64::try_from(submitted_at.saturating_duration_since(started).as_micros() * 90 / 1000)?;
        let timestamp = previous_time.map_or(ticks, |last| ticks.max(last + 1));
        let encode_start = Instant::now();
        let encoded = encoder.encode(surface, timestamp, force || recover)?;
        pacer.record_emission(Instant::now());
        let Some(bytes) = encoded else {
            continue;
        };
        let encode_us = encode_start.elapsed().as_micros() as u64;
        let host_latency_us = submitted_at.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        previous_time = Some(timestamp);
        let unit = AccessUnit::from_annex_b(format.codec, sequence, bytes.into())?
            .with_rtp_timestamp(timestamp)
            .with_host_latency_us(host_latency_us);
        sequence += 1;
        match output.try_send(Ok(unit)) {
            Ok(()) => {
                recover = false;
                stats.record(encode_us, false);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                recover = true;
                stats.record(encode_us, true);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
    use screencapturekit::cv::CVPixelBuffer;

    use super::{
        CursorDetector, INITIAL_WAIT_SLICE, KEEPALIVE_INTERVAL, KeepalivePacer, LatestFrameMailbox,
        MailboxEvent, RetainedPixelBuffer,
    };

    #[test]
    fn macos_cursor_detector_polls_safely_across_threads() {
        let detector = CursorDetector::new();
        let _ = detector.poll();
        std::thread::spawn(move || {
            let _ = detector.poll();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn keepalive_pacer_emits_at_200ms_idle_and_resets_on_new_frames() {
        let start = Instant::now();
        let mut pacer = KeepalivePacer::default();

        // Before any frame arrives, keepalive is inactive and wait is capped for prompt shutdown.
        assert!(!pacer.should_emit_keepalive(start + Duration::from_secs(1)));
        assert_eq!(pacer.wait_duration(start, start), INITIAL_WAIT_SLICE);

        // After emitting a frame at t0, wait tracks the remaining 200ms keepalive interval.
        let t0 = start + Duration::from_millis(10);
        pacer.record_emission(t0);
        assert!(!pacer.should_emit_keepalive(t0 + Duration::from_millis(50)));
        assert_eq!(
            pacer.wait_duration(t0 + Duration::from_millis(50), start),
            Duration::from_millis(150)
        );
        assert!(!pacer.should_emit_keepalive(t0 + Duration::from_millis(199)));
        assert_eq!(
            pacer.wait_duration(t0 + Duration::from_millis(199), start),
            Duration::from_millis(1)
        );

        // At 200ms idle, keepalive triggers (~5 fps).
        let t_keepalive = t0 + KEEPALIVE_INTERVAL;
        assert!(pacer.should_emit_keepalive(t_keepalive));
        assert_eq!(pacer.wait_duration(t_keepalive, start), Duration::ZERO);

        // Recording the keepalive schedules the next keepalive 200ms later.
        pacer.record_emission(t_keepalive);
        assert!(!pacer.should_emit_keepalive(t_keepalive + Duration::from_millis(150)));
        assert!(pacer.should_emit_keepalive(t_keepalive + KEEPALIVE_INTERVAL));

        // A fresh capture frame mid-interval resets the 200ms idle deadline.
        let t_fresh = t_keepalive + Duration::from_millis(60);
        pacer.record_emission(t_fresh);
        assert!(!pacer.should_emit_keepalive(t_keepalive + KEEPALIVE_INTERVAL));
        assert_eq!(
            pacer.wait_duration(t_keepalive + KEEPALIVE_INTERVAL, start),
            Duration::from_millis(60)
        );
        assert!(pacer.should_emit_keepalive(t_fresh + KEEPALIVE_INTERVAL));
    }

    #[test]
    fn latest_frame_mailbox_replaces_superseded_frames_and_wakes_on_close() {
        let mailbox = Arc::new(LatestFrameMailbox::<u32>::default());
        let t1 = Instant::now();
        assert!(mailbox.push(1, t1).is_none());

        let t2 = t1 + Duration::from_millis(16);
        let superseded = mailbox.push(2, t2).expect("older frame should be replaced");
        assert_eq!(superseded.frame, 1);
        assert_eq!(superseded.captured_at, t1);

        mailbox.request_keyframe();
        match mailbox.wait_next(Duration::from_millis(50), true) {
            MailboxEvent::Frame {
                frame,
                captured_at,
                force_keyframe,
            } => {
                assert_eq!(frame, 2);
                assert_eq!(captured_at, t2);
                assert!(force_keyframe);
            }
            _ => panic!("expected latest frame with keyframe flag"),
        }

        // Keyframe request on static screen wakes immediately when a retained frame exists.
        mailbox.request_keyframe();
        assert!(matches!(
            mailbox.wait_next(Duration::from_millis(50), true),
            MailboxEvent::KeyframeOnly
        ));

        // Closing the mailbox wakes a waiting encoder thread immediately.
        let waiter_mailbox = mailbox.clone();
        let handle = thread::spawn(move || waiter_mailbox.wait_next(Duration::from_secs(5), true));
        thread::sleep(Duration::from_millis(15));
        mailbox.close();
        assert!(matches!(handle.join().unwrap(), MailboxEvent::Closed));
    }

    #[test]
    fn retained_pixel_buffer_replaces_and_retains_safely_in_mailbox() {
        let first = RetainedPixelBuffer::new(
            CVPixelBuffer::create(160, 90, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)
                .expect("first NV12 pixel buffer"),
        );
        let second = RetainedPixelBuffer::new(
            CVPixelBuffer::create(160, 90, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)
                .expect("second NV12 pixel buffer"),
        );
        let second_ptr = second.as_pixel_buffer().as_ptr();

        let mailbox = LatestFrameMailbox::<RetainedPixelBuffer>::default();
        assert!(mailbox.push(first, Instant::now()).is_none());
        let superseded = mailbox
            .push(second, Instant::now())
            .expect("first buffer should be superseded");
        assert_eq!(superseded.frame.as_pixel_buffer().width(), 160);
        drop(superseded);

        match mailbox.wait_next(Duration::from_millis(50), false) {
            MailboxEvent::Frame { frame, .. } => {
                assert_eq!(frame.as_pixel_buffer().as_ptr(), second_ptr);
                assert_eq!(frame.as_pixel_buffer().width(), 160);
                assert_eq!(frame.as_pixel_buffer().height(), 90);
            }
            _ => panic!("expected retained CVPixelBuffer"),
        }
    }
}
