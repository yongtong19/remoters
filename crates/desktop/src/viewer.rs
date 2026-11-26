use std::{
    collections::VecDeque,
    net::SocketAddrV4,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use remoters_core::{
    GamepadState, Input, Point,
    network::Pairing,
    session::{
        ClipboardData, CursorShape, DisplayPreference, SessionAuxiliary, StreamPreference,
        ViewerSession,
    },
    video::{Codec, ReceivedFrame, nal_units, obu},
};
use tokio::{
    runtime::Builder,
    sync::{Notify, oneshot},
};

use crate::{
    codec::{self, Decoder},
    metrics::{LiveViewerMetrics, SessionTelemetry, ViewerStats},
    platform::VideoFrame,
};

const DECODE_QUEUE_CAPACITY: usize = 2;
const BALANCED_PACING_CAPACITY: usize = 2;
const MAX_COMMAND_QUEUE: usize = 512;
const GAMEPAD_SOFT_LIMIT: usize = 96;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum FramePacingMode {
    #[default]
    LowestLatency = 0,
    Balanced = 1,
}

impl FramePacingMode {
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Balanced,
            _ => Self::LowestLatency,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::LowestLatency => "Low latency",
            Self::Balanced => "Balanced",
        }
    }
}

pub enum Event {
    Ready(bool),
    Cursor(CursorShape),
    Clipboard(ClipboardData),
    Transfer {
        message: String,
        error: bool,
        active: bool,
    },
    Closed(Result<()>),
}

#[derive(Debug)]
enum Command {
    Input(Input),
    SetBitrate(Option<u16>),
    Clipboard(ClipboardData),
    SendFiles(Vec<(String, PathBuf)>),
}

struct CommandQueueInner {
    queue: VecDeque<Command>,
    pending_move: Option<Point>,
    closed: bool,
}

struct CommandQueue {
    inner: Mutex<CommandQueueInner>,
    notify: Notify,
    senders: AtomicUsize,
}

struct CommandSender {
    queue: Arc<CommandQueue>,
}

struct CommandReceiver {
    queue: Arc<CommandQueue>,
}

fn command_channel() -> (CommandSender, CommandReceiver) {
    let queue = Arc::new(CommandQueue {
        inner: Mutex::new(CommandQueueInner {
            queue: VecDeque::with_capacity(32),
            pending_move: None,
            closed: false,
        }),
        notify: Notify::new(),
        senders: AtomicUsize::new(1),
    });
    (
        CommandSender {
            queue: queue.clone(),
        },
        CommandReceiver { queue },
    )
}

impl Clone for CommandSender {
    fn clone(&self) -> Self {
        self.queue.senders.fetch_add(1, Ordering::Relaxed);
        Self {
            queue: self.queue.clone(),
        }
    }
}

impl Drop for CommandSender {
    fn drop(&mut self) {
        if self.queue.senders.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut inner = self.queue.inner.lock().unwrap();
            inner.closed = true;
            drop(inner);
            self.queue.notify.notify_waiters();
        }
    }
}

impl Drop for CommandReceiver {
    fn drop(&mut self) {
        let mut inner = self.queue.inner.lock().unwrap();
        inner.closed = true;
        drop(inner);
        self.queue.notify.notify_waiters();
    }
}

impl CommandSender {
    fn send(&self, command: Command) -> bool {
        let mut inner = self.queue.inner.lock().unwrap();
        if inner.closed {
            return false;
        }
        match command {
            Command::Input(Input::Move(at)) => {
                inner.pending_move = Some(at);
            }
            Command::Input(Input::Gamepad(state)) => {
                if inner.queue.len() >= GAMEPAD_SOFT_LIMIT {
                    return true;
                }
                inner.queue.push_back(Command::Input(Input::Gamepad(state)));
            }
            Command::Input(other) => {
                // Strictly preserve ordering: commit the latest pending move before any
                // click, scroll, key, or release event.
                if inner.queue.len() >= MAX_COMMAND_QUEUE {
                    return false;
                }
                if let Some(at) = inner.pending_move.take() {
                    inner.queue.push_back(Command::Input(Input::Move(at)));
                }
                inner.queue.push_back(Command::Input(other));
            }
            other => {
                if inner.queue.len() >= MAX_COMMAND_QUEUE {
                    return false;
                }
                inner.queue.push_back(other);
            }
        }
        drop(inner);
        self.queue.notify.notify_one();
        true
    }

