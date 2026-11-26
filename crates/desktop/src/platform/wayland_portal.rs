#[cfg(target_os = "linux")]
use std::{
    os::fd::{AsRawFd, OwnedFd, RawFd},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[cfg(target_os = "linux")]
use anyhow::{Context, Result, anyhow};
#[cfg(target_os = "linux")]
use ashpd::desktop::{
    PersistMode,
    remote_desktop::{Axis as PortalAxis, DeviceType, KeyState, RemoteDesktop},
    screencast::{CursorMode, Screencast, SourceType},
};
#[cfg(target_os = "linux")]
use remoters_core::Button;
#[cfg(any(target_os = "linux", test))]
use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
use tokio::sync::{oneshot, watch};

/// Returns `true` when running inside a Wayland desktop session.
#[cfg(any(target_os = "linux", test))]
#[allow(dead_code)]
pub fn is_wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|val| !val.is_empty())
        || std::env::var("XDG_SESSION_TYPE")
            .is_ok_and(|val| val.trim().eq_ignore_ascii_case("wayland"))
}

/// Maps a Windows Virtual-Key (`1..=254`) wire code to a Linux `evdev` keycode
/// (`linux/input-event-codes.h`), used by `xdg-desktop-portal` `NotifyKeyboardKeycode`.
#[cfg(any(target_os = "linux", test))]
pub fn vk_to_evdev(vk: u16) -> Option<i32> {
    Some(match vk {
        // Mouse buttons (`0x01`, `0x02`, `0x04`, `0x05`, `0x06`) are delivered via `Input::Button`.
        0x08 => 14,  // KEY_BACKSPACE
        0x09 => 15,  // KEY_TAB
        0x0C => 76,  // KEY_KP5 (Clear)
        0x0D => 28,  // KEY_ENTER
        0x10 => 42,  // KEY_LEFTSHIFT
        0x11 => 29,  // KEY_LEFTCTRL
        0x12 => 56,  // KEY_LEFTALT
        0x13 => 119, // KEY_PAUSE
        0x14 => 58,  // KEY_CAPSLOCK
        0x1B => 1,   // KEY_ESC
        0x20 => 57,  // KEY_SPACE
        0x21 => 104, // KEY_PAGEUP
        0x22 => 109, // KEY_PAGEDOWN
        0x23 => 107, // KEY_END
        0x24 => 102, // KEY_HOME
        0x25 => 105, // KEY_LEFT
        0x26 => 103, // KEY_UP
        0x27 => 106, // KEY_RIGHT
        0x28 => 108, // KEY_DOWN
        0x2C => 99,  // KEY_SYSRQ
        0x2D => 110, // KEY_INSERT
        0x2E => 111, // KEY_DELETE
        0x2F => 138, // KEY_HELP
        // Digits '0'..'9'
        0x30 => 11, // KEY_0
        0x31 => 2,  // KEY_1
        0x32 => 3,  // KEY_2
        0x33 => 4,  // KEY_3
        0x34 => 5,  // KEY_4
        0x35 => 6,  // KEY_5
        0x36 => 7,  // KEY_6
        0x37 => 8,  // KEY_7
        0x38 => 9,  // KEY_8
        0x39 => 10, // KEY_9
        // Letters 'A'..'Z'
        0x41 => 30,  // KEY_A
        0x42 => 48,  // KEY_B
        0x43 => 46,  // KEY_C
        0x44 => 32,  // KEY_D
        0x45 => 18,  // KEY_E
        0x46 => 33,  // KEY_F
        0x47 => 34,  // KEY_G
        0x48 => 35,  // KEY_H
        0x49 => 23,  // KEY_I
        0x4A => 36,  // KEY_J
        0x4B => 37,  // KEY_K
        0x4C => 38,  // KEY_L
        0x4D => 50,  // KEY_M
        0x4E => 49,  // KEY_N
        0x4F => 24,  // KEY_O
        0x50 => 25,  // KEY_P
        0x51 => 16,  // KEY_Q
        0x52 => 19,  // KEY_R
        0x53 => 31,  // KEY_S
        0x54 => 20,  // KEY_T
        0x55 => 22,  // KEY_U
        0x56 => 47,  // KEY_V
        0x57 => 17,  // KEY_W
        0x58 => 45,  // KEY_X
        0x59 => 21,  // KEY_Y
        0x5A => 44,  // KEY_Z
        0x5B => 125, // KEY_LEFTMETA
        0x5C => 126, // KEY_RIGHTMETA
        0x5D => 139, // KEY_MENU
        0x5F => 142, // KEY_SLEEP
        // Numpad
        0x60 => 82, // KEY_KP0
        0x61 => 79, // KEY_KP1
        0x62 => 80, // KEY_KP2
        0x63 => 81, // KEY_KP3
        0x64 => 75, // KEY_KP4
        0x65 => 76, // KEY_KP5
        0x66 => 77, // KEY_KP6
        0x67 => 71, // KEY_KP7
        0x68 => 72, // KEY_KP8
        0x69 => 73, // KEY_KP9
        0x6A => 55, // KEY_KPASTERISK
        0x6B => 78, // KEY_KPPLUS
        0x6D => 74, // KEY_KPMINUS
        0x6E => 83, // KEY_KPDOT
        0x6F => 98, // KEY_KPSLASH
        // Function keys F1..F24
        0x70 => 59,  // KEY_F1
        0x71 => 60,  // KEY_F2
        0x72 => 61,  // KEY_F3
        0x73 => 62,  // KEY_F4
        0x74 => 63,  // KEY_F5
        0x75 => 64,  // KEY_F6
        0x76 => 65,  // KEY_F7
        0x77 => 66,  // KEY_F8
        0x78 => 67,  // KEY_F9
        0x79 => 68,  // KEY_F10
        0x7A => 87,  // KEY_F11
        0x7B => 88,  // KEY_F12
        0x7C => 183, // KEY_F13
        0x7D => 184, // KEY_F14
        0x7E => 185, // KEY_F15
        0x7F => 186, // KEY_F16
        0x80 => 187, // KEY_F17
        0x81 => 188, // KEY_F18
        0x82 => 189, // KEY_F19
        0x83 => 190, // KEY_F20
        0x84 => 191, // KEY_F21
        0x85 => 192, // KEY_F22
        0x86 => 193, // KEY_F23
        0x87 => 194, // KEY_F24
        0x90 => 69,  // KEY_NUMLOCK
        0x91 => 70,  // KEY_SCROLLLOCK
        0xA0 => 42,  // KEY_LEFTSHIFT
        0xA1 => 54,  // KEY_RIGHTSHIFT
        0xA2 => 29,  // KEY_LEFTCTRL
        0xA3 => 97,  // KEY_RIGHTCTRL
        0xA4 => 56,  // KEY_LEFTALT
        0xA5 => 100, // KEY_RIGHTALT
        0xA6 => 158, // KEY_BACK
        0xA7 => 159, // KEY_FORWARD
        0xA8 => 173, // KEY_REFRESH
        0xA9 => 128, // KEY_STOP
        0xAA => 217, // KEY_SEARCH
        0xAB => 156, // KEY_BOOKMARKS
        0xAC => 172, // KEY_HOMEPAGE
        0xAD => 113, // KEY_MUTE
        0xAE => 114, // KEY_VOLUMEDOWN
        0xAF => 115, // KEY_VOLUMEUP
        0xB0 => 163, // KEY_NEXTSONG
        0xB1 => 165, // KEY_PREVIOUSSONG
        0xB2 => 166, // KEY_STOPCD
        0xB3 => 164, // KEY_PLAYPAUSE
        0xBA => 39,  // KEY_SEMICOLON
        0xBB => 13,  // KEY_EQUAL
        0xBC => 51,  // KEY_COMMA
        0xBD => 12,  // KEY_MINUS
        0xBE => 52,  // KEY_DOT
        0xBF => 53,  // KEY_SLASH
        0xC0 => 41,  // KEY_GRAVE
        0xDB => 26,  // KEY_LEFTBRACE
        0xDC => 43,  // KEY_BACKSLASH
        0xDD => 27,  // KEY_RIGHTBRACE
        0xDE => 40,  // KEY_APOSTROPHE
        0xE2 => 86,  // KEY_102ND
        _ => return None,
    })
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PortalScrollAxis {
    Vertical,
    Horizontal,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, PartialEq)]
