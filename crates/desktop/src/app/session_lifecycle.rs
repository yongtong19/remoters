use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::mpsc::channel,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, ensure};
use gpui::{Context, Window};
use gpui_component::IndexPath;
use remoters_core::{
    Input,
    account::DeviceRecord,
    network::{
        self, Pairing,
        discovery::{
            DiscoveryBeacon, HostAdvertisement, InterfaceAddress, local_hostname,
            random_instance_id,
        },
    },
    session::CursorShape,
};

use crate::{
    account::LocalSharingSnapshot,
    account_ui::{AccountSelection, parse_device_connect_target},
    clipboard, codec, i18n, metrics,
    platform::{
        self,
        display::{self, DisplaySource, ViewerResolution, VirtualResolution},
    },
    sharing::{self, HostDisplayTarget, HostEvent},
    viewer,
};

use super::{
    discovery::{host_platform, local_addresses, local_interface_addresses},
    state::{
        ActiveTab, Desktop, Role, SessionReconnectParams, SessionTab, Sharing, Viewer,
        build_reconnect_stream_preference, effective_audio_muted, find_live_session_by_target,
        next_active_tab_after_close, session_target_key, update_reconnect_params,
    },
};

const RECONNECT_SETTLE_DELAY: Duration = Duration::from_millis(220);