    fn try_send_gamepad(&self, state: GamepadState) -> bool {
        let mut inner = self.queue.inner.lock().unwrap();
        if inner.closed {
            return true;
        }
        if inner.queue.len() >= GAMEPAD_SOFT_LIMIT {
            return false;
        }
        inner.queue.push_back(Command::Input(Input::Gamepad(state)));
        drop(inner);
        self.queue.notify.notify_one();
        true
    }
}

impl CommandReceiver {
    #[cfg(test)]
    fn try_recv(&self) -> Option<Command> {
        let mut inner = self.queue.inner.lock().unwrap();
        if let Some(cmd) = inner.queue.pop_front() {
            return Some(cmd);
        }
        inner
            .pending_move
            .take()
            .map(|at| Command::Input(Input::Move(at)))
    }

    async fn recv(&mut self) -> Option<Command> {
        loop {
            let notified = self.queue.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut inner = self.queue.inner.lock().unwrap();
            if let Some(cmd) = inner.queue.pop_front() {
                return Some(cmd);
            }
            if let Some(at) = inner.pending_move.take() {
                return Some(Command::Input(Input::Move(at)));
            }
            if inner.closed {
                return None;
            }
            drop(inner);
            notified.await;
        }
    }
}

struct DecodeQueueInner<T> {
    frames: VecDeque<(T, bool)>,
    closed: bool,
}

struct DecodeQueue<T> {
    inner: Mutex<DecodeQueueInner<T>>,
    available: Condvar,
    space: Notify,
    capacity: usize,
}

impl<T> DecodeQueue<T> {
    fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inner: Mutex::new(DecodeQueueInner {
                frames: VecDeque::with_capacity(capacity),
                closed: false,
            }),
            available: Condvar::new(),
            space: Notify::new(),
            capacity,
        }
    }

    fn has_space(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        !inner.closed && inner.frames.len() < self.capacity
    }

    async fn wait_space(&self) -> bool {
        loop {
            let notified = self.space.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let inner = self.inner.lock().unwrap();
            if inner.closed {
                return false;
            }
            if inner.frames.len() < self.capacity {
                return true;
            }
            drop(inner);
            notified.await;
        }
    }

    fn push(&self, item: T, random_access: bool, mode: FramePacingMode) -> usize {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            return 0;
        }
        let mut flushed = 0;
        if random_access && mode == FramePacingMode::LowestLatency && !inner.frames.is_empty() {
            flushed = inner.frames.len();
            inner.frames.clear();
        }
        inner.frames.push_back((item, random_access));
        drop(inner);
        self.available.notify_one();
        if flushed > 0 {
            self.space.notify_one();
        }
        flushed
    }

    fn pop_blocking(&self, mode: &AtomicU8) -> Option<T> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            if !inner.frames.is_empty() {
                let pacing = FramePacingMode::from_u8(mode.load(Ordering::Acquire));
                if pacing == FramePacingMode::LowestLatency
                    && inner.frames.len() > 1
                    && let Some(latest_kf) = inner.frames.iter().rposition(|(_, kf)| *kf)
                    && latest_kf > 0
                {
                    inner.frames.drain(..latest_kf);
                }
                let (item, _) = inner.frames.pop_front().unwrap();
                drop(inner);
                self.space.notify_one();
                return Some(item);
            }
            if inner.closed {
                return None;
            }
            inner = self.available.wait(inner).unwrap();
        }
    }

    fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        drop(inner);
        self.available.notify_all();
        self.space.notify_waiters();
    }
}

struct DecodeQueueCloseGuard<T>(Arc<DecodeQueue<T>>);