enum PortalInputCommand {
    PointerMotionAbsolute { x: f64, y: f64 },
    PointerButton { button: i32, down: bool },
    PointerScroll { axis: PortalScrollAxis, steps: i32 },
    KeyboardKeycode { keycode: i32, down: bool },
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PortalCursorPreference {
    Hidden,
    Embedded,
}

/// Chooses the ScreenCast cursor mode for a Wayland session.
///
/// Interactive remote-control sessions prefer `Hidden` (matching macOS and Windows)
/// so the viewer renders a single zero-latency local cursor without a second cursor
/// baked into the video stream. View-only sessions prefer `Embedded` so observers
/// can see the host's pointer.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn select_cursor_preference(
    allow_input: bool,
    supports_hidden: bool,
    supports_embedded: bool,
) -> PortalCursorPreference {
    if allow_input {
        if supports_hidden {
            PortalCursorPreference::Hidden
        } else if supports_embedded {
            PortalCursorPreference::Embedded
        } else {
            PortalCursorPreference::Hidden
        }
    } else if supports_embedded {
        PortalCursorPreference::Embedded
    } else {
        PortalCursorPreference::Hidden
    }
}

/// Coalesces consecutive `PointerMotionAbsolute` commands already queued in `rx`
/// so rapid 250–1000Hz pointer updates only dispatch the latest coordinates over
/// D-Bus while preserving exact ordering relative to clicks, scrolls, and keys.
#[cfg(any(target_os = "linux", test))]
fn coalesce_command(
    first: PortalInputCommand,
    rx: &mut mpsc::UnboundedReceiver<PortalInputCommand>,
    deferred: &mut Option<PortalInputCommand>,
) -> PortalInputCommand {
    let PortalInputCommand::PointerMotionAbsolute { mut x, mut y } = first else {
        return first;
    };
    while let Ok(next) = rx.try_recv() {
        match next {
            PortalInputCommand::PointerMotionAbsolute { x: nx, y: ny } => {
                x = nx;
                y = ny;
            }
            other => {
                *deferred = Some(other);
                break;
            }
        }
    }
    PortalInputCommand::PointerMotionAbsolute { x, y }
}