impl Desktop {
    #[allow(clippy::too_many_arguments)]
    fn start_viewer_session(
        &mut self,
        reconnect: &SessionReconnectParams,
        pairing: Pairing,
        bitrate_mbps: Option<u16>,
        frame_pacing: viewer::FramePacingMode,
        audio_muted: bool,
        window: &mut Window,
    ) -> Result<Viewer> {
        let stream_preference = build_reconnect_stream_preference(
            reconnect,
            bitrate_mbps,
            display::resolve_viewer_preference,
        );
        let connection = viewer::Connection::start_with_stream_preference(
            reconnect.address,
            pairing,
            stream_preference,
            self.wake.clone(),
            window,
        )?;
        connection.set_frame_pacing(frame_pacing);
        connection.set_audio_muted(audio_muted);
        Ok(Viewer {
            address: reconnect.address,
            connection,
            can_control: false,
            auto_control_armed: true,
            frame: None,
            remote_cursor: CursorShape::Arrow,
            audio_muted,
            quality_expanded: false,
            resolution: reconnect.resolution,
            fps_cap: reconnect.fps_cap,
            active_bitrate_mbps: bitrate_mbps,
            frame_pacing,
            telemetry: metrics::SessionTelemetry::default(),
            transfer_notice: None,
            controlling: false,
            session_menu_open: false,
            stats_section_open: true,
            stats_hud_open: false,
            menu_pos: None,
            menu_drag: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn open_or_focus_session(
        &mut self,
        title: String,
        target_key: String,
        address: SocketAddrV4,
        pairing_code: String,
        pairing: Pairing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<u64> {
        if let Some(index) = find_live_session_by_target(&self.sessions, &target_key, |tab| {
            (&tab.target_key, tab.closed_notice.is_some())
        }) {
            let existing_id = self.sessions[index].id;
            self.activate_tab(ActiveTab::Session(existing_id), window, cx);
            return Ok(existing_id);
        }
        // Remove any previously closed tab for the exact same target before reconnecting.
        self.sessions
            .retain(|tab| !(tab.target_key == target_key && tab.closed_notice.is_some()));

        let resolution = self
            .viewer_resolution
            .read(cx)
            .selected_value()
            .copied()
            .unwrap_or(ViewerResolution::MatchLocal);
        let reconnect = SessionReconnectParams {
            address,
            pairing_code,
            title: title.clone(),
            codec_preference: self.codec_preference,
            hdr_enabled: self.hdr_enabled,
            resolution,
            fps_cap: self.fps_cap,
        };
        let viewer = self.start_viewer_session(
            &reconnect,
            pairing,
            self.bitrate_mbps,
            self.frame_pacing,
            false,
            window,
        )?;
        let id = self.next_session_id;
        self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
        self.release_active_input();
        self.sessions.push(SessionTab {
            id,
            title,
            target_key,
            reconnect,
            viewer,
            closed_notice: None,
        });
        self.active_tab = ActiveTab::Session(id);
        self.sync_session_audio_mute();
        Ok(id)
    }

    pub(crate) fn set_active_resolution(
        &mut self,
        resolution: ViewerResolution,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reconnect_active_session_with_preferences(Some(resolution), None, window, cx);
    }

    pub(crate) fn set_active_fps_cap(
        &mut self,
        fps_cap: Option<u16>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reconnect_active_session_with_preferences(None, Some(fps_cap), window, cx);
    }

    fn reconnect_active_session_with_preferences(
        &mut self,
        next_resolution: Option<ViewerResolution>,
        next_fps_cap: Option<Option<u16>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ActiveTab::Session(tab_id) = self.active_tab else {
            return;
        };
        let Some(index) = self.sessions.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        if self.sessions[index].closed_notice.is_some() {
            return;
        }
        let mut next_reconnect = self.sessions[index].reconnect.clone();
        if !update_reconnect_params(&mut next_reconnect, next_resolution, next_fps_cap) {
            return;
        }
        let pairing = match Pairing::parse(&next_reconnect.pairing_code) {
            Ok(pairing) => pairing,
            Err(err) => {
                self.sessions[index].viewer.transfer_notice =
                    Some((format!("{err:#}"), true, false, Instant::now()));
                cx.notify();
                return;
            }
        };

        let audio_muted = self.sessions[index].viewer.audio_muted;
        let active_bitrate_mbps = self.sessions[index].viewer.active_bitrate_mbps;
        let frame_pacing = self.sessions[index].viewer.frame_pacing;
        let stats_section_open = self.sessions[index].viewer.stats_section_open;
        let stats_hud_open = self.sessions[index].viewer.stats_hud_open;
        let quality_expanded = self.sessions[index].viewer.quality_expanded;
        let menu_pos = self.sessions[index].viewer.menu_pos;
        let auto_control = self.sessions[index].viewer.controlling
            || self.sessions[index].viewer.auto_control_armed;

        self.release_active_input();

        // Remove and drop the old viewer connection first so the host finishes tearing down the
        // previous broadcast and renegotiates the newly requested resolution / max FPS cleanly.
        let old_tab = self.sessions.remove(index);
        let SessionTab {
            id,
            title,
            target_key,
            viewer: old_viewer,
            ..
        } = old_tab;
        drop(old_viewer);
        thread::sleep(RECONNECT_SETTLE_DELAY);

        match self.start_viewer_session(
            &next_reconnect,
            pairing,
            active_bitrate_mbps,
            frame_pacing,
            audio_muted,
            window,
        ) {
            Ok(mut viewer) => {
                viewer.auto_control_armed = auto_control;
                viewer.stats_section_open = stats_section_open;
                viewer.stats_hud_open = stats_hud_open;
                viewer.quality_expanded = quality_expanded;
                viewer.menu_pos = menu_pos;
                self.sessions.insert(
                    index,
                    SessionTab {
                        id,
                        title,
                        target_key,
                        reconnect: next_reconnect,
                        viewer,
                        closed_notice: None,
                    },
                );
                self.active_tab = ActiveTab::Session(id);
                self.last_clipboard_hash = None;
                self.sync_session_audio_mute();
            }
            Err(err) => {
                self.status = Some((format!("{err:#}"), true));
                self.active_tab = ActiveTab::Home;
            }
        }
        cx.notify();
    }

    pub(crate) fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw_address = self.address.read(cx).value().to_string();
        let raw_code = self.code.read(cx).value().to_string();
        let result: Result<u64> = (|| {
            let address = network::endpoint(&raw_address)?;
            let pairing = Pairing::parse(&raw_code)?;
            let title = self
                .discovered
                .iter()
                .find(|host| host.address == address)
                .map(|host| host.host_name.clone())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| address.ip().to_string());
            let target_key = session_target_key(address, &raw_code);
            self.open_or_focus_session(title, target_key, address, raw_code, pairing, window, cx)
        })();
        match result {
            Ok(_) => {
                self.status = None;
            }
            Err(err) => self.status = Some((format!("{err:#}"), true)),
        }
        cx.notify();
    }

    pub(crate) fn connect_to_account_device(
        &mut self,
        device: &DeviceRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result: Result<u64> = (|| {
            let target = parse_device_connect_target(device)?;
            let title = if device.name.trim().is_empty() {
                target.address.ip().to_string()
            } else {
                device.name.trim().to_string()
            };
            let raw_code = device.pairing_code.as_deref().unwrap_or("");
            let target_key = session_target_key(target.address, raw_code);
            self.open_or_focus_session(
                title,
                target_key,
                target.address,
                raw_code.to_string(),
                target.pairing,
                window,
                cx,
            )
        })();
        match result {
            Ok(_) => {
                self.account.selection = AccountSelection::None;
                self.status = None;
            }
            Err(err) => self.status = Some((format!("{err:#}"), true)),
        }
        cx.notify();
    }

    pub(crate) fn release_active_input(&mut self) {
        let active_id = match self.active_tab {
            ActiveTab::Home => None,
            ActiveTab::Session(id) => Some(id),
        };
        if let Some(id) = active_id
            && let Some(tab) = self.sessions.iter_mut().find(|t| t.id == id)
        {
            if tab.viewer.controlling
                || !self.pressed_buttons.is_empty()
                || self.modifiers.iter().any(|down| *down)
            {
                let _ = tab.viewer.connection.input(Input::Release);
            }
            tab.viewer.controlling = false;
            tab.viewer.session_menu_open = false;
            tab.viewer.menu_drag = None;
        }
        self.pressed_buttons.clear();
        self.modifiers = [false; 4];
        self.last_sent_move = None;
        self.screen_bounds.set(None);
    }

    pub(crate) fn sync_session_audio_mute(&self) {
        for tab in &self.sessions {
            let muted = effective_audio_muted(
                tab.id,
                tab.viewer.audio_muted,
                tab.closed_notice.is_some(),
                self.active_tab,
            );
            tab.viewer.connection.set_audio_muted(muted);
        }
    }

    pub(crate) fn activate_tab(
        &mut self,
        next: ActiveTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_tab == next {
            return;
        }
        self.release_active_input();
        self.active_tab = next;
        self.last_clipboard_hash = None;
        if let ActiveTab::Session(id) = next {
            if let Some(tab) = self.sessions.iter_mut().find(|t| t.id == id) {
                tab.viewer.telemetry = tab.viewer.connection.telemetry();
                if tab.closed_notice.is_none()
                    && tab.viewer.frame.is_some()
                    && tab.viewer.can_control
                    && window.is_window_active()
                {
                    tab.viewer.controlling = true;
                    tab.viewer.auto_control_armed = false;
                    window.focus(&self.remote_focus);
                }
            }
        } else if window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        self.sync_session_audio_mute();
        cx.notify();
    }

    pub(crate) fn open_new_session_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab(ActiveTab::Home, window, cx);
        if self.idle() {
            self.role = Role::Connect;
            self.account.selection = AccountSelection::None;
            self.status = None;
        }
        cx.notify();
    }

    pub(crate) fn close_session_tab(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.sessions.iter().position(|tab| tab.id == id) else {
            return;
        };
        let closing_active = self.active_tab == ActiveTab::Session(id);
        if closing_active {
            self.release_active_input();
        }
        let ids: Vec<u64> = self.sessions.iter().map(|tab| tab.id).collect();
        let next_tab = next_active_tab_after_close(&ids, index, self.active_tab);
        self.sessions.remove(index);
        self.active_tab = next_tab;
        if closing_active {
            self.last_clipboard_hash = None;
            if let ActiveTab::Session(next_id) = next_tab {
                if let Some(tab) = self.sessions.iter_mut().find(|t| t.id == next_id) {
                    tab.viewer.telemetry = tab.viewer.connection.telemetry();
                    if tab.closed_notice.is_none()
                        && tab.viewer.frame.is_some()
                        && tab.viewer.can_control
                        && window.is_window_active()
                    {
                        tab.viewer.controlling = true;
                        tab.viewer.auto_control_armed = false;
                        window.focus(&self.remote_focus);
                    }
                }
            } else if window.is_fullscreen() {
                window.toggle_fullscreen();
            }
        }
        self.sync_session_audio_mute();
        cx.notify();
    }

    pub(crate) fn close_active_session_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let ActiveTab::Session(id) = self.active_tab {
            self.close_session_tab(id, window, cx);
        }
    }

