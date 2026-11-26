use std::{
    collections::HashMap,
    net::{SocketAddr, SocketAddrV4},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use remoters_core::{
    Input,
    session::{
        AuxSender, CODEC_BIT_AV1, CODEC_BIT_H264, CODEC_BIT_HEVC, ClipboardData, CursorShape,
        CursorUpdate, HostInput, HostSession, PendingHostSession, ReceivedFile, SessionHost,
        StreamPreference,
    },
    video::{AccessUnit, Codec, Format},
};
use tokio::{
    runtime::Builder,
    sync::{Notify, mpsc, oneshot},
    task::JoinHandle as TokioJoinHandle,
};
use xcap::Monitor;

use crate::{
    audio::{AudioSink, HostAudioCapture},
    codec::{self, Encoder},
    metrics::HostStats,
    platform::{
        NativeDesktop,
        display::{self, VirtualResolution, VirtualSpec},
    },
    privacy::{PrivacyScreenGuard, PrivacyTarget},
};

const PEER_VIDEO_QUEUE_CAPACITY: usize = 2;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum HostControlMode {
    #[default]
    Collaborative = 0,
    SingleController = 1,
}

impl HostControlMode {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::SingleController,
            _ => Self::Collaborative,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum HostDisplayTarget {
    Fixed { display_id: u32, format: Format },
    Virtual(VirtualResolution),
}

#[derive(Debug)]
pub enum HostEvent {
    PeersChanged {
        peers: Vec<SocketAddr>,
        controller: Option<SocketAddr>,
        active_format: Option<(u16, u16, u16)>,
    },
    Clipboard(ClipboardData),
    FileReceived(ReceivedFile),
    Finished(Result<()>),
    Stopped,
}

pub struct Host {
    pub address: SocketAddrV4,
    pub code: String,
    clipboard: mpsc::Sender<ClipboardData>,
    control_mode: Arc<AtomicU8>,
    privacy_screen: Arc<AtomicBool>,
    privacy_notify: Arc<Notify>,
    _stop: oneshot::Sender<()>,
}

impl Host {
    pub fn send_clipboard(&self, data: ClipboardData) {
        let _ = self.clipboard.try_send(data);
    }

    #[allow(dead_code)]
    pub fn set_control_mode(&self, mode: HostControlMode) {
        self.control_mode.store(mode.as_u8(), Ordering::Release);
    }

    pub fn set_privacy_screen(&self, enabled: bool) {
        self.privacy_screen.store(enabled, Ordering::Release);
        self.privacy_notify.notify_one();
    }

    #[allow(dead_code)]
    pub fn privacy_screen_enabled(&self) -> bool {
        self.privacy_screen.load(Ordering::Acquire)
    }

    pub fn start(
        address: SocketAddrV4,
        view_only: bool,
        control_mode: HostControlMode,
        privacy_screen: bool,
        target: HostDisplayTarget,
        event: impl Fn(HostEvent) + Send + 'static,
    ) -> Result<Self> {
        match target {
            HostDisplayTarget::Fixed { format, .. } => format.validate()?,
            HostDisplayTarget::Virtual(VirtualResolution::Fixed(spec)) => {
                codec::virtual_format(spec.width, spec.height, spec.fps as u16)?;
            }
            HostDisplayTarget::Virtual(VirtualResolution::Adaptive) => {}
        }
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let host = {
            let _entered = runtime.enter();
            SessionHost::bind_dynamic(address, view_only)
                .with_context(|| format!("Could not listen on {address}."))?
        };
        let address = host.address()?;
        let code = host.pairing_code();
        let (clipboard, mut outgoing_clipboard) = mpsc::channel::<ClipboardData>(16);
        let control_mode_state = Arc::new(AtomicU8::new(control_mode.as_u8()));
        let worker_control_mode = control_mode_state.clone();
        let privacy_screen_state = Arc::new(AtomicBool::new(privacy_screen));
        let worker_privacy_screen = privacy_screen_state.clone();
        let privacy_notify = Arc::new(Notify::new());
        let worker_privacy_notify = privacy_notify.clone();
        let (stop, mut stopped) = oneshot::channel();
        thread::Builder::new()
            .name("lan-host".into())
            .spawn(move || {
                runtime.block_on(async {
                    #[cfg(target_os = "linux")]
                    let mut wayland_portal =
                        if crate::platform::wayland_portal::is_wayland_session() {
                            match crate::platform::wayland_portal::WaylandPortalSession::open(
                                !view_only,
                            )
                            .await
                            {
                                Ok(portal) => Some(Arc::new(portal)),
                                Err(error) => {
                                    eprintln!("Wayland portal unavailable at startup: {error:#}");
                                    None
                                }
                            }
                        } else {
                            None
                        };

                    let mut active: Option<ActiveBroadcast> = None;
                    let mut next_peer_id: u64 = 1;
                    let (attached_tx, mut attached_rx) = mpsc::channel::<PeerAttached>(16);
                    // Run `host.accept_pending()` on its own task so 60fps video sends inside
                    // `tokio::select!` never cancel an in-flight QUIC/TLS handshake mid-flight.
                    let (pending_tx, mut pending_rx) =
                        mpsc::channel::<Result<PendingHostSession>>(16);
                    let accept_task = tokio::spawn(async move {
                        loop {
                            let accepted = host.accept_pending().await;
                            if pending_tx.send(accepted).await.is_err() {
                                break;
                            }
                        }
                    });

                    loop {
                        if let Some(broadcast) = active.as_mut() {
                            tokio::select! {
                                biased;
                                _ = &mut stopped => {
                                    teardown_broadcast(
                                        &mut active,
                                        #[cfg(target_os = "linux")]
                                        &mut wayland_portal,
                                    )
                                    .await;
                                    break;
                                }
                                _ = worker_privacy_notify.notified() => {
                                    let enabled = worker_privacy_screen.load(Ordering::Acquire);
                                    broadcast.sync_privacy_screen(enabled);
                                }
                                accepted = pending_rx.recv() => {
                                    let Some(accepted) = accepted else {
                                        teardown_broadcast(
                                            &mut active,
                                            #[cfg(target_os = "linux")]
                                            &mut wayland_portal,
                                        )
                                        .await;
                                        break;
                                    };
                                    match accepted {
                                        Ok(pending) => {
                                            let pref = pending.stream_preference();
                                            if !viewer_supports_active_format(broadcast.format, pref) {
                                                pending.reject(b"active stream format unsupported");
                                                continue;
                                            }
                                            let mode = HostControlMode::from_u8(
                                                worker_control_mode.load(Ordering::Acquire),
                                            );
                                            let (can_control, becomes_controller) =
                                                resolve_new_peer_access(
                                                    view_only,
                                                    mode,
                                                    broadcast.controller_id.is_some(),
                                                );
                                            let peer_view_only = !can_control;
                                            let peer_id = next_peer_id;
                                            next_peer_id += 1;
                                            if becomes_controller {
                                                broadcast.controller_id = Some(peer_id);
                                            }
                                            let format = broadcast.format;
                                            let tx = attached_tx.clone();
                                            tokio::spawn(async move {
                                                let addr = pending.peer_address();
                                                let receive_dir = (!peer_view_only)
                                                    .then(crate::clipboard::remote_receive_dir);
                                                match pending
                                                    .start_with_options(
                                                        format,
                                                        Some(peer_view_only),
                                                        receive_dir,
                                                    )
                                                    .await
                                                {
                                                    Ok(session) => {
                                                        let _ = tx
                                                            .send(PeerAttached {
                                                                peer_id,
                                                                addr,
                                                                can_control,
                                                                session,
                                                            })
                                                            .await;
                                                    }
                                                    Err(error) => {
                                                        eprintln!(
                                                            "Failed to start concurrent viewer {addr}: {error:#}"
                                                        );
                                                    }
                                                }
                                            });
                                        }
                                        Err(error) => {
                                            eprintln!("Accept failed during active broadcast: {error:#}");
                                        }
                                    }
                                }
                                Some(attached) = attached_rx.recv() => {
                                    broadcast.add_peer(attached);
                                    broadcast.emit_peers(&event);
                                }
                                Some(peer_event) = broadcast.peer_events_rx.recv() => {
                                    match peer_event {
                                        PeerEvent::Input(peer_id, input) => {
                                            let mode = HostControlMode::from_u8(
                                                worker_control_mode.load(Ordering::Acquire),
                                            );
                                            let peer_can_control = broadcast
                                                .peers
                                                .get(&peer_id)
                                                .is_some_and(|peer| peer.can_control);
                                            let allow_control = allows_peer_input(
                                                view_only,
                                                mode,
                                                broadcast.controller_id,
                                                peer_id,
                                                peer_can_control,
                                            );
                                            match input {
                                                HostInput::Remote(remote) => {
                                                    if allow_control
                                                        && let Err(error) =
                                                            broadcast.apply_remote_input(remote)
                                                    {
                                                        eprintln!("Remote input error from peer {peer_id}: {error:#}");
                                                    }
                                                }
                                                HostInput::Keyframe => {
                                                    broadcast.media().encoder.keyframe_trigger().request()
                                                }
                                                HostInput::Bitrate(mbps) => {
                                                    if !view_only
                                                        && (mode == HostControlMode::Collaborative
                                                            || broadcast.controller_id
                                                                == Some(peer_id))
                                                    {
                                                        broadcast
                                                            .media()
                                                            .encoder
                                                            .bitrate_control()
                                                            .set(mbps);
                                                    }
                                                }
                                                HostInput::Clipboard(data) => {
                                                    if !view_only {
                                                        event(HostEvent::Clipboard(data));
                                                    }
                                                }
                                                HostInput::FileReceived(file) => {
                                                    if !view_only {
                                                        event(HostEvent::FileReceived(file));
                                                    }
                                                }
                                            }
                                        }
                                        PeerEvent::Closed(peer_id, result) => {
                                            let Some((remaining, was_controller)) =
                                                broadcast.remove_peer(peer_id)
                                            else {
                                                continue;
                                            };
                                            if was_controller {
                                                let _ = broadcast.apply_remote_input(Input::Release);
                                            }
                                            if remaining == 0 {
                                                teardown_broadcast(
                                                    &mut active,
                                                    #[cfg(target_os = "linux")]
                                                    &mut wayland_portal,
                                                )
                                                .await;
                                                emit_empty_peers(&event);
                                                event(HostEvent::Finished(result));
                                                while attached_rx.try_recv().is_ok() {}
                                            } else {
                                                broadcast.emit_peers(&event);
                                            }
                                        }
                                    }
                                }
                                Some(data) = outgoing_clipboard.recv(), if !view_only => {
                                    broadcast.send_clipboard(data);
                                }
                                encoded = broadcast
                                    .media
                                    .as_mut()
                                    .expect("broadcast media is present while active")
                                    .encoder
                                    .units
                                    .recv() => {
                                    match encoded {
                                        Some(Ok(unit)) => {
                                            broadcast.fanout_video(unit);
                                        }
                                        Some(Err(error)) => {
                                            teardown_broadcast(
                                                &mut active,
                                                #[cfg(target_os = "linux")]
                                                &mut wayland_portal,
                                            )
                                            .await;
                                            emit_empty_peers(&event);
                                            event(HostEvent::Finished(Err(error)));
                                            while attached_rx.try_recv().is_ok() {}
                                        }
                                        None => {
                                            teardown_broadcast(
                                                &mut active,
                                                #[cfg(target_os = "linux")]
                                                &mut wayland_portal,
                                            )
                                            .await;
                                            emit_empty_peers(&event);
                                            event(HostEvent::Finished(Err(anyhow::anyhow!(
                                                "The desktop encoder stopped."
                                            ))));
                                            while attached_rx.try_recv().is_ok() {}
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        // Idle state: no viewers connected yet. Wait for the first viewer.
                        while attached_rx.try_recv().is_ok() {}
                        let result = tokio::select! {
                            _ = &mut stopped => break,
                            accepted = pending_rx.recv() => match accepted {
                                Some(result) => result,
                                None => break,
                            },
                        };
                        while outgoing_clipboard.try_recv().is_ok() {}
                        #[cfg(target_os = "linux")]
                        if crate::platform::wayland_portal::is_wayland_session()
                            && !matches!(target, HostDisplayTarget::Virtual(_))
                            && wayland_portal
                                .as_ref()
                                .is_none_or(|portal| portal.closed_reason().is_some())
                            && let Ok(portal) =
                                crate::platform::wayland_portal::WaylandPortalSession::open(
                                    !view_only,
                                )
                                .await
                        {
                            wayland_portal = Some(Arc::new(portal));
                        }
                        match result {
                            Ok(pending) => {
                                let mode = HostControlMode::from_u8(
                                    worker_control_mode.load(Ordering::Acquire),
                                );
                                let privacy_enabled = worker_privacy_screen.load(Ordering::Acquire);
                                let peer_id = next_peer_id;
                                next_peer_id += 1;
                                let init = tokio::select! {
                                    _ = &mut stopped => break,
                                    res = start_first_broadcast(
                                        pending,
                                        peer_id,
                                        target,
                                        view_only,
                                        mode,
                                        privacy_enabled,
                                        #[cfg(target_os = "linux")]
                                        &mut wayland_portal,
                                    ) => res,
                                };
                                match init {
                                    Ok(mut broadcast) => {
                                        // Reconcile any live toggle that raced with handshake setup.
                                        broadcast.sync_privacy_screen(
                                            worker_privacy_screen.load(Ordering::Acquire),
                                        );
                                        broadcast.emit_peers(&event);
                                        active = Some(broadcast);
                                    }
                                    Err(error) => {
                                        emit_empty_peers(&event);
                                        event(HostEvent::Finished(Err(error)));
                                    }
                                }
                            }
                            Err(error) => {
                                event(HostEvent::Finished(Err(error)));
                            }
                        }
                    }
                    accept_task.abort();
                    event(HostEvent::Stopped);
                });
            })?;
        Ok(Self {
            address,
            code,
            clipboard,
            control_mode: control_mode_state,
            privacy_screen: privacy_screen_state,
            privacy_notify,
            _stop: stop,
        })
    }
}

fn emit_empty_peers(event: &impl Fn(HostEvent)) {
    event(HostEvent::PeersChanged {
        peers: Vec::new(),
        controller: None,
        active_format: None,
    });
}

enum CaptureSource {
    Monitor(Monitor),
    #[cfg(target_os = "linux")]
    Wayland(Arc<crate::platform::wayland_portal::WaylandPortalSession>),
}

enum PeerEvent {
    Input(u64, HostInput),
    Closed(u64, Result<()>),
}

struct PeerAttached {
    peer_id: u64,
    addr: SocketAddr,
    can_control: bool,
    session: HostSession,
}

struct PeerHandle {
    addr: SocketAddr,
    can_control: bool,
    aux: AuxSender,
    video_tx: mpsc::Sender<AccessUnit>,
    video_task: TokioJoinHandle<()>,
    input_task: TokioJoinHandle<()>,
}

impl Drop for PeerHandle {
    fn drop(&mut self) {
        self.video_task.abort();
        self.input_task.abort();
    }
}

struct BroadcastMedia {
    encoder: Encoder,
    desktop: NativeDesktop,
    virtual_gamepads: Option<crate::gamepad::VirtualGamepadHost>,
    cursor_watcher: CursorWatcher,
    _audio_capture: HostAudioCapture,
}

struct ActiveBroadcast {
    format: Format,
    media: Option<BroadcastMedia>,
    virtual_display: Option<display::VirtualDisplay>,
    privacy_target: PrivacyTarget,
    privacy_screen: Option<PrivacyScreenGuard>,
    aux_peers: Arc<StdMutex<Vec<AuxSender>>>,
    peer_events_tx: mpsc::Sender<PeerEvent>,
    peer_events_rx: mpsc::Receiver<PeerEvent>,
    peer_order: Vec<u64>,
    peers: HashMap<u64, PeerHandle>,
    controller_id: Option<u64>,
    stats: HostStats,
}

impl ActiveBroadcast {
    fn sync_privacy_screen(&mut self, enabled: bool) {
        let changed = sync_privacy_guard(
            &mut self.privacy_screen,
            self.privacy_target,
            enabled,
            PrivacyScreenGuard::activate,
        );
        if changed && let Some(media) = self.media.as_ref() {
            media.encoder.keyframe_trigger().request();
        }
    }
    fn media(&self) -> &BroadcastMedia {
        self.media
            .as_ref()
            .expect("broadcast media is present while active")
    }

    fn media_mut(&mut self) -> &mut BroadcastMedia {
        self.media
            .as_mut()
            .expect("broadcast media is present while active")
    }

    fn add_peer(&mut self, attached: PeerAttached) {
        let PeerAttached {
            peer_id,
            addr,
            can_control,
            session,
        } = attached;
        let aux = session.aux_sender();
        if let Some(shape) = self.media().cursor_watcher.latest_shape() {
            let _ = aux.send_cursor(CursorUpdate::new(shape));
        }
        let (mut video, mut inputs) = session.split();
        let (video_tx, mut video_rx) = mpsc::channel::<AccessUnit>(PEER_VIDEO_QUEUE_CAPACITY);
        let close_tx = self.peer_events_tx.clone();
        let video_task = tokio::spawn(async move {
            let mut outcome = Ok(());
            while let Some(unit) = video_rx.recv().await {
                if let Err(error) = video.send(&unit).await {
                    outcome = Err(error);
                    break;
                }
            }
            let _ = close_tx.send(PeerEvent::Closed(peer_id, outcome)).await;
        });

        let input_tx = self.peer_events_tx.clone();
        let input_task = tokio::spawn(async move {
            let outcome = loop {
                match inputs.next_input().await {
                    Ok(Some(item)) => {
                        if input_tx
                            .send(PeerEvent::Input(peer_id, item))
                            .await
                            .is_err()
                        {
                            break Ok(());
                        }
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(error),
                }
            };
            let _ = input_tx.send(PeerEvent::Closed(peer_id, outcome)).await;
        });

        self.peer_order.push(peer_id);
        self.peers.insert(
            peer_id,
            PeerHandle {
                addr,
                can_control,
                aux,
                video_tx,
                video_task,
                input_task,
            },
        );
        self.sync_aux_peers();
        self.media().encoder.keyframe_trigger().request();
    }

    fn remove_peer(&mut self, peer_id: u64) -> Option<(usize, bool)> {
        self.peers.remove(&peer_id)?;
        self.peer_order.retain(|&id| id != peer_id);
        let was_controller = if self.controller_id == Some(peer_id) {
            self.controller_id = None;
            true
        } else {
            false
        };
        self.sync_aux_peers();
        Some((self.peers.len(), was_controller))
    }

    fn sync_aux_peers(&self) {
        let list: Vec<AuxSender> = self
            .peer_order
            .iter()
            .filter_map(|id| self.peers.get(id).map(|peer| peer.aux.clone()))
            .collect();
        *self
            .aux_peers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = list;
    }

    fn emit_peers(&self, event: &impl Fn(HostEvent)) {
        let peers: Vec<SocketAddr> = self
            .peer_order
            .iter()
            .filter_map(|id| self.peers.get(id).map(|peer| peer.addr))
            .collect();
        let controller = self
            .controller_id
            .and_then(|id| self.peers.get(&id).map(|peer| peer.addr));
        event(HostEvent::PeersChanged {
            peers,
            controller,
            active_format: Some((
                self.format.width,
                self.format.height,
                self.format.frames_per_second,
            )),
        });
    }

    fn fanout_video(&mut self, unit: AccessUnit) {
        let bytes = unit.bytes().len();
        let is_keyframe = unit.is_random_access();
        let mut any_sent = false;
        let mut need_keyframe = false;
        let started = Instant::now();
        for peer_id in &self.peer_order {
            if let Some(peer) = self.peers.get(peer_id) {
                if peer.video_tx.try_send(unit.clone()).is_ok() {
                    any_sent = true;
                } else {
                    need_keyframe = true;
                }
            }
        }
        if need_keyframe {
            self.media().encoder.keyframe_trigger().request();
        }
        self.stats.record(
            bytes,
            is_keyframe,
            any_sent,
            started.elapsed(),
            Duration::ZERO,
        );
    }

    fn send_clipboard(&self, data: ClipboardData) {
        for peer_id in &self.peer_order {
            if let Some(peer) = self.peers.get(peer_id) {
                let aux = peer.aux.clone();
                let payload = data.clone();
                tokio::spawn(async move {
                    let _ = aux.send_clipboard(&payload).await;
                });
            }
        }
    }

    fn apply_remote_input(&mut self, input: Input) -> Result<()> {
        let media = self.media_mut();
        match input {
            Input::Gamepad(state) => {
                if let Some(gamepads) = media.virtual_gamepads.as_mut() {
                    gamepads.apply(state);
                }
            }
            Input::Release => {
                if let Some(gamepads) = media.virtual_gamepads.as_mut() {
                    gamepads.reset();
                }
                media.desktop.input(Input::Release)?;
            }
            other => media.desktop.input(other)?,
        }
        Ok(())
    }
}

fn sync_privacy_guard(
    slot: &mut Option<PrivacyScreenGuard>,
    target: PrivacyTarget,
    enabled: bool,
    activate: impl FnOnce(PrivacyTarget) -> PrivacyScreenGuard,
) -> bool {
    if enabled {
        if slot.is_none() {
            *slot = Some(activate(target));
            return true;
        }
    } else if slot.take().is_some() {
        return true;
    }
    false
}

async fn teardown_broadcast(
    active: &mut Option<ActiveBroadcast>,
    #[cfg(target_os = "linux")] wayland_portal: &mut Option<
        Arc<crate::platform::wayland_portal::WaylandPortalSession>,
    >,
) {
    let Some(mut broadcast) = active.take() else {
        return;
    };
    let had_virtual_display = broadcast.virtual_display.is_some();
    // Restore local physical screens immediately before tearing down media and virtual display.
    drop(broadcast.privacy_screen.take());
    broadcast.peers.clear();
    broadcast.peer_order.clear();
    drop(broadcast.media.take());
    #[cfg(target_os = "linux")]
    if had_virtual_display && crate::platform::wayland_portal::is_wayland_session() {
        if let Some(portal) = wayland_portal.take() {
            portal.shutdown().await;
        }
        // Let Mutter finish closing the ScreenCast PipeWire node before gdctl restores mode.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = had_virtual_display;
    drop(broadcast.virtual_display.take());
}

#[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
async fn start_first_broadcast(
    pending: PendingHostSession,
    peer_id: u64,
    target: HostDisplayTarget,
    view_only: bool,
    control_mode: HostControlMode,
    privacy_screen: bool,
    #[cfg(target_os = "linux")] wayland_portal: &mut Option<
        Arc<crate::platform::wayland_portal::WaylandPortalSession>,
    >,
) -> Result<ActiveBroadcast> {
    let pref = pending.stream_preference();
    let requested_codec = codec::resolve_host_codec(pref.codec, pref.supported_codecs);
    let requested_hdr = codec::resolve_hdr_enabled(requested_codec, pref.hdr);
    let (format, mut capture_source, virtual_display) = {
        #[cfg(target_os = "linux")]
        let wayland_mode = crate::platform::wayland_portal::is_wayland_session();
        #[cfg(not(target_os = "linux"))]
        let wayland_mode = false;

        if wayland_mode {
            #[cfg(target_os = "linux")]
            {
                match target {
                    HostDisplayTarget::Fixed {
                        format: fixed_fmt, ..
                    } => {
                        let portal = match wayland_portal
                            .as_ref()
                            .filter(|p| p.closed_reason().is_none())
                            .cloned()
                        {
                            Some(portal) => portal,
                            None => {
                                pending.reject(b"display unavailable");
                                anyhow::bail!("Wayland screen sharing portal is unavailable.");
                            }
                        };
                        let (w, h, base_fps) = crate::platform::linux::linux_primary_display_spec()
                            .map(|(_, w, h, fps)| (w, h, fps))
                            .or_else(|| {
                                portal
                                    .dimensions()
                                    .map(|(w, h)| (w, h, fixed_fmt.frames_per_second))
                            })
                            .unwrap_or((
                                u32::from(fixed_fmt.width),
                                u32::from(fixed_fmt.height),
                                fixed_fmt.frames_per_second,
                            ));
                        let fps = match pref.max_fps {
                            Some(cap) => base_fps.min(cap.clamp(30, 120)),
                            None => base_fps,
                        };
                        let format = codec::virtual_format_with_hdr(
                            w,
                            h,
                            fps,
                            requested_codec,
                            requested_hdr,
                        )?;
                        (format, CaptureSource::Wayland(portal), None)
                    }
                    HostDisplayTarget::Virtual(resolution) => {
                        let mut spec = match resolution {
                            VirtualResolution::Adaptive => pending
                                .display_preference()
                                .map(VirtualSpec::from_preference)
                                .unwrap_or(VirtualSpec::DEFAULT_FALLBACK),
                            VirtualResolution::Fixed(spec) => {
                                match VirtualSpec::sanitized(spec.width, spec.height, spec.fps) {
                                    Ok(spec) => spec,
                                    Err(error) => {
                                        pending.reject(b"invalid display format");
                                        return Err(error);
                                    }
                                }
                            }
                        };
                        if let Some(cap) = pref.max_fps {
                            spec.fps = spec.fps.min(u32::from(cap.clamp(30, 120)));
                        }
                        // Close any active portal session before gdctl mutates monitors so Mutter
                        // does not tear down an active ScreenCast node or crash ClutterActor.
                        if let Some(old_portal) = wayland_portal.take() {
                            old_portal.shutdown().await;
                        }
                        let applied = tokio::task::spawn_blocking(move || {
                            display::apply_linux_virtual_mode(spec)
                        })
                        .await;
                        let (virtual_display, actual_w, actual_h, actual_fps) = match applied {
                            Ok(Ok(tuple)) => tuple,
                            Ok(Err(error)) => {
                                pending.reject(b"virtual display unavailable");
                                return Err(error);
                            }
                            Err(error) => {
                                pending.reject(b"virtual display failed");
                                return Err(error.into());
                            }
                        };
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        let portal =
                            match crate::platform::wayland_portal::WaylandPortalSession::open(
                                !view_only,
                            )
                            .await
                            {
                                Ok(portal) => {
                                    let arc = Arc::new(portal);
                                    *wayland_portal = Some(arc.clone());
                                    arc
                                }
                                Err(error) => {
                                    drop(virtual_display);
                                    pending.reject(b"display unavailable");
                                    return Err(error);
                                }
                            };
                        let fps = resolve_virtual_stream_fps(spec.fps, actual_fps, pref.max_fps);
                        let format = codec::virtual_format_with_hdr(
                            actual_w,
                            actual_h,
                            fps,
                            requested_codec,
                            requested_hdr,
                        )?;
                        (
                            format,
                            CaptureSource::Wayland(portal),
                            Some(virtual_display),
                        )
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            unreachable!()
        } else {
            match target {
                HostDisplayTarget::Fixed { display_id, format } => {
                    let monitor = match display::monitor_by_id(display_id) {
                        Ok(monitor) => monitor,
                        Err(error) => {
                            pending.reject(b"display unavailable");
                            return Err(error);
                        }
                    };
                    let fps = match pref.max_fps {
                        Some(cap) => format.frames_per_second.min(cap.clamp(30, 120)),
                        None => format.frames_per_second,
                    };
                    let format = Format {
                        codec: requested_codec,
                        width: format.width,
                        height: format.height,
                        frames_per_second: fps,
                        hdr: requested_hdr,
                    };
                    if let Err(error) = format.validate() {
                        pending.reject(b"invalid display format");
                        return Err(error);
                    }
                    (format, CaptureSource::Monitor(monitor), None)
                }
                HostDisplayTarget::Virtual(resolution) => {
                    let mut spec = match resolution {
                        VirtualResolution::Adaptive => pending
                            .display_preference()
                            .map(VirtualSpec::from_preference)
                            .unwrap_or(VirtualSpec::DEFAULT_FALLBACK),
                        VirtualResolution::Fixed(spec) => {
                            match VirtualSpec::sanitized(spec.width, spec.height, spec.fps) {
                                Ok(spec) => spec,
                                Err(error) => {
                                    pending.reject(b"invalid display format");
                                    return Err(error);
                                }
                            }
                        }
                    };
                    if let Some(cap) = pref.max_fps {
                        spec.fps = spec.fps.min(u32::from(cap.clamp(30, 120)));
                    }
                    let format = match codec::virtual_format_with_hdr(
                        spec.width,
                        spec.height,
                        spec.fps as u16,
                        requested_codec,
                        requested_hdr,
                    ) {
                        Ok(format) => format,
                        Err(error) => {
                            pending.reject(b"invalid display format");
                            return Err(error);
                        }
                    };
                    let created = tokio::task::spawn_blocking(move || {
                        let (monitor, virtual_display) = display::create_virtual(spec)?;
                        Ok::<_, anyhow::Error>((monitor.id()?, virtual_display))
                    })
                    .await;
                    let (monitor, virtual_display) = match created {
                        Ok(Ok((id, virtual_display))) => match display::monitor_by_id(id) {
                            Ok(monitor) => (monitor, virtual_display),
                            Err(error) => {
                                pending.reject(b"virtual display unavailable");
                                return Err(error);
                            }
                        },
                        Ok(Err(error)) => {
                            pending.reject(b"virtual display unavailable");
                            return Err(error);
                        }
                        Err(error) => {
                            pending.reject(b"virtual display failed");
                            return Err(error.into());
                        }
                    };
                    #[cfg(target_os = "linux")]
                    let format = codec::desktop_format_with_hdr(
                        &monitor,
                        false,
                        requested_codec,
                        Some(resolve_virtual_stream_fps(
                            spec.fps,
                            spec.fps as u16,
                            pref.max_fps,
                        )),
                        requested_hdr,
                    )
                    .unwrap_or(format);
                    (
                        format,
                        CaptureSource::Monitor(monitor),
                        Some(virtual_display),
                    )
                }
            }
        }
    };

    #[cfg(target_os = "macos")]
    let (audio_tx, audio_rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(8);

    let first_attempt = try_start_encoder(
        &capture_source,
        format,
        pref,
        #[cfg(target_os = "macos")]
        &audio_tx,
    );
    let (format, encoder) = match first_attempt {
        Ok(pair) => pair,
        Err(first_error) => {
            #[cfg(target_os = "linux")]
            if matches!(capture_source, CaptureSource::Wayland(_)) {
                drop(capture_source);
                if let Some(old_portal) = wayland_portal.take() {
                    old_portal.shutdown().await;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                let fresh =
                    match crate::platform::wayland_portal::WaylandPortalSession::open(!view_only)
                        .await
                    {
                        Ok(fresh) => fresh,
                        Err(open_err) => {
                            pending.reject(b"encoder unavailable");
                            return Err(open_err);
                        }
                    };
                let arc = Arc::new(fresh);
                *wayland_portal = Some(arc.clone());
                capture_source = CaptureSource::Wayland(arc);
                match try_start_encoder(&capture_source, format, pref) {
                    Ok(pair) => pair,
                    Err(retry_err) => {
                        pending.reject(b"encoder unavailable");
                        return Err(retry_err);
                    }
                }
            } else {
                pending.reject(b"encoder unavailable");
                return Err(first_error);
            }
            #[cfg(not(target_os = "linux"))]
            {
                pending.reject(b"encoder unavailable");
                return Err(first_error);
            }
        }
    };

    let (can_control, becomes_controller) = resolve_new_peer_access(view_only, control_mode, false);
    let peer_view_only = !can_control;
    let receive_dir = (!peer_view_only).then(crate::clipboard::remote_receive_dir);
    let addr = pending.peer_address();
    let session = pending
        .start_with_options(format, Some(peer_view_only), receive_dir)
        .await?;

    let captured_display_id = match &capture_source {
        CaptureSource::Monitor(monitor) => monitor.id().ok(),
        #[cfg(target_os = "linux")]
        CaptureSource::Wayland(_) => None,
    };
    let privacy_target = PrivacyTarget {
        is_virtual_display: virtual_display.is_some(),
        captured_display_id,
    };
    let privacy_guard = privacy_screen.then(|| PrivacyScreenGuard::activate(privacy_target));
    if privacy_guard.is_some() {
        encoder.keyframe_trigger().request();
    }

    let desktop = match capture_source {
        CaptureSource::Monitor(monitor) => NativeDesktop::new(view_only, monitor)?,
        #[cfg(target_os = "linux")]
        CaptureSource::Wayland(portal) => NativeDesktop::new_wayland(
            view_only,
            portal,
            u32::from(format.width),
            u32::from(format.height),
        )?,
    };
    let virtual_gamepads = (!view_only).then(crate::gamepad::VirtualGamepadHost::new);
    let aux_peers = Arc::new(StdMutex::new(Vec::new()));
    let cursor_watcher = CursorWatcher::start(aux_peers.clone())?;
    let audio_peers = aux_peers.clone();
    let audio_sink: AudioSink = Arc::new(move |frame| {
        let senders = audio_peers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for aux in senders {
            let _ = aux.send_audio(frame.clone());
        }
    });
    #[cfg(target_os = "macos")]
    let audio_capture = HostAudioCapture::start_macos(audio_sink, audio_rx);
    #[cfg(target_os = "windows")]
    let audio_capture = HostAudioCapture::start_windows(audio_sink);
    #[cfg(target_os = "linux")]
    let audio_capture = HostAudioCapture::start_linux(audio_sink);

    let (peer_events_tx, peer_events_rx) = mpsc::channel(256);
    let mut broadcast = ActiveBroadcast {
        format,
        media: Some(BroadcastMedia {
            encoder,
            desktop,
            virtual_gamepads,
            cursor_watcher,
            _audio_capture: audio_capture,
        }),
        virtual_display,
        privacy_target,
        privacy_screen: privacy_guard,
        aux_peers,
        peer_events_tx,
        peer_events_rx,
        peer_order: Vec::new(),
        peers: HashMap::new(),
        controller_id: becomes_controller.then_some(peer_id),
        stats: HostStats::new(),
    };
    broadcast.add_peer(PeerAttached {
        peer_id,
        addr,
        can_control,
        session,
    });
    Ok(broadcast)
}

fn try_start_encoder(
    capture_source: &CaptureSource,
    format: Format,
    pref: StreamPreference,
    #[cfg(target_os = "macos")] audio_tx: &std::sync::mpsc::SyncSender<Vec<i16>>,
) -> Result<(Format, Encoder)> {
    let mut last_error = None;
    for candidate in encoder_candidates(format, pref.supported_codecs, pref.hdr) {
        let attempt = match capture_source {
            CaptureSource::Monitor(monitor) => {
                #[cfg(target_os = "macos")]
                {
                    Encoder::start_with_stream(
                        candidate,
                        monitor,
                        Some(audio_tx.clone()),
                        pref.target_bitrate_mbps,
                    )
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Encoder::start_with_bitrate(candidate, monitor, pref.target_bitrate_mbps)
                }
            }
            #[cfg(target_os = "linux")]
            CaptureSource::Wayland(portal) => {
                Encoder::start_wayland(candidate, portal.clone(), pref.target_bitrate_mbps)
            }
        };
        match attempt {
            Ok(encoder) => return Ok((candidate, encoder)),
            Err(error) => {
                eprintln!(
                    "Encoder unavailable for {:?} (hdr={}): {error:#}; trying fallback",
                    candidate.codec, candidate.hdr
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Encoder unavailable.")))
}

fn encoder_candidates(
    initial: Format,
    viewer_supported_mask: u8,
    requested_hdr: bool,
) -> Vec<Format> {
    let mut candidates = vec![initial];
    if initial.hdr {
        candidates.push(Format {
            hdr: false,
            ..initial
        });
    }
    if initial.codec == Codec::Av1 {
        let viewer_allows_hevc =
            viewer_supported_mask == 0 || (viewer_supported_mask & CODEC_BIT_HEVC) != 0;
        if viewer_allows_hevc && codec::supports_hevc_encode() {
            if codec::resolve_hdr_enabled(Codec::Hevc, requested_hdr) {
                candidates.push(Format {
                    codec: Codec::Hevc,
                    hdr: true,
                    ..initial
                });
            }
            candidates.push(Format {
                codec: Codec::Hevc,
                hdr: false,
                ..initial
            });
        }
    }
    if initial.codec != Codec::H264 {
        candidates.push(Format {
            codec: Codec::H264,
            hdr: false,
            ..initial
        });
    }
    candidates
}

pub(crate) fn viewer_supports_active_format(active: Format, pref: StreamPreference) -> bool {
    if active.hdr && !pref.hdr {
        return false;
    }
    if pref.supported_codecs == 0 {
        return true;
    }
    let effective_mask = pref.supported_codecs | CODEC_BIT_H264;
    let active_bit = match active.codec {
        Codec::H264 => CODEC_BIT_H264,
        Codec::Hevc => CODEC_BIT_HEVC,
        Codec::Av1 => CODEC_BIT_AV1,
    };
    (effective_mask & active_bit) != 0
}

pub(crate) fn resolve_new_peer_access(
    host_view_only: bool,
    mode: HostControlMode,
    has_controller: bool,
) -> (bool, bool) {
    if host_view_only {
        return (false, false);
    }
    match mode {
        HostControlMode::Collaborative => (true, !has_controller),
        HostControlMode::SingleController => {
            let becomes_controller = !has_controller;
            (becomes_controller, becomes_controller)
        }
    }
}

pub(crate) fn allows_peer_input(
    host_view_only: bool,
    mode: HostControlMode,
    controller_id: Option<u64>,
    peer_id: u64,
    peer_can_control: bool,
) -> bool {
    if host_view_only || !peer_can_control {
        return false;
    }
    match mode {
        HostControlMode::Collaborative => true,
        HostControlMode::SingleController => controller_id == Some(peer_id),
    }
}

struct CursorWatcher {
    stop: Arc<AtomicBool>,
    latest: Arc<StdMutex<Option<CursorShape>>>,
    worker: Option<JoinHandle<()>>,
}

impl CursorWatcher {
    fn start(aux_peers: Arc<StdMutex<Vec<AuxSender>>>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let latest = Arc::new(StdMutex::new(None));
        let worker_latest = latest.clone();
        let worker = thread::Builder::new()
            .name("host-cursor".into())
            .spawn(move || {
                let detector = codec::CursorDetector::new();
                let mut last: Option<CursorShape> = None;
                while !worker_stop.load(Ordering::Acquire) {
                    let shape = detector.poll();
                    if last != Some(shape) {
                        last = Some(shape);
                        *worker_latest
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(shape);
                        let senders = aux_peers
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone();
                        for aux in senders {
                            let _ = aux.send_cursor(CursorUpdate::new(shape));
                        }
                    }
                    thread::park_timeout(Duration::from_millis(20));
                }
            })?;
        Ok(Self {
            stop,
            latest,
            worker: Some(worker),
        })
    }

    fn latest_shape(&self) -> Option<CursorShape> {
        *self
            .latest
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Drop for CursorWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

/// Computes the stream/encoding FPS for a virtual display session so that Apollo-style
/// 2x virtual display refresh rates (e.g. 120Hz display for a 60fps stream) do not
/// accidentally double the encoded stream framerate.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn resolve_virtual_stream_fps(
    target_spec_fps: u32,
    actual_display_fps: u16,
    max_fps: Option<u16>,
) -> u16 {
    let target = target_spec_fps.clamp(30, 120) as u16;
    let capped = match max_fps {
        Some(cap) => target.min(cap.clamp(30, 120)),
        None => target,
    };
    actual_display_fps.clamp(30, 120).min(capped)
}

#[cfg(test)]
mod tests {
    use remoters_core::{
        session::{CODEC_BIT_AV1, CODEC_BIT_H264, CODEC_BIT_HEVC, StreamPreference},
        video::{Codec, Format},
    };

    use super::{
        HostControlMode, allows_peer_input, resolve_new_peer_access, resolve_virtual_stream_fps,
        sync_privacy_guard, viewer_supports_active_format,
    };
    use crate::privacy::{PrivacyScreenGuard, PrivacyTarget};

    #[test]
    fn privacy_guard_sync_activates_toggles_and_restores_cleanly() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        let activations = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let target = PrivacyTarget {
            is_virtual_display: true,
            captured_display_id: Some(42),
        };
        let mut slot: Option<PrivacyScreenGuard> = None;

        let make_guard = |t: PrivacyTarget,
                          acts: &Arc<AtomicUsize>,
                          drps: &Arc<AtomicUsize>|
         -> PrivacyScreenGuard {
            acts.fetch_add(1, Ordering::SeqCst);
            let d = drps.clone();
            PrivacyScreenGuard::new_for_test(
                t,
                vec![700_001],
                Box::new(move || {
                    d.fetch_add(1, Ordering::SeqCst);
                }),
            )
        };

        // Turning off when already off is a no-op.
        assert!(!sync_privacy_guard(&mut slot, target, false, |t| {
            make_guard(t, &activations, &drops)
        }));
        assert!(slot.is_none());
        assert_eq!(activations.load(Ordering::SeqCst), 0);

        // Turning on engages the guard once.
        assert!(sync_privacy_guard(&mut slot, target, true, |t| {
            make_guard(t, &activations, &drops)
        }));
        assert!(slot.is_some());
        assert_eq!(activations.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        // Repeatedly setting enabled=true does not re-create the guard.
        assert!(!sync_privacy_guard(&mut slot, target, true, |t| {
            make_guard(t, &activations, &drops)
        }));
        assert_eq!(activations.load(Ordering::SeqCst), 1);

        // Toggling off drops the guard immediately.
        assert!(sync_privacy_guard(&mut slot, target, false, |t| {
            make_guard(t, &activations, &drops)
        }));
        assert!(slot.is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        // Re-enabling and then dropping the slot cleans up on teardown.
        assert!(sync_privacy_guard(&mut slot, target, true, |t| {
            make_guard(t, &activations, &drops)
        }));
        assert_eq!(activations.load(Ordering::SeqCst), 2);
        drop(slot);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn virtual_stream_fps_keeps_target_when_display_runs_at_double_refresh() {
        // 60fps stream on a 120Hz virtual display stays at 60fps encoding.
        assert_eq!(resolve_virtual_stream_fps(60, 120, None), 60);
        // 30fps stream on a 60Hz virtual display stays at 30fps encoding.
        assert_eq!(resolve_virtual_stream_fps(30, 60, None), 30);
        // If monitor only supports 60Hz when 120fps was requested, cap to actual 60Hz.
        assert_eq!(resolve_virtual_stream_fps(120, 60, None), 60);
        // Explicit viewer max_fps cap still applies.
        assert_eq!(resolve_virtual_stream_fps(120, 120, Some(90)), 90);
    }

    #[test]
    fn host_control_mode_and_permission_gating_work_across_modes() {
        assert_eq!(
            HostControlMode::from_u8(HostControlMode::Collaborative.as_u8()),
            HostControlMode::Collaborative
        );
        assert_eq!(
            HostControlMode::from_u8(HostControlMode::SingleController.as_u8()),
            HostControlMode::SingleController
        );

        // Collaborative (default): all peers can control simultaneously unless host is view-only.
        assert_eq!(
            resolve_new_peer_access(false, HostControlMode::Collaborative, false),
            (true, true)
        );
        assert_eq!(
            resolve_new_peer_access(false, HostControlMode::Collaborative, true),
            (true, false)
        );
        assert!(allows_peer_input(
            false,
            HostControlMode::Collaborative,
            Some(1),
            2,
            true
        ));

        // SingleController: 1st peer controls, 2nd peer is view-only; after 1st leaves, 3rd promotes.
        assert_eq!(
            resolve_new_peer_access(false, HostControlMode::SingleController, false),
            (true, true)
        );
        assert_eq!(
            resolve_new_peer_access(false, HostControlMode::SingleController, true),
            (false, false)
        );
        assert!(allows_peer_input(
            false,
            HostControlMode::SingleController,
            Some(1),
            1,
            true
        ));
        assert!(!allows_peer_input(
            false,
            HostControlMode::SingleController,
            Some(1),
            2,
            false
        ));

        // Host view-only overrides both modes.
        assert_eq!(
            resolve_new_peer_access(true, HostControlMode::Collaborative, false),
            (false, false)
        );
        assert!(!allows_peer_input(
            true,
            HostControlMode::Collaborative,
            Some(1),
            1,
            true
        ));
    }

    #[test]
    fn viewer_compatibility_checks_codec_mask_and_hdr() {
        let h264_sdr = Format {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            frames_per_second: 60,
            hdr: false,
        };
        let hevc_hdr = Format {
            codec: Codec::Hevc,
            hdr: true,
            ..h264_sdr
        };
        let av1_sdr = Format {
            codec: Codec::Av1,
            hdr: false,
            ..h264_sdr
        };

        assert!(viewer_supports_active_format(
            h264_sdr,
            StreamPreference::default()
        ));
        assert!(viewer_supports_active_format(
            av1_sdr,
            StreamPreference {
                supported_codecs: CODEC_BIT_H264 | CODEC_BIT_HEVC | CODEC_BIT_AV1,
                ..StreamPreference::default()
            }
        ));
        assert!(!viewer_supports_active_format(
            av1_sdr,
            StreamPreference {
                supported_codecs: CODEC_BIT_H264 | CODEC_BIT_HEVC,
                ..StreamPreference::default()
            }
        ));
        assert!(!viewer_supports_active_format(
            hevc_hdr,
            StreamPreference {
                supported_codecs: CODEC_BIT_H264 | CODEC_BIT_HEVC,
                hdr: false,
                ..StreamPreference::default()
            }
        ));
        assert!(viewer_supports_active_format(
            hevc_hdr,
            StreamPreference {
                supported_codecs: CODEC_BIT_H264 | CODEC_BIT_HEVC,
                hdr: true,
                ..StreamPreference::default()
            }
        ));
    }
}