/// Tracks active interactive Wayland sessions that had to fall back to
/// `CursorMode::Embedded` because the compositor did not advertise `Hidden`.
#[cfg(target_os = "linux")]
static EMBEDDED_CURSOR_SESSIONS: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_os = "linux")]
pub fn is_embedded_cursor_active() -> bool {
    EMBEDDED_CURSOR_SESSIONS.load(Ordering::Acquire) > 0
}

#[cfg(target_os = "linux")]
pub struct WaylandPortalSession {
    pipewire_fd: OwnedFd,
    node_id: u32,
    width: u32,
    height: u32,
    input_enabled: bool,
    embedded_cursor_counted: AtomicBool,
    input_tx: mpsc::UnboundedSender<PortalInputCommand>,
    closed: Arc<Mutex<Option<String>>>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    closed_done: watch::Receiver<bool>,
}

#[cfg(target_os = "linux")]
impl WaylandPortalSession {
    fn release_embedded_cursor_slot(&self) {
        if self.embedded_cursor_counted.swap(false, Ordering::AcqRel) {
            EMBEDDED_CURSOR_SESSIONS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for WaylandPortalSession {
    fn drop(&mut self) {
        self.release_embedded_cursor_slot();
        if let Some(tx) = self
            .shutdown_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = tx.send(());
        }
    }
}

#[cfg(target_os = "linux")]
fn token_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|val| !val.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("remoters").join("wayland-portal-token"))
}

#[cfg(target_os = "linux")]
fn load_restore_token() -> Option<String> {
    let path = token_path()?;
    let token = std::fs::read_to_string(path).ok()?;
    let trimmed = token.trim().to_owned();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(target_os = "linux")]
fn save_restore_token(token: &str) {
    let Some(path) = token_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, token);
}

