use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JClass, JObject, JString},
    sys::{jboolean, jint, jlong},
};
use remoters_core::{
    Input, Point,
    network::{self, Pairing},
    session::{StreamPreference, ViewerSession},
};
use tokio::sync::{Notify, oneshot};

use crate::{
    input::{
        build_stream_preference, clamp_point, map_bitrate_mbps, map_gamepad_input, map_key_input,
        map_pointer_button, map_scroll_input,
    },
    jni_bridge::{
        call_on_audio_pcm, call_on_closed, call_on_ready, call_on_stats_json, call_on_video_frame,
        read_jstring,
    },
    types::StatsAccumulator,
};

const STATS_INTERVAL: Duration = Duration::from_millis(500);
const MAX_COMMAND_QUEUE: usize = 512;
const GAMEPAD_SOFT_LIMIT: usize = 96;

static SESSIONS: OnceLock<Mutex<HashMap<jlong, Arc<SessionController>>>> = OnceLock::new();
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn sessions() -> &'static Mutex<HashMap<jlong, Arc<SessionController>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Eq, PartialEq)]
enum SessionCommand {
    Input(Input),
    SetBitrate(Option<u16>),
}

struct CommandQueueInner {
    queue: VecDeque<SessionCommand>,
    pending_move: Option<Point>,
    closed: bool,
}

struct CommandQueue {
    inner: Mutex<CommandQueueInner>,
    notify: Notify,
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
    });
    (
        CommandSender {
            queue: queue.clone(),
        },
        CommandReceiver { queue },
    )
}

impl Drop for CommandSender {
    fn drop(&mut self) {
        let mut inner = self
            .queue
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.closed = true;
        drop(inner);
        self.queue.notify.notify_waiters();
    }
}

impl Drop for CommandReceiver {
    fn drop(&mut self) {
        let mut inner = self
            .queue
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.closed = true;
        drop(inner);
        self.queue.notify.notify_waiters();
    }
}

impl CommandSender {
    fn send(&self, command: SessionCommand) -> bool {
        let mut inner = self
            .queue
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.closed {
            return false;
        }
        match command {
            SessionCommand::Input(Input::Move(at)) => {
                // O(1) move coalescing: 120/240Hz touch/pointer updates never consume queue capacity.
                inner.pending_move = Some(at);
            }
            SessionCommand::Input(Input::Gamepad(state)) => {
                // Soft-drop high-rate gamepad updates under congestion without fragmenting pending moves.
                if inner.queue.len() >= GAMEPAD_SOFT_LIMIT {
                    return true;
                }
                inner
                    .queue
                    .push_back(SessionCommand::Input(Input::Gamepad(state)));
            }
            SessionCommand::Input(other) => {
                // Strictly preserve ordering: flush the latest pending move right before any
                // button, key, scroll, or release event.
                if inner.queue.len() >= MAX_COMMAND_QUEUE {
                    return false;
                }
                if let Some(at) = inner.pending_move.take() {
                    inner
                        .queue
                        .push_back(SessionCommand::Input(Input::Move(at)));
                }
                inner.queue.push_back(SessionCommand::Input(other));
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
}

impl CommandReceiver {
    #[cfg(test)]
    fn try_recv(&self) -> Option<SessionCommand> {
        let mut inner = self
            .queue
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cmd) = inner.queue.pop_front() {
            return Some(cmd);
        }
        inner
            .pending_move
            .take()
            .map(|at| SessionCommand::Input(Input::Move(at)))
    }

    async fn recv(&mut self) -> Option<SessionCommand> {
        loop {
            let notified = self.queue.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut inner = self
                    .queue
                    .inner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(cmd) = inner.queue.pop_front() {
                    return Some(cmd);
                }
                if let Some(at) = inner.pending_move.take() {
                    return Some(SessionCommand::Input(Input::Move(at)));
                }
                if inner.closed {
                    return None;
                }
            }
            notified.await;
        }
    }
}

struct SessionController {
    commands: CommandSender,
    audio_muted: Arc<AtomicBool>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
}

impl SessionController {
    fn send_command(&self, command: SessionCommand) {
        let _ = self.commands.send(command);
    }

    fn set_audio_muted(&self, muted: bool) {
        self.audio_muted.store(muted, Ordering::Relaxed);
    }