    pub(crate) fn sync_account_sharing(&self) {
        let snapshot = match &self.sharing {
            Some(sharing) => LocalSharingSnapshot {
                sharing: true,
                bind_address: Some(sharing.host.address),
                pairing_code: Some(sharing.host.code.clone()),
                view_only: self.view_only,
                display_label: sharing.display_summary.clone(),
            },
            None => LocalSharingSnapshot::default(),
        };
        self.account.sync_sharing(snapshot);
    }

    pub(crate) fn share(&mut self, cx: &mut Context<Self>) {
        let view_only = self.view_only;
        let control_mode = self.host_control_mode;
        let privacy_screen = self.privacy_screen;
        let t = i18n::tr();
        let result: Result<Sharing> = (|| {
            let address: Ipv4Addr = self
                .local_ip
                .read(cx)
                .selected_value()
                .ok_or_else(|| anyhow!("{}", t.err_select_local_ip))?
                .parse()?;
            let port = self
                .port
                .read(cx)
                .value()
                .trim()
                .parse::<u16>()
                .map_err(|_| anyhow!("{}", t.err_invalid_port))?;
            ensure!(port > 0, "{}", t.err_invalid_port);
            platform::request_host_permissions(view_only)?;
            let source = self
                .display
                .read(cx)
                .selected_value()
                .copied()
                .context(t.err_select_display)?;
            let (target, virtual_display, display_summary) = match source {
                DisplaySource::Primary | DisplaySource::Existing(_) => {
                    #[cfg(target_os = "linux")]
                    if platform::wayland_portal::is_wayland_session() {
                        let (_, w, h, fps) = platform::linux::linux_primary_display_spec()
                            .unwrap_or_else(|| ("Main".to_owned(), 1920, 1080, 60));
                        let format = codec::virtual_format(w, h, fps)?;
                        let label = i18n::fmt_main_display_size(
                            u32::from(format.width),
                            u32::from(format.height),
                        );
                        (
                            HostDisplayTarget::Fixed {
                                display_id: 0,
                                format,
                            },
                            None,
                            label,
                        )
                    } else {
                        let monitor = display::open_existing(source)?;
                        let format = codec::desktop_format(&monitor, false)?;
                        let display_id = monitor.id()?;
                        let label = match source {
                            DisplaySource::Primary => i18n::fmt_main_display_size(
                                u32::from(format.width),
                                u32::from(format.height),
                            ),
                            DisplaySource::Existing(_) => format!(
                                "{} · {} × {}",
                                monitor.name().unwrap_or_else(|_| t.display.into()),
                                format.width,
                                format.height
                            ),
                            DisplaySource::Virtual => unreachable!(),
                        };
                        (HostDisplayTarget::Fixed { display_id, format }, None, label)
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        let monitor = display::open_existing(source)?;
                        let format = codec::desktop_format(&monitor, false)?;
                        let display_id = monitor.id()?;
                        let label = match source {
                            DisplaySource::Primary => i18n::fmt_main_display_size(
                                u32::from(format.width),
                                u32::from(format.height),
                            ),
                            DisplaySource::Existing(_) => format!(
                                "{} · {} × {}",
                                monitor.name().unwrap_or_else(|_| t.display.into()),
                                format.width,
                                format.height
                            ),
                            DisplaySource::Virtual => unreachable!(),
                        };
                        (HostDisplayTarget::Fixed { display_id, format }, None, label)
                    }
                }
                DisplaySource::Virtual => {
                    display::virtual_display_supported()?;
                    let resolution = self
                        .virtual_resolution
                        .read(cx)
                        .selected_value()
                        .copied()
                        .unwrap_or(VirtualResolution::Adaptive);
                    let display_summary = match resolution {
                        VirtualResolution::Adaptive => t.virtual_display_match_viewer.to_string(),
                        VirtualResolution::Fixed(spec) => {
                            let format =
                                codec::virtual_format(spec.width, spec.height, spec.fps as u16)?;
                            i18n::fmt_virtual_display_size(
                                u32::from(format.width),
                                u32::from(format.height),
                            )
                        }
                    };
                    (
                        HostDisplayTarget::Virtual(resolution),
                        None,
                        display_summary,
                    )
                }
            };
            let (sender, events) = channel();
            let host_wake = self.wake.clone();
            let host = sharing::Host::start(
                SocketAddrV4::new(address, port),
                view_only,
                control_mode,
                privacy_screen,
                target,
                move |event| {
                    let _ = sender.send(event);
                    host_wake.notify_one();
                },
            )?;
            let beacon = match random_instance_id() {
                Ok(instance_id) => {
                    let iface = local_interface_addresses()
                        .into_iter()
                        .find(|iface| iface.ip == address)
                        .unwrap_or(InterfaceAddress {
                            ip: address,
                            broadcast: None,
                        });
                    DiscoveryBeacon::start(HostAdvertisement {
                        instance_id,
                        port: host.address.port(),
                        host_name: local_hostname(),
                        platform: host_platform().to_string(),
                        display_label: display_summary.clone(),
                        view_only,
                        interfaces: vec![iface],
                    })
                    .ok()
                }
                Err(_) => None,
            };
            Ok(Sharing {
                host,
                events,
                peers: Vec::new(),
                controller: None,
                display_summary,
                active_format: None,
                _display: virtual_display,
                _beacon: beacon,
            })
        })();
        match result {
            Ok(sharing) => {
                self.sharing = Some(sharing);
                self.status = None;
                self.sync_account_sharing();
            }
            Err(err) => self.status = Some((format!("{err:#}"), true)),
        }
        cx.notify();
    }