#[cfg(target_os = "linux")]
impl WaylandPortalSession {
    pub async fn open(allow_input: bool) -> Result<Self> {
        let restore_token = load_restore_token();
        let screencast = Screencast::new()
            .await
            .context("Failed to connect to xdg-desktop-portal ScreenCast.")?;
        let cursor_pref = screencast
            .available_cursor_modes()
            .await
            .ok()
            .map(|modes| {
                select_cursor_preference(
                    allow_input,
                    modes.contains(CursorMode::Hidden),
                    modes.contains(CursorMode::Embedded),
                )
            })
            .unwrap_or_else(|| select_cursor_preference(allow_input, true, true));
        let cursor_mode = match cursor_pref {
            PortalCursorPreference::Hidden => CursorMode::Hidden,
            PortalCursorPreference::Embedded => CursorMode::Embedded,
        };

        let (input_tx, mut input_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let (closed_done_tx, closed_done) = watch::channel(false);
        let closed = Arc::new(Mutex::new(None));

        if allow_input {
            let remote = RemoteDesktop::new()
                .await
                .context("Failed to connect to xdg-desktop-portal RemoteDesktop.")?;
            let session = remote
                .create_session()
                .await
                .context("Failed to create Wayland RemoteDesktop portal session.")?;
            remote
                .select_devices(
                    &session,
                    DeviceType::Keyboard | DeviceType::Pointer,
                    restore_token.as_deref(),
                    PersistMode::ExplicitlyRevoked,
                )
                .await
                .context("Failed to select Wayland remote input devices.")?;
            screencast
                .select_sources(
                    &session,
                    cursor_mode,
                    SourceType::Monitor.into(),
                    false,
                    None,
                    PersistMode::DoNot,
                )
                .await
                .context("Failed to select Wayland screencast sources.")?;
            let selected = remote
                .start(&session, None)
                .await
                .context("Failed to start Wayland RemoteDesktop session.")?
                .response()
                .context("Wayland RemoteDesktop authorization was cancelled or denied.")?;
            if let Some(new_token) = selected.restore_token() {
                save_restore_token(new_token);
            }
            let stream = selected
                .streams()
                .and_then(|streams| streams.first())
                .context("Wayland RemoteDesktop portal returned no monitor stream.")?;
            let node_id = stream.pipe_wire_node_id();
            let (width, height) = stream
                .size()
                .map(|(w, h)| (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0)))
                .unwrap_or((0, 0));
            let pw_fd = screencast
                .open_pipe_wire_remote(&session)
                .await
                .context("Failed to open PipeWire remote fd from portal.")?;
            let input_enabled = selected.devices().contains(DeviceType::Pointer)
                || selected.devices().contains(DeviceType::Keyboard);

            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<()>>(1);
            let worker_closed = closed.clone();
            std::thread::Builder::new()
                .name("wayland-portal-input".into())
                .spawn(move || {
                    // Own `session` and `remote` inside an async thread so D-Bus keepalive & notify_* run cleanly.
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        let _ = ready_tx.send(Err(anyhow!(
                            "Failed to start Wayland portal worker runtime."
                        )));
                        let _ = closed_done_tx.send(true);
                        return;
                    };
                    let _ = ready_tx.send(Ok(()));
                    rt.block_on(async move {
                        let _session_guard = &session;
                        {
                            let closed_stream = session.receive_closed().await.ok();
                            let closed_future = async {
                                match closed_stream {
                                    Some(stream) => {
                                        tokio::pin!(stream);
                                        let _ = std::future::poll_fn(|cx| {
                                            futures_util::Stream::poll_next(stream.as_mut(), cx)
                                        })
                                        .await;
                                    }
                                    None => std::future::pending::<()>().await,
                                }
                            };
                            tokio::pin!(closed_future);
                            let mut deferred: Option<PortalInputCommand> = None;
                            loop {
                                let cmd = if let Some(cmd) = deferred.take() {
                                    if shutdown_rx.try_recv().is_ok() {
                                        break;
                                    }
                                    Some(cmd)
                                } else {
                                    tokio::select! {
                                        biased;
                                        _ = &mut shutdown_rx => break,
                                        _ = &mut closed_future => {
                                            *worker_closed.lock().unwrap_or_else(|p| p.into_inner()) =
                                                Some("Wayland RemoteDesktop session closed by compositor.".to_owned());
                                            break;
                                        }
                                        cmd = input_rx.recv() => cmd,
                                    }
                                };
                                let Some(cmd) = cmd else {
                                    break;
                                };
                                match coalesce_command(cmd, &mut input_rx, &mut deferred) {
                                    PortalInputCommand::PointerMotionAbsolute { x, y } => {
                                        let _ = remote
                                            .notify_pointer_motion_absolute(&session, node_id, x, y)
                                            .await;
                                    }
                                    PortalInputCommand::PointerButton { button, down } => {
                                        let state = if down {
                                            KeyState::Pressed
                                        } else {
                                            KeyState::Released
                                        };
                                        let _ = remote
                                            .notify_pointer_button(&session, button, state)
                                            .await;
                                    }
                                    PortalInputCommand::PointerScroll { axis, steps } => {
                                        let portal_axis = match axis {
                                            PortalScrollAxis::Vertical => PortalAxis::Vertical,
                                            PortalScrollAxis::Horizontal => PortalAxis::Horizontal,
                                        };
                                        let _ = remote
                                            .notify_pointer_axis_discrete(
                                                &session,
                                                portal_axis,
                                                steps,
                                            )
                                            .await;
                                    }
                                    PortalInputCommand::KeyboardKeycode { keycode, down } => {
                                        let state = if down {
                                            KeyState::Pressed
                                        } else {
                                            KeyState::Released
                                        };
                                        let _ = remote
                                            .notify_keyboard_keycode(&session, keycode, state)
                                            .await;
                                    }
                                }
                            }
                        }
                        let _ = session.close().await;
                        {
                            let mut guard = worker_closed.lock().unwrap_or_else(|p| p.into_inner());
                            if guard.is_none() {
                                *guard = Some("Wayland RemoteDesktop session closed.".to_owned());
                            }
                        }
                        let _ = closed_done_tx.send(true);
                    });
                })?;
            ready_rx
                .recv()
                .map_err(|_| anyhow!("Wayland portal thread exited."))??;

            let track_embedded = input_enabled && cursor_pref == PortalCursorPreference::Embedded;
            if track_embedded {
                EMBEDDED_CURSOR_SESSIONS.fetch_add(1, Ordering::AcqRel);
            }

            return Ok(Self {
                pipewire_fd: pw_fd,
                node_id,
                width,
                height,
                input_enabled,
                embedded_cursor_counted: AtomicBool::new(track_embedded),
                input_tx,
                closed,
                shutdown_tx: Mutex::new(Some(shutdown_tx)),
                closed_done,
            });
        }