impl<T> Drop for DecodeQueueCloseGuard<T> {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct FramePacer<T> {
    queue: VecDeque<T>,
    last_release: Option<Instant>,
    cadence: Duration,
}

impl<T> FramePacer<T> {
    fn new(target_fps: u16) -> Self {
        let fps = u64::from(target_fps.clamp(1, 120));
        Self {
            queue: VecDeque::with_capacity(BALANCED_PACING_CAPACITY),
            last_release: None,
            cadence: Duration::from_nanos(1_000_000_000 / fps),
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        if self.queue.is_empty() {
            return None;
        }
        let last = self.last_release?;
        let step = if self.queue.len() >= BALANCED_PACING_CAPACITY {
            self.cadence.mul_f64(0.85)
        } else {
            self.cadence
        };
        Some(last + step)
    }

    fn push(&mut self, frame: T, mode: FramePacingMode, now: Instant) -> Option<T> {
        match mode {
            FramePacingMode::LowestLatency => {
                self.queue.clear();
                self.last_release = Some(now);
                Some(frame)
            }
            FramePacingMode::Balanced => {
                if self.queue.len() >= BALANCED_PACING_CAPACITY {
                    self.queue.pop_front();
                }
                self.queue.push_back(frame);
                self.poll_due(mode, now)
            }
        }
    }

    fn poll_due(&mut self, mode: FramePacingMode, now: Instant) -> Option<T> {
        if self.queue.is_empty() {
            return None;
        }
        match mode {
            FramePacingMode::LowestLatency => {
                let latest = self.queue.pop_back();
                self.queue.clear();
                if latest.is_some() {
                    self.last_release = Some(now);
                }
                latest
            }
            FramePacingMode::Balanced => {
                let due = match self.next_deadline() {
                    None => true,
                    Some(deadline) => now >= deadline,
                };
                if !due {
                    return None;
                }
                let frame = self.queue.pop_front()?;
                self.last_release = Some(match self.last_release {
                    Some(prev) => {
                        let nominal = prev + self.cadence;
                        if now >= nominal
                            && now.saturating_duration_since(nominal) <= self.cadence / 4
                        {
                            nominal
                        } else {
                            now
                        }
                    }
                    None => now,
                });
                Some(frame)
            }
        }
    }
}

fn is_random_access_frame(codec: Codec, bytes: &[u8]) -> bool {
    match codec {
        Codec::Av1 => obu::inspect_av1(bytes).unwrap_or(false),
        Codec::H264 => {
            let mut params = 0u8;
            let mut idr = false;
            for nal in nal_units(bytes) {
                match nal[0] & 0x1f {
                    7 => params |= 1,
                    8 => params |= 2,
                    5 => idr = true,
                    _ => {}
                }
            }
            idr && params == 3
        }
        Codec::Hevc => {
            let mut params = 0u8;
            let mut idr = false;
            for nal in nal_units(bytes) {
                match (nal[0] >> 1) & 0x3f {
                    32 => params |= 1,
                    33 => params |= 2,
                    34 => params |= 4,
                    19 | 20 => idr = true,
                    _ => {}
                }
            }
            idr && params == 7
        }
    }
}

type PresentedFrame = (VideoFrame, Instant, Instant, u32);

pub struct Connection {
    sink: CommandSender,
    pub events: Receiver<Event>,
    latest: Arc<Mutex<Option<PresentedFrame>>>,
    stats: Arc<Mutex<ViewerStats>>,
    telemetry: Arc<LiveViewerMetrics>,
    rtt_us: Arc<AtomicU64>,
    audio_muted: Arc<AtomicBool>,
    frame_pacing: Arc<AtomicU8>,
    pacing_notify: Arc<Notify>,
    connected_gamepads: Arc<AtomicU8>,
    _stop: oneshot::Sender<()>,
}

impl Connection {
    #[allow(dead_code)]
    pub fn start(
        address: SocketAddrV4,
        pairing: Pairing,
        preference: Option<DisplayPreference>,
        wake: Arc<Notify>,
        window: &gpui::Window,
    ) -> Result<Self> {
        Self::start_with_stream_preference(
            address,
            pairing,
            StreamPreference {
                display: preference,
                ..StreamPreference::default()
            },
            wake,
            window,
        )
    }