    pub(crate) fn refresh_addresses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.local_ip.update(cx, |state, cx| {
            let prior = state.selected_value().cloned();
            let addresses = local_addresses();
            let row = prior
                .and_then(|prior| addresses.iter().position(|ip| *ip == prior))
                .unwrap_or(0);
            state.set_items(addresses, window, cx);
            state.set_selected_index(Some(IndexPath::default().row(row)), window, cx);
        });
    }

    pub(crate) fn refresh_displays(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.display.update(cx, |state, cx| {
            let prior = state.selected_value().copied();
            let displays = display::display_choices();
            let row = prior
                .and_then(|prior| displays.iter().position(|choice| choice.source == prior))
                .unwrap_or(0);
            state.set_items(displays, window, cx);
            state.set_selected_index(Some(IndexPath::default().row(row)), window, cx);
        });
    }

    pub(crate) fn refresh_resolution_choices(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.virtual_resolution.update(cx, |state, cx| {
            let prior = state.selected_value().copied();
            let items = display::virtual_resolution_choices();
            let row = prior
                .and_then(|prior| items.iter().position(|choice| choice.value == prior))
                .unwrap_or(0);
            state.set_items(items, window, cx);
            state.set_selected_index(Some(IndexPath::default().row(row)), window, cx);
        });
        self.viewer_resolution.update(cx, |state, cx| {
            let prior = state.selected_value().copied();
            let items = display::viewer_resolution_choices();
            let row = prior
                .and_then(|prior| items.iter().position(|choice| choice.value == prior))
                .unwrap_or(0);
            state.set_items(items, window, cx);
            state.set_selected_index(Some(IndexPath::default().row(row)), window, cx);
        });
    }

    pub(crate) fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut viewer_clipboard = None;
        let mut clear_code_input = false;
        let mut focus_remote = false;
        let mut mute_dirty = false;
        let mut active_closed = false;

        for tab in &mut self.sessions {
            let is_active = self.active_tab == ActiveTab::Session(tab.id);
            let mut closed = None;
            for event in tab.viewer.connection.events.try_iter() {
                match event {
                    viewer::Event::Ready(can_control) => {
                        tab.viewer.can_control = can_control;
                        if is_active
                            && can_control
                            && tab.viewer.auto_control_armed
                            && window.is_window_active()
                        {
                            tab.viewer.controlling = true;
                            tab.viewer.auto_control_armed = false;
                            focus_remote = true;
                        }
                        if is_active {
                            self.last_clipboard_hash = None;
                        }
                        clear_code_input = true;
                        changed = true;
                    }
                    viewer::Event::Cursor(shape) => {
                        if tab.viewer.remote_cursor != shape {
                            tab.viewer.remote_cursor = shape;
                            if is_active {
                                changed = true;
                            }
                        }
                    }
                    viewer::Event::Clipboard(data) => {
                        if is_active {
                            viewer_clipboard = Some(data);
                            changed = true;
                        }
                    }
                    viewer::Event::Transfer {
                        message,
                        error,
                        active,
                    } => {
                        tab.viewer.transfer_notice = Some((message, error, active, Instant::now()));
                        if is_active {
                            changed = true;
                        }
                    }
                    viewer::Event::Closed(result) => {
                        closed = Some(result);
                        changed = true;
                    }
                }
            }
            if let Some(result) = closed {
                tab.viewer.controlling = false;
                tab.viewer.session_menu_open = false;
                tab.viewer.menu_drag = None;
                tab.closed_notice = Some(match result {
                    Ok(()) => (i18n::tr().session_disconnected.into(), false),
                    Err(err) => (format!("{err:#}"), true),
                });
                mute_dirty = true;
                if is_active {
                    active_closed = true;
                }
            } else if tab.closed_notice.is_none()
                && let Some(frame) = tab.viewer.connection.take_frame()
            {
                let first_frame = tab.viewer.frame.is_none();
                if first_frame {
                    tab.viewer.telemetry = tab.viewer.connection.telemetry();
                    if is_active
                        && tab.viewer.can_control
                        && tab.viewer.auto_control_armed
                        && window.is_window_active()
                    {
                        tab.viewer.controlling = true;
                        tab.viewer.auto_control_armed = false;
                    }
                    if is_active && tab.viewer.controlling {
                        focus_remote = true;
                    }
                }
                tab.viewer.frame = Some(frame);
                if is_active || first_frame {
                    changed = true;
                }
            }
        }

        if active_closed {
            self.pressed_buttons.clear();
            self.modifiers = [false; 4];
            self.last_sent_move = None;
            self.screen_bounds.set(None);
        }
        if mute_dirty {
            self.sync_session_audio_mute();
        }
        if clear_code_input {
            self.code
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if focus_remote {
            window.focus(&self.remote_focus);
        }
        if let Some(data) = viewer_clipboard {
            self.last_clipboard_hash = Some(clipboard::write_clipboard(cx, data));
            changed = true;
        }
        if self
            .active_session()
            .is_some_and(|tab| tab.closed_notice.is_none() && tab.viewer.can_control)
            && self.last_clipboard_hash.is_none()
        {
            self.sync_clipboard(cx);
        }

        let mut host_clipboard = None;
        let mut seed_host_clipboard = false;
        if let Some(sharing) = self.sharing.as_mut() {
            let mut stopped = false;
            for event in sharing.events.try_iter() {
                changed = true;
                match event {
                    HostEvent::PeersChanged {
                        peers,
                        controller,
                        active_format,
                    } => {
                        let first_peer = sharing.peers.is_empty() && !peers.is_empty();
                        sharing.peers = peers;
                        sharing.controller = controller;
                        sharing.active_format = active_format;
                        if first_peer {
                            seed_host_clipboard = true;
                        }
                        if sharing.peers.is_empty() {
                            self.last_clipboard_hash = None;
                        } else {
                            self.status = None;
                        }
                    }
                    HostEvent::Clipboard(data) => host_clipboard = Some(data),
                    HostEvent::FileReceived(file) => {
                        self.status = Some((i18n::fmt_saved_file(&file.relative_path), false));
                    }
                    HostEvent::Finished(result) => {
                        if let Err(err) = result {
                            self.status =
                                Some((i18n::fmt_session_ended(&format!("{err:#}")), true));
                        }
                    }
                    HostEvent::Stopped => stopped = true,
                }
            }
            if stopped {
                self.sharing = None;
                self.reveal = false;
                self.last_clipboard_hash = None;
                self.sync_account_sharing();
            }
        }
        if let Some(data) = host_clipboard {
            self.last_clipboard_hash = Some(clipboard::write_clipboard(cx, data));
            changed = true;
        } else if seed_host_clipboard {
            self.last_clipboard_hash = clipboard::read_clipboard(cx).map(|(hash, _)| hash);
        }
        if self.active_tab == ActiveTab::Home && self.idle() && self.sync_discovered() {
            changed = true;
        }
        let (account_changed, account_notice) = self.account.poll(window, cx);
        if account_changed {
            changed = true;
        }
        if let Some(notice) = account_notice {
            self.status = Some(notice);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }
}