        // View-only ScreenCast session
        let session = screencast
            .create_session()
            .await
            .context("Failed to create Wayland ScreenCast session.")?;
        screencast
            .select_sources(
                &session,
                cursor_mode,
                SourceType::Monitor.into(),
                false,
                restore_token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await
            .context("Failed to select Wayland screencast sources.")?;
        let streams = screencast
            .start(&session, None)
            .await
            .context("Failed to start Wayland ScreenCast session.")?
            .response()
            .context("Wayland ScreenCast authorization was cancelled or denied.")?;
        if let Some(new_token) = streams.restore_token() {
            save_restore_token(new_token);
        }
        let stream = streams
            .streams()
            .first()
            .context("Wayland ScreenCast portal returned no monitor stream.")?;
        let node_id = stream.pipe_wire_node_id();
        let (width, height) = stream
            .size()
            .map(|(w, h)| (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0)))
            .unwrap_or((0, 0));
        let pw_fd = screencast
            .open_pipe_wire_remote(&session)
            .await
            .context("Failed to open PipeWire remote fd from portal.")?;

        let worker_closed = closed.clone();
        std::thread::Builder::new()
            .name("wayland-screencast-keepalive".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    let _ = closed_done_tx.send(true);
                    return;
                };
                rt.block_on(async move {
                    let _session_guard = &session;
                    {
                        let closed_stream = session.receive_closed().await.ok();
                        let closed_future = async {
                            match closed_stream {
                                Some(stream) => {
                                    tokio::pin!(stream);
                                    let _ = std::future::poll_fn(|cx| {
                                        futures_util::Stream::poll_next(stream.as_mut(), cx)
                                    })
                                    .await;
                                }
                                None => std::future::pending::<()>().await,
                            }
                        };
                        tokio::pin!(closed_future);
                        tokio::select! {
                            _ = &mut shutdown_rx => {}
                            _ = &mut closed_future => {
                                *worker_closed.lock().unwrap_or_else(|p| p.into_inner()) =
                                    Some("Wayland ScreenCast session closed by compositor.".to_owned());
                            }
                        }
                    }
                    let _ = session.close().await;
                    {
                        let mut guard = worker_closed.lock().unwrap_or_else(|p| p.into_inner());
                        if guard.is_none() {
                            *guard = Some("Wayland ScreenCast session closed.".to_owned());
                        }
                    }
                    let _ = closed_done_tx.send(true);
                });
            })?;

        Ok(Self {
            pipewire_fd: pw_fd,
            node_id,
            width,
            height,
            input_enabled: false,
            embedded_cursor_counted: AtomicBool::new(false),
            input_tx,
            closed,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            closed_done,
        })
    }

    pub async fn shutdown(&self) {
        self.release_embedded_cursor_slot();
        if let Some(tx) = self
            .shutdown_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = tx.send(());
        }
        let mut rx = self.closed_done.clone();
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.wait_for(|&done| done)).await;
    }

    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        (self.width >= 2 && self.height >= 2).then_some((self.width, self.height))
    }

    pub fn clone_pipewire_fd(&self) -> Result<OwnedFd> {
        self.pipewire_fd
            .try_clone()
            .context("Failed to clone PipeWire portal file descriptor.")
    }

    #[allow(dead_code)]
    pub fn pipewire_raw_fd(&self) -> RawFd {
        self.pipewire_fd.as_raw_fd()
    }

    #[allow(dead_code)]
    pub fn closed_reason(&self) -> Option<String> {
        self.closed.lock().unwrap().clone()
    }

    pub fn input_enabled(&self) -> bool {
        self.input_enabled
    }

    pub fn pointer_motion_absolute(&self, x: f64, y: f64) {
        if self.input_enabled {
            let _ = self
                .input_tx
                .send(PortalInputCommand::PointerMotionAbsolute { x, y });
        }
    }

    pub fn pointer_button(&self, button: Button, down: bool) {
        if !self.input_enabled {
            return;
        }
        let evdev_btn = match button {
            Button::Left => 272,   // BTN_LEFT
            Button::Right => 273,  // BTN_RIGHT
            Button::Middle => 274, // BTN_MIDDLE
        };
        let _ = self.input_tx.send(PortalInputCommand::PointerButton {
            button: evdev_btn,
            down,
        });
    }

    pub fn pointer_scroll_discrete(&self, vertical_steps: i32, horizontal_steps: i32) {
        if !self.input_enabled {
            return;
        }
        if vertical_steps != 0 {
            let _ = self.input_tx.send(PortalInputCommand::PointerScroll {
                axis: PortalScrollAxis::Vertical,
                steps: vertical_steps,
            });
        }
        if horizontal_steps != 0 {
            let _ = self.input_tx.send(PortalInputCommand::PointerScroll {
                axis: PortalScrollAxis::Horizontal,
                steps: horizontal_steps,
            });
        }
    }

    pub fn keyboard_vk(&self, vk: u16, down: bool) -> bool {
        if !self.input_enabled {
            return false;
        }
        let Some(keycode) = vk_to_evdev(vk) else {
            return false;
        };
        self.input_tx
            .send(PortalInputCommand::KeyboardKeycode { keycode, down })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::{
        PortalCursorPreference, PortalInputCommand, PortalScrollAxis, coalesce_command,
        select_cursor_preference, vk_to_evdev,
    };

    #[test]
    fn maps_full_vk_range_to_evdev_without_panicking() {
        for vk in 1u16..=254 {
            let _ = vk_to_evdev(vk);
        }
        assert_eq!(vk_to_evdev(0x01), None);
        assert_eq!(vk_to_evdev(0x02), None);
        assert_eq!(vk_to_evdev(0x04), None);
    }

    #[test]
    fn maps_common_vks_to_expected_evdev_codes() {
        assert_eq!(vk_to_evdev(0x41), Some(30)); // A
        assert_eq!(vk_to_evdev(0x5A), Some(44)); // Z
        assert_eq!(vk_to_evdev(0x0D), Some(28)); // Enter
        assert_eq!(vk_to_evdev(0x1B), Some(1)); // Esc
        assert_eq!(vk_to_evdev(0x20), Some(57)); // Space
        assert_eq!(vk_to_evdev(0x5B), Some(125)); // SuperLeft
        assert_eq!(vk_to_evdev(0xA2), Some(29)); // LeftCtrl
        assert_eq!(vk_to_evdev(0x70), Some(59)); // F1
    }

    #[test]
    fn cursor_preference_avoids_double_cursor_on_interactive_sessions() {
        // Interactive control prefers Hidden so only the viewer's local cursor is shown.
        assert_eq!(
            select_cursor_preference(true, true, true),
            PortalCursorPreference::Hidden
        );
        // Falls back to Embedded only when the compositor does not advertise Hidden.
        assert_eq!(
            select_cursor_preference(true, false, true),
            PortalCursorPreference::Embedded
        );
        // View-only observers prefer Embedded so they can see the host cursor.
        assert_eq!(
            select_cursor_preference(false, true, true),
            PortalCursorPreference::Embedded
        );
        assert_eq!(
            select_cursor_preference(false, true, false),
            PortalCursorPreference::Hidden
        );
    }

    #[test]
    fn coalesces_consecutive_pointer_motions_without_reordering_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(PortalInputCommand::PointerMotionAbsolute { x: 10.0, y: 20.0 })
            .unwrap();
        tx.send(PortalInputCommand::PointerMotionAbsolute { x: 30.0, y: 40.0 })
            .unwrap();
        tx.send(PortalInputCommand::PointerMotionAbsolute { x: 50.0, y: 60.0 })
            .unwrap();
        tx.send(PortalInputCommand::PointerButton {
            button: 272,
            down: true,
        })
        .unwrap();
        tx.send(PortalInputCommand::PointerMotionAbsolute { x: 70.0, y: 80.0 })
            .unwrap();
        tx.send(PortalInputCommand::PointerMotionAbsolute { x: 90.0, y: 100.0 })
            .unwrap();
        tx.send(PortalInputCommand::PointerScroll {
            axis: PortalScrollAxis::Vertical,
            steps: -1,
        })
        .unwrap();
        tx.send(PortalInputCommand::PointerScroll {
            axis: PortalScrollAxis::Horizontal,
            steps: 2,
        })
        .unwrap();
        tx.send(PortalInputCommand::KeyboardKeycode {
            keycode: 30,
            down: true,
        })
        .unwrap();
        tx.send(PortalInputCommand::PointerButton {
            button: 272,
            down: false,
        })
        .unwrap();

        let mut deferred = None;
        let mut dispatched = Vec::new();
        loop {
            let next = deferred.take().or_else(|| rx.try_recv().ok());
            let Some(cmd) = next else {
                break;
            };
            dispatched.push(coalesce_command(cmd, &mut rx, &mut deferred));
        }

        assert_eq!(
            dispatched,
            vec![
                PortalInputCommand::PointerMotionAbsolute { x: 50.0, y: 60.0 },
                PortalInputCommand::PointerButton {
                    button: 272,
                    down: true,
                },
                PortalInputCommand::PointerMotionAbsolute { x: 90.0, y: 100.0 },
                PortalInputCommand::PointerScroll {
                    axis: PortalScrollAxis::Vertical,
                    steps: -1,
                },
                PortalInputCommand::PointerScroll {
                    axis: PortalScrollAxis::Horizontal,
                    steps: 2,
                },
                PortalInputCommand::KeyboardKeycode {
                    keycode: 30,
                    down: true,
                },
                PortalInputCommand::PointerButton {
                    button: 272,
                    down: false,
                },
            ]
        );
    }

    #[test]
    fn coalesces_pure_motion_burst_to_latest_coordinate() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        for i in 0..500 {
            tx.send(PortalInputCommand::PointerMotionAbsolute {
                x: f64::from(i),
                y: f64::from(i) * 0.5,
            })
            .unwrap();
        }
        let mut deferred = None;
        let first = rx.try_recv().unwrap();
        let coalesced = coalesce_command(first, &mut rx, &mut deferred);
        assert_eq!(
            coalesced,
            PortalInputCommand::PointerMotionAbsolute { x: 499.0, y: 249.5 }
        );
        assert_eq!(deferred, None);
        assert!(rx.try_recv().is_err());
    }
}