    pub fn start_with_stream_preference(
        address: SocketAddrV4,
        pairing: Pairing,
        mut stream_preference: StreamPreference,
        wake: Arc<Notify>,
        _window: &gpui::Window,
    ) -> Result<Self> {
        stream_preference.supported_codecs = codec::viewer_supported_codec_mask();
        stream_preference.hdr = stream_preference.hdr && codec::supports_hdr_decode();
        #[cfg(target_os = "windows")]
        let device = _window
            .d3d11_device()
            .ok_or_else(|| anyhow::anyhow!("The window has no Direct3D video device."))?;
        let (sink, mut commands) = command_channel();
        let gamepad_sink = sink.clone();
        let (events, receiver) = channel();
        let latest = Arc::new(Mutex::new(None));
        let slot = latest.clone();
        let stats = Arc::new(Mutex::new(ViewerStats::new()));
        let worker_stats = stats.clone();
        let telemetry = Arc::new(LiveViewerMetrics::new());
        let worker_telemetry = telemetry.clone();
        let rtt_us = Arc::new(AtomicU64::new(0));
        let worker_rtt = rtt_us.clone();
        let audio_muted = Arc::new(AtomicBool::new(false));
        let worker_audio_muted = audio_muted.clone();
        let frame_pacing = Arc::new(AtomicU8::new(FramePacingMode::default().as_u8()));
        let worker_frame_pacing = frame_pacing.clone();
        let pacing_notify = Arc::new(Notify::new());
        let worker_pacing_notify = pacing_notify.clone();
        let connected_gamepads = Arc::new(AtomicU8::new(0));
        let worker_gamepads = connected_gamepads.clone();
        let (stop, mut stopped) = oneshot::channel();
        thread::Builder::new()
            .name("lan-viewer".into())
            .spawn(move || {
                let result = (|| {
                    Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(async {
                            let mut session = tokio::select! {
                                _ = &mut stopped => return Ok(()),
                                session = ViewerSession::connect_with_stream_preference(
                                    address,
                                    pairing,
                                    stream_preference,
                                ) => session?,
                            };
                            let can_control = session.can_control();
                            let _ = events.send(Event::Ready(can_control));
                            wake.notify_one();
                            let _gamepad_reader = can_control
                                .then(|| {
                                    let count_wake = wake.clone();
                                    crate::gamepad::GamepadReader::start(
                                        worker_gamepads.clone(),
                                        move |_| count_wake.notify_one(),
                                        move |state| gamepad_sink.try_send_gamepad(state),
                                    )
                                })
                                .transpose()?;
                            let result = tokio::select! {
                                _ = &mut stopped => Ok(()),
                                result = run(
                                    &mut session,
                                    &mut commands,
                                    &events,
                                    slot,
                                    wake.clone(),
                                    worker_stats,
                                    worker_telemetry,
                                    worker_rtt,
                                    worker_audio_muted,
                                    worker_frame_pacing,
                                    worker_pacing_notify,
                                    #[cfg(target_os = "windows")]
                                    device,
                                ) => result,
                            };
                            drop(_gamepad_reader);
                            session.disconnect().await;
                            result
                        })
                })();
                let _ = events.send(Event::Closed(result));
                wake.notify_one();
            })?;
        Ok(Self {
            sink,
            events: receiver,
            latest,
            stats,
            telemetry,
            rtt_us,
            audio_muted,
            frame_pacing,
            pacing_notify,
            connected_gamepads,
            _stop: stop,
        })
    }

    #[allow(dead_code)]
    pub fn connected_gamepads(&self) -> u8 {
        self.connected_gamepads.load(Ordering::Acquire)
    }

    pub fn set_audio_muted(&self, muted: bool) {
        self.audio_muted.store(muted, Ordering::Release);
    }

    pub fn set_frame_pacing(&self, mode: FramePacingMode) {
        self.frame_pacing.store(mode.as_u8(), Ordering::Release);
        self.pacing_notify.notify_one();
    }

    #[allow(dead_code)]
    pub fn frame_pacing(&self) -> FramePacingMode {
        FramePacingMode::from_u8(self.frame_pacing.load(Ordering::Acquire))
    }

    pub fn take_frame(&self) -> Option<VideoFrame> {
        let (frame, received_at, decoded_at, host_latency_us) =
            self.latest.lock().unwrap().take()?;
        let rtt = Duration::from_micros(self.rtt_us.load(Ordering::Relaxed));
        self.stats
            .lock()
            .unwrap()
            .record_present(decoded_at.elapsed(), rtt);
        self.telemetry
            .record_e2e(host_latency_us, rtt, received_at.elapsed());
        self.telemetry.record_present();
        Some(frame)
    }

    pub fn telemetry(&self) -> SessionTelemetry {
        self.telemetry.snapshot()
    }

    #[must_use]
    pub fn input(&self, event: Input) -> bool {
        self.sink.send(Command::Input(event))
    }

    pub fn send_clipboard(&self, data: ClipboardData) {
        let _ = self.sink.send(Command::Clipboard(data));
    }

    pub fn send_files(&self, files: Vec<(String, PathBuf)>) {
        let _ = self.sink.send(Command::SendFiles(files));
    }