    fn stop(&self) {
        if let Ok(mut guard) = self.stop.lock()
            && let Some(tx) = guard.take()
        {
            let _ = tx.send(());
        }
    }
}

fn with_session(handle: jlong, f: impl FnOnce(&SessionController)) {
    if handle <= 0 {
        return;
    }
    let controller = sessions()
        .lock()
        .ok()
        .and_then(|map| map.get(&handle).cloned());
    if let Some(controller) = controller {
        f(&controller);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_session_worker(
    jvm: JavaVM,
    callback: GlobalRef,
    address: std::net::SocketAddrV4,
    pairing: Pairing,
    preference: StreamPreference,
    mut commands: CommandReceiver,
    audio_muted: Arc<AtomicBool>,
    mut stop_rx: oneshot::Receiver<()>,
    handle: jlong,
) {
    let Ok(mut env) = jvm.attach_current_thread() else {
        let _ = sessions().lock().map(|mut map| map.remove(&handle));
        return;
    };

    let outcome: Result<()> = (|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("Failed to create session Tokio runtime.")?;

        rt.block_on(async {
            let mut session = tokio::select! {
                _ = &mut stop_rx => return Ok(()),
                result = ViewerSession::connect_with_stream_preference(address, pairing, preference) => result?,
            };

            let format = session.format();
            call_on_ready(&mut env, callback.as_obj(), !session.can_control(), format.hdr);
            let mut audio_rx = session.take_audio_receiver();
            let mut audio_open = audio_rx.is_some();
            let mut stats = StatsAccumulator::new(format);
            let mut stats_tick = tokio::time::interval(STATS_INTERVAL);
            stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let loop_result: Result<()> = loop {
                tokio::select! {
                    biased;
                    _ = &mut stop_rx => break Ok(()),
                    command = commands.recv() => {
                        match command {
                            Some(SessionCommand::Input(input)) => {
                                if let Err(error) = session.input(input).await {
                                    break Err(error);
                                }
                            }
                            Some(SessionCommand::SetBitrate(mbps)) => {
                                if let Err(error) = session.set_bitrate_mbps(mbps).await {
                                    break Err(error);
                                }
                            }
                            None => break Ok(()),
                        }
                    }
                    audio = async {
                        match audio_rx.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    }, if audio_open => {
                        match audio {
                            Some(frame) => {
                                if !audio_muted.load(Ordering::Relaxed) {
                                    call_on_audio_pcm(
                                        &mut env,
                                        callback.as_obj(),
                                        frame.sample_rate,
                                        frame.channels,
                                        &frame.pcm,
                                    );
                                }
                            }
                            None => audio_open = false,
                        }
                    }
                    _ = stats_tick.tick() => {
                        let transport = session.transport_stats();
                        let json = stats.flush_json(Instant::now(), transport.rtt, transport.lost_packets);
                        call_on_stats_json(&mut env, callback.as_obj(), &json);
                    }
                    frame = session.recv() => {
                        match frame {
                            Ok(Some(frame)) => {
                                let rtt = session.rtt();
                                stats.record_frame(frame.bytes.len(), frame.host_latency_us, rtt);
                                call_on_video_frame(&mut env, callback.as_obj(), format, &frame);
                            }
                            Ok(None) => break Ok(()),
                            Err(error) => break Err(error),
                        }
                    }
                }
            };

            session.disconnect().await;
            loop_result
        })
    })();

    let _ = sessions().lock().map(|mut map| map.remove(&handle));

    match outcome {
        Ok(()) => call_on_closed(&mut env, callback.as_obj(), None),
        Err(error) => {
            let msg = format!("{error:#}");
            call_on_closed(&mut env, callback.as_obj(), Some(&msg));
        }
    }
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionStart<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    address: JString<'local>,
    code: JString<'local>,
    width: jint,
    height: jint,
    codec_pref: jint,
    supported_codecs: jint,
    hdr: jboolean,
    max_fps: jint,
    bitrate_mbps: jint,
    callback: JObject<'local>,
) -> jlong {
    if callback.is_null() {
        return 0;
    }

    let setup: Result<(
        std::net::SocketAddrV4,
        Pairing,
        StreamPreference,
        JavaVM,
        GlobalRef,
    )> = (|| {
        let address_str = read_jstring(&mut env, &address)?;
        let code_str = read_jstring(&mut env, &code)?;
        let addr = network::endpoint(&address_str)?;
        let pairing = Pairing::parse(&code_str)?;
        let preference = build_stream_preference(
            width,
            height,
            codec_pref,
            supported_codecs,
            hdr != 0,
            max_fps,
            bitrate_mbps,
        )?;
        let jvm = env.get_java_vm().context("Failed to acquire JavaVM.")?;
        let global_cb = env
            .new_global_ref(&callback)
            .context("Failed to retain SessionCallback.")?;
        Ok((addr, pairing, preference, jvm, global_cb))
    })();

    let (addr, pairing, preference, jvm, global_cb) = match setup {
        Ok(parsed) => parsed,
        Err(error) => {
            let msg = format!("{error:#}");
            call_on_closed(&mut env, &callback, Some(&msg));
            return 0;
        }
    };

    let (cmd_tx, cmd_rx) = command_channel();
    let audio_muted = Arc::new(AtomicBool::new(false));
    let (stop_tx, stop_rx) = oneshot::channel();
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed).max(1);
    let controller = Arc::new(SessionController {
        commands: cmd_tx,
        audio_muted: audio_muted.clone(),
        stop: Mutex::new(Some(stop_tx)),
    });

    if let Ok(mut map) = sessions().lock() {
        map.insert(handle, controller);
    }

    let worker_cb = global_cb.clone();
    let spawn_result = thread::Builder::new()
        .name("android-viewer".into())
        .spawn(move || {
            run_session_worker(
                jvm,
                worker_cb,
                addr,
                pairing,
                preference,
                cmd_rx,
                audio_muted,
                stop_rx,
                handle,
            );
        });

    if let Err(error) = spawn_result {
        let _ = sessions().lock().map(|mut map| map.remove(&handle));
        let msg = format!("Failed to start viewer thread: {error}");
        call_on_closed(&mut env, &callback, Some(&msg));
        return 0;
    }

    handle
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionSendPointerMove<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    x: jint,
    y: jint,
) {
    let point = clamp_point(x, y);
    with_session(handle, |controller| {
        controller.send_command(SessionCommand::Input(Input::Move(point)));
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionSendPointerButton<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    x: jint,
    y: jint,
    button: jint,
    down: jboolean,
) {
    let Some(button) = map_pointer_button(button) else {
        return;
    };
    let at = clamp_point(x, y);
    with_session(handle, |controller| {
        controller.send_command(SessionCommand::Input(Input::Button {
            at,
            button,
            down: down != 0,
        }));
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionSendScroll<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    x: jint,
    y: jint,
    vertical: jint,
    horizontal: jint,
) {
    let input = map_scroll_input(x, y, vertical, horizontal);
    with_session(handle, |controller| {
        controller.send_command(SessionCommand::Input(input));
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionSendKey<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    vk_code: jint,
    down: jboolean,
) {
    let Some(input) = map_key_input(vk_code, down != 0) else {
        return;
    };
    with_session(handle, |controller| {
        controller.send_command(SessionCommand::Input(input));
    });
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionSendGamepad<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    slot: jint,
    connected: jboolean,
    buttons: jint,
    lt: jint,
    rt: jint,
    lx: jint,
    ly: jint,
    rx: jint,
    ry: jint,
) {
    let Some(input) = map_gamepad_input(slot, connected != 0, buttons, lt, rt, lx, ly, rx, ry)
    else {
        return;
    };
    with_session(handle, |controller| {
        controller.send_command(SessionCommand::Input(input));
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionSetBitrate<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    bitrate_mbps: jint,
) {
    let mbps = map_bitrate_mbps(bitrate_mbps);
    with_session(handle, |controller| {
        controller.send_command(SessionCommand::SetBitrate(mbps));
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionSetAudioMuted<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    muted: jboolean,
) {
    with_session(handle, |controller| {
        controller.set_audio_muted(muted != 0);
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionReleaseAll<
    'local,
>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    with_session(handle, |controller| {
        controller.send_command(SessionCommand::Input(Input::Release));
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_sessionStop<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    if handle <= 0 {
        return;
    }
    let controller = sessions()
        .lock()
        .ok()
        .and_then(|mut map| map.remove(&handle));
    if let Some(controller) = controller {
        controller.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoters_core::{Button, GamepadState};

    fn pt(x: u16, y: u16) -> Point {
        Point { x, y }
    }

    #[test]
    fn command_queue_coalesces_high_hz_moves_and_preserves_event_ordering() {
        let (tx, rx) = command_channel();

        // Simulate 240Hz touch/pointer drag before a button press.
        for i in 0..1_000u16 {
            assert!(tx.send(SessionCommand::Input(Input::Move(pt(i, i + 1)))));
        }
        assert!(tx.send(SessionCommand::Input(Input::Button {
            at: pt(999, 1000),
            button: Button::Left,
            down: true,
        })));

        // More moves before a key event.
        for i in 10..500u16 {
            assert!(tx.send(SessionCommand::Input(Input::Move(pt(i, 20)))));
        }
        assert!(tx.send(SessionCommand::Input(Input::Key {
            code: 65,
            down: true,
        })));

        // Moves before scroll and release, plus a trailing move.
        assert!(tx.send(SessionCommand::Input(Input::Move(pt(1, 2)))));
        assert!(tx.send(SessionCommand::Input(Input::Move(pt(3, 4)))));
        assert!(tx.send(SessionCommand::Input(Input::Scroll {
            at: pt(3, 4),
            vertical: 120,
            horizontal: 0,
        })));
        assert!(tx.send(SessionCommand::Input(Input::Move(pt(5, 6)))));
        assert!(tx.send(SessionCommand::Input(Input::Release)));
        assert!(tx.send(SessionCommand::Input(Input::Move(pt(7, 8)))));
        assert!(tx.send(SessionCommand::Input(Input::Move(pt(9, 10)))));

        let mut drained = Vec::new();
        while let Some(cmd) = rx.try_recv() {
            drained.push(cmd);
        }

        assert_eq!(
            drained,
            vec![
                SessionCommand::Input(Input::Move(pt(999, 1000))),
                SessionCommand::Input(Input::Button {
                    at: pt(999, 1000),
                    button: Button::Left,
                    down: true,
                }),
                SessionCommand::Input(Input::Move(pt(499, 20))),
                SessionCommand::Input(Input::Key {
                    code: 65,
                    down: true,
                }),
                SessionCommand::Input(Input::Move(pt(3, 4))),
                SessionCommand::Input(Input::Scroll {
                    at: pt(3, 4),
                    vertical: 120,
                    horizontal: 0,
                }),
                SessionCommand::Input(Input::Move(pt(5, 6))),
                SessionCommand::Input(Input::Release),
                SessionCommand::Input(Input::Move(pt(9, 10))),
            ]
        );
    }

    #[test]
    fn command_queue_gamepad_soft_drops_without_breaking_move_coalescing() {
        let (tx, rx) = command_channel();

        for i in 0..200u16 {
            assert!(tx.send(SessionCommand::Input(Input::Move(pt(i, i)))));
        }

        // Flood gamepad state past GAMEPAD_SOFT_LIMIT.
        for i in 0..(GAMEPAD_SOFT_LIMIT + 40) {
            assert!(tx.send(SessionCommand::Input(Input::Gamepad(GamepadState {
                slot: 0,
                connected: true,
                buttons: i as u16,
                ..GamepadState::default()
            }))));
        }

        // Subsequent moves overwrite pending_move without fragmentation; Button still succeeds.
        for i in 200..400u16 {
            assert!(tx.send(SessionCommand::Input(Input::Move(pt(i, i)))));
        }
        assert!(tx.send(SessionCommand::Input(Input::Button {
            at: pt(399, 399),
            button: Button::Right,
            down: true,
        })));

        let mut drained = Vec::new();
        while let Some(cmd) = rx.try_recv() {
            drained.push(cmd);
        }

        // Expect exactly GAMEPAD_SOFT_LIMIT gamepads, followed by 1 coalesced move, then the button.
        assert_eq!(drained.len(), GAMEPAD_SOFT_LIMIT + 2);
        assert!(
            drained[..GAMEPAD_SOFT_LIMIT]
                .iter()
                .all(|cmd| matches!(cmd, SessionCommand::Input(Input::Gamepad(_))))
        );
        assert_eq!(
            drained[GAMEPAD_SOFT_LIMIT],
            SessionCommand::Input(Input::Move(pt(399, 399)))
        );
        assert_eq!(
            drained[GAMEPAD_SOFT_LIMIT + 1],
            SessionCommand::Input(Input::Button {
                at: pt(399, 399),
                button: Button::Right,
                down: true,
            })
        );
    }

    #[tokio::test]
    async fn command_queue_async_recv_delivers_coalesced_commands_and_closes() {
        let (tx, mut rx) = command_channel();
        let producer = tokio::spawn(async move {
            tx.send(SessionCommand::Input(Input::Move(pt(10, 10))));
            tx.send(SessionCommand::Input(Input::Move(pt(20, 30))));
            tx.send(SessionCommand::SetBitrate(Some(45)));
            drop(tx);
        });

        let mut received = Vec::new();
        while let Some(cmd) = rx.recv().await {
            received.push(cmd);
        }
        producer.await.unwrap();

        assert!(received.contains(&SessionCommand::SetBitrate(Some(45))));
        assert!(received.contains(&SessionCommand::Input(Input::Move(pt(20, 30)))));
        assert!(!received.contains(&SessionCommand::Input(Input::Move(pt(10, 10)))));
    }

    #[test]
    fn session_controller_audio_muted_toggles_atomic_flag() {
        let (cmd_tx, _cmd_rx) = command_channel();
        let audio_muted = Arc::new(AtomicBool::new(false));
        let (stop_tx, _stop_rx) = oneshot::channel();
        let controller = SessionController {
            commands: cmd_tx,
            audio_muted: audio_muted.clone(),
            stop: Mutex::new(Some(stop_tx)),
        };

        assert!(!audio_muted.load(Ordering::Relaxed));
        controller.set_audio_muted(true);
        assert!(audio_muted.load(Ordering::Relaxed));
        controller.set_audio_muted(false);
        assert!(!audio_muted.load(Ordering::Relaxed));
    }
}