    #[allow(dead_code)]
    pub fn set_bitrate_mbps(&self, mbps: Option<u16>) {
        let _ = self
            .sink
            .send(Command::SetBitrate(mbps.map(|m| m.clamp(5, 150))));
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    session: &mut ViewerSession,
    commands: &mut CommandReceiver,
    events: &Sender<Event>,
    slot: Arc<Mutex<Option<PresentedFrame>>>,
    wake: Arc<Notify>,
    stats: Arc<Mutex<ViewerStats>>,
    telemetry: Arc<LiveViewerMetrics>,
    rtt_us: Arc<AtomicU64>,
    audio_muted: Arc<AtomicBool>,
    frame_pacing: Arc<AtomicU8>,
    pacing_notify: Arc<Notify>,
    #[cfg(target_os = "windows")] device: gpui::ID3D11Device,
) -> Result<()> {
    let format = session.format();
    telemetry.set_format(format);
    let initial_transport = session.transport_stats();
    rtt_us.store(initial_transport.rtt.as_micros() as u64, Ordering::Relaxed);
    telemetry.record_transport(initial_transport);
    let audio_task = session.take_audio_receiver().map(|mut audio_rx| {
        let player = Arc::new(crate::audio::AudioPlayer::new(audio_muted));
        tokio::spawn(async move {
            while let Some(frame) = audio_rx.recv().await {
                player.push_frame(&frame);
            }
        })
    });
    let mut aux_rx = session
        .take_aux_receiver()
        .context("Missing viewer auxiliary receiver.")?;
    let mut aux_open = true;
    let aux_tx = session.aux_sender();
    let decode_queue = Arc::new(DecodeQueue::<(ReceivedFrame, Instant)>::new(
        DECODE_QUEUE_CAPACITY,
    ));
    let pacer = Arc::new(Mutex::new(FramePacer::<PresentedFrame>::new(
        format.frames_per_second,
    )));
    let pacer_task = {
        let pacer = pacer.clone();
        let pacing_mode = frame_pacing.clone();
        let pacing_notify = pacing_notify.clone();
        let pacer_slot = slot.clone();
        let pacer_wake = wake.clone();
        tokio::spawn(async move {
            loop {
                let notified = pacing_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let (due_frame, next_deadline) = {
                    let mode = FramePacingMode::from_u8(pacing_mode.load(Ordering::Acquire));
                    let mut guard = pacer.lock().unwrap();
                    let frame = guard.poll_due(mode, Instant::now());
                    let next = guard.next_deadline();
                    (frame, next)
                };
                if let Some(frame) = due_frame {
                    *pacer_slot.lock().unwrap() = Some(frame);
                    pacer_wake.notify_one();
                    continue;
                }
                match next_deadline {
                    Some(deadline) => {
                        tokio::select! {
                            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {}
                            _ = notified => {}
                        }
                    }
                    None => {
                        notified.await;
                    }
                }
            }
        })
    };

    let decoder_queue = decode_queue.clone();
    let decode_slot = slot;
    let decode_wake = wake.clone();
    let decode_stats = stats;
    let decode_telemetry = telemetry.clone();
    let decode_pacing = frame_pacing.clone();
    let decode_pacer = pacer;
    let decode_pacing_notify = pacing_notify;
    let decoder_thread =
        thread::Builder::new()
            .name("video-decoder".into())
            .spawn(move || -> Result<()> {
                let _close_guard = DecodeQueueCloseGuard(decoder_queue.clone());
                #[cfg(target_os = "macos")]
                let mut decoder = Decoder::with_format(format);
                #[cfg(target_os = "windows")]
                let mut decoder = Decoder::with_format(device, format);
                #[cfg(target_os = "linux")]
                let mut decoder = Decoder::with_format(format);
                while let Some((frame, received_at)) = decoder_queue.pop_blocking(&decode_pacing) {
                    let host_latency_us = frame.host_latency_us;
                    let started = Instant::now();
                    let decoded = decoder.decode(&frame)?;
                    let elapsed = started.elapsed();
                    decode_telemetry.record_decode(elapsed);
                    if let Some(frame) = decoded {
                        let now = Instant::now();
                        let presented = (frame, received_at, now, host_latency_us);
                        let mode = FramePacingMode::from_u8(decode_pacing.load(Ordering::Acquire));
                        let ready_now = decode_pacer.lock().unwrap().push(presented, mode, now);
                        if let Some(frame) = ready_now {
                            *decode_slot.lock().unwrap() = Some(frame);
                            decode_wake.notify_one();
                        } else {
                            decode_pacing_notify.notify_one();
                        }
                        decode_stats.lock().unwrap().record_decode(elapsed, true);
                    } else {
                        decode_stats.lock().unwrap().record_decode(elapsed, false);
                    }
                }
                Ok(())
            })?;

    let mut stats_tick = tokio::time::interval(Duration::from_secs(1));
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_cursor = CursorShape::Arrow;
    let loop_result: Result<()> = loop {
        let can_recv_video = decode_queue.has_space();
        tokio::select! {
            biased;
            command = commands.recv() => {
                match command {
                    Some(Command::Input(input)) => {
                        if let Err(error) = session.input(input).await {
                            break Err(error);
                        }
                    }
                    Some(Command::SetBitrate(mbps)) => {
                        if let Err(error) = session.set_bitrate_mbps(mbps).await {
                            break Err(error);
                        }
                    }
                    Some(Command::Clipboard(data)) => {
                        let aux = aux_tx.clone();
                        tokio::spawn(async move {
                            let _ = aux.send_clipboard(&data).await;
                        });
                    }
                    Some(Command::SendFiles(files)) => {
                        let aux = aux_tx.clone();
                        let events = events.clone();
                        let wake = wake.clone();
                        tokio::spawn(async move {
                            let total_files = files.len();
                            let outcome: Result<()> = async {
                                for (index, (rel, src)) in files.into_iter().enumerate() {
                                    let label = rel
                                        .rsplit('/')
                                        .next()
                                        .unwrap_or(&rel)
                                        .to_string();
                                    let mut last_update = Instant::now() - Duration::from_secs(1);
                                    aux.send_file(&rel, &src, |sent, total| {
                                        if sent == total
                                            || last_update.elapsed() >= Duration::from_millis(120)
                                        {
                                            last_update = Instant::now();
                                            let pct = if total == 0 {
                                                100
                                            } else {
                                                ((sent * 100) / total).min(100)
                                            };
                                            let _ = events.send(Event::Transfer {
                                                message: format!(
                                                    "Sending {label} ({}/{}, {pct}%)…",
                                                    index + 1,
                                                    total_files
                                                ),
                                                error: false,
                                                active: true,
                                            });
                                            wake.notify_one();
                                        }
                                    })
                                    .await?;
                                }
                                Ok(())
                            }
                            .await;
                            let notice = match outcome {
                                Ok(()) => Event::Transfer {
                                    message: if total_files == 1 {
                                        "Sent 1 file to remote Downloads.".into()
                                    } else {
                                        format!("Sent {total_files} files to remote Downloads.")
                                    },
                                    error: false,
                                    active: false,
                                },
                                Err(error) => Event::Transfer {
                                    message: format!("Transfer failed: {error:#}"),
                                    error: true,
                                    active: false,
                                },
                            };
                            let _ = events.send(notice);
                            wake.notify_one();
                        });
                    }
                    None => break Ok(()),
                }
            }
            aux = aux_rx.recv(), if aux_open => {
                match aux {
                    Some(SessionAuxiliary::Clipboard(data)) => {
                        let _ = events.send(Event::Clipboard(data));
                        wake.notify_one();
                    }
                    Some(SessionAuxiliary::Cursor(update)) => {
                        if update.shape != last_cursor {
                            last_cursor = update.shape;
                            let _ = events.send(Event::Cursor(update.shape));
                            wake.notify_one();
                        }
                    }
                    Some(SessionAuxiliary::FileReceived(_)) => {}
                    None => aux_open = false,
                }
            }
            has_space = decode_queue.wait_space(), if !can_recv_video => {
                if !has_space {
                    break Ok(());
                }
            }
            _ = stats_tick.tick() => {
                let transport = session.transport_stats();
                rtt_us.store(transport.rtt.as_micros() as u64, Ordering::Relaxed);
                telemetry.record_transport(transport);
            }
            frame = session.recv(), if can_recv_video => {
                match frame {
                    Ok(Some(frame)) => {
                        telemetry.record_video_frame(frame.bytes.len());
                        let random_access = is_random_access_frame(frame.codec, &frame.bytes);
                        let mode = FramePacingMode::from_u8(frame_pacing.load(Ordering::Acquire));
                        decode_queue.push((frame, Instant::now()), random_access, mode);
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(error),
                }
            }
        }
    };
    decode_queue.close();
    pacer_task.abort();
    let _ = pacer_task.await;
    if let Some(task) = audio_task {
        task.abort();
        let _ = task.await;
    }
    let decode_result = decoder_thread
        .join()
        .unwrap_or_else(|_| Err(anyhow::anyhow!("The video decoder panicked.")));
    loop_result.and(decode_result)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::AtomicU8,
        time::{Duration, Instant},
    };

    use remoters_core::{Button, GamepadState, Input, Point, video::Codec};

    use super::{
        Command, DecodeQueue, FramePacer, FramePacingMode, command_channel, is_random_access_frame,
    };

    fn pt(x: u16, y: u16) -> Point {
        Point { x, y }
    }

    #[test]
    fn move_coalescing_collapses_high_rate_moves_without_flooding() {
        let (tx, rx) = command_channel();
        for i in 0..1000u16 {
            assert!(tx.send(Command::Input(Input::Move(pt(i, i.wrapping_mul(2))))));
        }
        let first = rx.try_recv().expect("latest move should be present");
        match first {
            Command::Input(Input::Move(at)) => assert_eq!(at, pt(999, 1998)),
            other => panic!("expected coalesced Move, got {other:?}"),
        }
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn move_coalescing_preserves_exact_ordering_around_clicks_keys_and_scroll() {
        let (tx, rx) = command_channel();
        // High-frequency moves before left button down
        assert!(tx.send(Command::Input(Input::Move(pt(10, 10)))));
        assert!(tx.send(Command::Input(Input::Move(pt(20, 20)))));
        assert!(tx.send(Command::Input(Input::Move(pt(30, 30)))));
        assert!(tx.send(Command::Input(Input::Button {
            at: pt(30, 30),
            button: Button::Left,
            down: true,
        })));
        // Drag moves before button up
        assert!(tx.send(Command::Input(Input::Move(pt(40, 40)))));
        assert!(tx.send(Command::Input(Input::Move(pt(50, 50)))));
        assert!(tx.send(Command::Input(Input::Button {
            at: pt(50, 50),
            button: Button::Left,
            down: false,
        })));
        // Moves before key & scroll
        assert!(tx.send(Command::Input(Input::Move(pt(60, 60)))));
        assert!(tx.send(Command::Input(Input::Move(pt(70, 70)))));
        assert!(tx.send(Command::Input(Input::Key {
            code: 0x41,
            down: true,
        })));
        assert!(tx.send(Command::Input(Input::Move(pt(80, 80)))));
        assert!(tx.send(Command::Input(Input::Scroll {
            at: pt(80, 80),
            vertical: -120,
            horizontal: 0,
        })));
        // Trailing moves with no following click
        assert!(tx.send(Command::Input(Input::Move(pt(90, 90)))));
        assert!(tx.send(Command::Input(Input::Move(pt(100, 100)))));

        let mut received = Vec::new();
        while let Some(Command::Input(input)) = rx.try_recv() {
            received.push(input);
        }

        assert_eq!(
            received,
            vec![
                Input::Move(pt(30, 30)),
                Input::Button {
                    at: pt(30, 30),
                    button: Button::Left,
                    down: true,
                },
                Input::Move(pt(50, 50)),
                Input::Button {
                    at: pt(50, 50),
                    button: Button::Left,
                    down: false,
                },
                Input::Move(pt(70, 70)),
                Input::Key {
                    code: 0x41,
                    down: true,
                },
                Input::Move(pt(80, 80)),
                Input::Scroll {
                    at: pt(80, 80),
                    vertical: -120,
                    horizontal: 0,
                },
                Input::Move(pt(100, 100)),
            ]
        );
    }

    #[test]
    fn move_coalescing_never_drops_buttons_under_1000hz_mouse_and_gamepad_load() {
        let (tx, rx) = command_channel();
        for i in 0..500u16 {
            assert!(tx.send(Command::Input(Input::Move(pt(i, i)))));
        }
        assert!(tx.try_send_gamepad(GamepadState {
            slot: 0,
            connected: true,
            buttons: GamepadState::A,
            ..GamepadState::default()
        }));
        for i in 500..1000u16 {
            assert!(tx.send(Command::Input(Input::Move(pt(i, i)))));
        }
        assert!(tx.send(Command::Input(Input::Button {
            at: pt(999, 999),
            button: Button::Right,
            down: true,
        })));

        let mut received = Vec::new();
        while let Some(Command::Input(input)) = rx.try_recv() {
            received.push(input);
        }

        // Gamepad does not fragment mouse moves; final move precedes button press.
        assert_eq!(received.len(), 3);
        assert!(matches!(received[0], Input::Gamepad(_)));
        assert_eq!(received[1], Input::Move(pt(999, 999)));
        assert_eq!(
            received[2],
            Input::Button {
                at: pt(999, 999),
                button: Button::Right,
                down: true,
            }
        );
    }

    #[test]
    fn decode_queue_flushes_obsolete_frames_on_keyframe_in_lowest_latency() {
        let queue = DecodeQueue::new(2);
        let mode_atomic = AtomicU8::new(FramePacingMode::LowestLatency.as_u8());

        assert_eq!(queue.push(1u32, false, FramePacingMode::LowestLatency), 0);
        assert_eq!(queue.push(2u32, false, FramePacingMode::LowestLatency), 0);
        assert!(!queue.has_space());

        // Keyframe arrival flushes both obsolete P-frames immediately.
        assert_eq!(queue.push(3u32, true, FramePacingMode::LowestLatency), 2);
        assert!(queue.has_space());
        assert_eq!(queue.pop_blocking(&mode_atomic), Some(3));
    }

    #[test]
    fn frame_pacer_releases_immediately_in_lowest_latency_and_smoothly_in_balanced() {
        let t0 = Instant::now();
        let mut pacer = FramePacer::new(60);

        // LowestLatency always presents immediately and discards stale buffered frames.
        assert_eq!(
            pacer.push(1u32, FramePacingMode::LowestLatency, t0),
            Some(1)
        );
        assert_eq!(
            pacer.push(
                2u32,
                FramePacingMode::LowestLatency,
                t0 + Duration::from_millis(2)
            ),
            Some(2)
        );

        // Balanced mode absorbs a tight jitter burst and paces on ~16.67ms cadence.
        let mut balanced = FramePacer::new(60);
        assert_eq!(
            balanced.push(10u32, FramePacingMode::Balanced, t0),
            Some(10)
        );
        // Second frame arrives only 2ms later -> held until ~16.67ms.
        assert_eq!(
            balanced.push(
                11u32,
                FramePacingMode::Balanced,
                t0 + Duration::from_millis(2)
            ),
            None
        );
        assert_eq!(
            balanced.poll_due(FramePacingMode::Balanced, t0 + Duration::from_millis(10)),
            None
        );
        assert_eq!(
            balanced.poll_due(FramePacingMode::Balanced, t0 + Duration::from_millis(17)),
            Some(11)
        );

        // Burst of 3 frames stays bounded at 2 frames, evicting oldest.
        let base = t0 + Duration::from_millis(17);
        assert_eq!(
            balanced.push(
                12u32,
                FramePacingMode::Balanced,
                base + Duration::from_millis(1)
            ),
            None
        );
        assert_eq!(
            balanced.push(
                13u32,
                FramePacingMode::Balanced,
                base + Duration::from_millis(2)
            ),
            None
        );
        assert_eq!(
            balanced.push(
                14u32,
                FramePacingMode::Balanced,
                base + Duration::from_millis(3)
            ),
            None
        );
        // Switching to LowestLatency flushes directly to latest (14).
        assert_eq!(
            balanced.poll_due(
                FramePacingMode::LowestLatency,
                base + Duration::from_millis(4)
            ),
            Some(14)
        );
        assert_eq!(
            balanced.poll_due(
                FramePacingMode::LowestLatency,
                base + Duration::from_millis(5)
            ),
            None
        );
    }

    #[test]
    fn random_access_detection_identifies_h264_and_hevc_keyframes() {
        let h264_idr = [
            0, 0, 0, 1, 0x67, 0x64, // SPS
            0, 0, 0, 1, 0x68, 0xee, // PPS
            0, 0, 0, 1, 0x65, 0x88, // IDR
        ];
        let h264_p = [0, 0, 0, 1, 0x41, 0x9a];
        assert!(is_random_access_frame(Codec::H264, &h264_idr));
        assert!(!is_random_access_frame(Codec::H264, &h264_p));

        let hevc_idr = [
            0,
            0,
            0,
            1,
            32 << 1,
            1, // VPS
            0,
            0,
            0,
            1,
            33 << 1,
            1, // SPS
            0,
            0,
            0,
            1,
            34 << 1,
            1, // PPS
            0,
            0,
            0,
            1,
            19 << 1,
            1, // IDR_W_RADL
        ];
        let hevc_p = [0, 0, 0, 1, 1 << 1, 1];
        assert!(is_random_access_frame(Codec::Hevc, &hevc_idr));
        assert!(!is_random_access_frame(Codec::Hevc, &hevc_p));
    }
}
