use std::{
    cell::Cell,
    collections::HashSet,
    net::{SocketAddr, SocketAddrV4},
    rc::Rc,
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

use gpui::{Bounds, Context, Entity, FocusHandle, Pixels, SharedString, Window, prelude::*};
use gpui_component::{IndexPath, input::InputState, select::SelectState};
use remoters_core::{
    Button, DEFAULT_PORT, Point,
    network::discovery::{DiscoveredHost, DiscoveryBeacon, DiscoveryScanner},
    session::{CodecPreference, CursorShape, DisplayPreference, StreamPreference},
};
use tokio::sync::Notify;

use crate::{
    account_ui::AccountState,
    i18n, metrics,
    platform::{
        self,
        display::{
            self, DisplayChoice, ResolutionChoice, ViewerResolution, ViewerResolutionChoice,
        },
    },
    sharing::{self, HostControlMode, HostEvent},
    viewer,
};

use super::{
    cli::StartupOptions,
    discovery::{local_addresses, local_interface_addresses},
};

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) enum Role {
    #[default]
    Connect,
    Share,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ActiveTab {
    #[default]
    Home,
    Session(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTabStatus {
    Connecting,
    Live,
    Closed,
}

pub(crate) struct Sharing {
    pub(crate) host: sharing::Host,
    pub(crate) events: Receiver<HostEvent>,
    pub(crate) peers: Vec<SocketAddr>,
    pub(crate) controller: Option<SocketAddr>,
    pub(crate) display_summary: String,
    pub(crate) active_format: Option<(u16, u16, u16)>,
    pub(crate) _display: Option<display::VirtualDisplay>,
    pub(crate) _beacon: Option<DiscoveryBeacon>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionReconnectParams {
    pub(crate) address: SocketAddrV4,
    pub(crate) pairing_code: String,
    pub(crate) title: String,
    pub(crate) codec_preference: CodecPreference,
    pub(crate) hdr_enabled: bool,
    pub(crate) resolution: ViewerResolution,
    pub(crate) fps_cap: Option<u16>,
}

pub(crate) struct Viewer {
    pub(crate) address: SocketAddrV4,
    pub(crate) connection: viewer::Connection,
    pub(crate) can_control: bool,
    pub(crate) auto_control_armed: bool,
    pub(crate) frame: Option<platform::VideoFrame>,
    pub(crate) remote_cursor: CursorShape,
    pub(crate) audio_muted: bool,
    pub(crate) quality_expanded: bool,
    pub(crate) resolution: ViewerResolution,
    pub(crate) fps_cap: Option<u16>,
    pub(crate) active_bitrate_mbps: Option<u16>,
    pub(crate) frame_pacing: viewer::FramePacingMode,
    pub(crate) telemetry: metrics::SessionTelemetry,
    pub(crate) transfer_notice: Option<(String, bool, bool, Instant)>,
    pub(crate) controlling: bool,
    pub(crate) session_menu_open: bool,
    pub(crate) stats_section_open: bool,
    pub(crate) stats_hud_open: bool,
    pub(crate) menu_pos: Option<(f32, f32)>,
    pub(crate) menu_drag: Option<MenuDrag>,
}

pub(crate) struct SessionTab {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) target_key: String,
    pub(crate) reconnect: SessionReconnectParams,
    pub(crate) viewer: Viewer,
    pub(crate) closed_notice: Option<(String, bool)>,
}

impl SessionTab {
    pub(crate) fn status(&self) -> SessionTabStatus {
        if self.closed_notice.is_some() {
            SessionTabStatus::Closed
        } else if self.viewer.frame.is_some() {
            SessionTabStatus::Live
        } else {
            SessionTabStatus::Connecting
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct MenuDrag {
    pub(crate) pointer_start: (f32, f32),
    pub(crate) origin_start: (f32, f32),
    pub(crate) moved: bool,
}

pub(crate) struct Desktop {
    pub(crate) role: Role,
    pub(crate) address: Entity<InputState>,
    pub(crate) code: Entity<InputState>,
    pub(crate) local_ip: Entity<SelectState<Vec<SharedString>>>,
    pub(crate) display: Entity<SelectState<Vec<DisplayChoice>>>,
    pub(crate) virtual_resolution: Entity<SelectState<Vec<ResolutionChoice>>>,
    pub(crate) viewer_resolution: Entity<SelectState<Vec<ViewerResolutionChoice>>>,
    pub(crate) port: Entity<InputState>,
    pub(crate) view_only: bool,
    pub(crate) host_control_mode: HostControlMode,
    pub(crate) privacy_screen: bool,
    pub(crate) codec_preference: CodecPreference,
    pub hdr_enabled: bool,
    pub(crate) fps_cap: Option<u16>,
    pub(crate) bitrate_mbps: Option<u16>,
    pub(crate) frame_pacing: viewer::FramePacingMode,
    pub(crate) scanner: Option<DiscoveryScanner>,
    pub(crate) discovered: Vec<DiscoveredHost>,
    pub(crate) sharing: Option<Sharing>,
    pub(crate) sessions: Vec<SessionTab>,
    pub(crate) active_tab: ActiveTab,
    pub(crate) next_session_id: u64,
    pub(crate) remote_focus: FocusHandle,
    pub(crate) screen_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(crate) pressed_buttons: HashSet<Button>,
    pub(crate) modifiers: [bool; 4],
    pub(crate) last_sent_move: Option<Point>,
    pub(crate) last_clipboard_hash: Option<u64>,
    pub(crate) status: Option<(String, bool)>,
    pub(crate) reveal: bool,
    pub(crate) locale_menu_open: bool,
    pub(crate) account: AccountState,
    pub(crate) wake: Arc<Notify>,
}

impl Desktop {
    pub(crate) fn new(
        window: &mut Window,
        startup: &StartupOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.20:47821"));
        let code = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(i18n::tr().pairing_code_placeholder)
                .masked(true)
        });
        let initial_port = startup.bind_port.unwrap_or(DEFAULT_PORT).to_string();
        let port = cx.new(|cx| InputState::new(window, cx).default_value(initial_port));
        let mut addresses = local_addresses();
        let ip_row = if let Some(bind_ip) = startup.bind_ip {
            let bind_text: SharedString = bind_ip.to_string().into();
            if let Some(pos) = addresses.iter().position(|ip| *ip == bind_text) {
                pos
            } else {
                addresses.insert(0, bind_text);
                0
            }
        } else {
            0
        };
        let local_ip = cx.new(|cx| {
            SelectState::new(
                addresses,
                Some(IndexPath::default().row(ip_row)),
                window,
                cx,
            )
        });

        let choices = display::display_choices();
        let display_row = startup
            .display
            .and_then(|target| choices.iter().position(|choice| choice.source == target))
            .unwrap_or(0);
        let display = cx.new(|cx| {
            SelectState::new(
                choices,
                Some(IndexPath::default().row(display_row)),
                window,
                cx,
            )
        });
        let virtual_resolution = cx.new(|cx| {
            SelectState::new(
                display::virtual_resolution_choices(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let viewer_resolution = cx.new(|cx| {
            SelectState::new(
                display::viewer_resolution_choices(),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let scanner = DiscoveryScanner::start(local_interface_addresses()).ok();
        let discovered = scanner
            .as_ref()
            .map(DiscoveryScanner::hosts)
            .unwrap_or_default();
        cx.observe_window_activation(window, |desktop, window, cx| {
            if desktop.is_controlling() && !window.is_window_active() {
                desktop.release(window, cx);
            }
        })
        .detach();
        let wake = Arc::new(Notify::new());
        let account = AccountState::new(window, startup.account_server.clone(), wake.clone(), cx);
        let ui_wake = wake.clone();
        cx.spawn_in(window, async move |handle, cx| {
            loop {
                ui_wake.notified().await;
                let Ok(()) = handle.update_in(cx, |desktop, window, cx| {
                    desktop.poll(window, cx);
                }) else {
                    break;
                };
            }
        })
        .detach();
        let tick_wake = wake.clone();
        cx.spawn_in(window, async move |handle, cx| {
            let mut ticks = 0u32;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(350))
                    .await;
                ticks = ticks.wrapping_add(1);
                let Ok(changed) = handle.update(cx, |desktop, cx| {
                    let mut changed = false;
                    desktop.sync_clipboard(cx);
                    for tab in &mut desktop.sessions {
                        if let Some((_, _, active, updated_at)) = &tab.viewer.transfer_notice
                            && !*active
                            && updated_at.elapsed() >= Duration::from_secs(4)
                        {
                            tab.viewer.transfer_notice = None;
                            if desktop.active_tab == ActiveTab::Session(tab.id) {
                                changed = true;
                            }
                        }
                    }
                    if ticks.is_multiple_of(3)
                        && let Some(tab) = desktop.active_session_mut()
                        && (tab.viewer.session_menu_open || tab.viewer.stats_hud_open)
                    {
                        tab.viewer.telemetry = tab.viewer.connection.telemetry();
                        changed = true;
                    }
                    if ticks.is_multiple_of(3)
                        && desktop.active_tab == ActiveTab::Home
                        && desktop.idle()
                        && desktop.sync_discovered()
                    {
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                    changed
                }) else {
                    break;
                };
                if changed {
                    tick_wake.notify_one();
                }
            }
        })
        .detach();

        let mut desktop = Self {
            role: if startup.share {
                Role::Share
            } else {
                Role::Connect
            },
            address,
            code,
            local_ip,
            display,
            virtual_resolution,
            viewer_resolution,
            port,
            view_only: false,
            host_control_mode: startup.control_mode,
            privacy_screen: startup.privacy_screen,
            codec_preference: CodecPreference::Auto,
            hdr_enabled: false,
            fps_cap: None,
            bitrate_mbps: None,
            frame_pacing: viewer::FramePacingMode::default(),
            scanner,
            discovered,
            sharing: None,
            sessions: Vec::new(),
            active_tab: ActiveTab::Home,
            next_session_id: 1,
            remote_focus: cx.focus_handle(),
            screen_bounds: Default::default(),
            pressed_buttons: HashSet::new(),
            modifiers: [false; 4],
            last_sent_move: None,
            last_clipboard_hash: None,
            status: None,
            reveal: false,
            locale_menu_open: false,
            account,
            wake,
        };
        if startup.share {
            desktop.share(cx);
            desktop.sync_account_sharing();
            if let Some(path) = &startup.invite_file {
                let contents = if let Some(sharing) = &desktop.sharing {
                    format!(
                        "address={}\ncode={}\ndisplay={}\n",
                        sharing.host.address, sharing.host.code, sharing.display_summary
                    )
                } else if let Some((message, true)) = &desktop.status {
                    format!("error={message}\n")
                } else {
                    format!("error={}\n", i18n::tr().sharing_failed_to_start)
                };
                let _ = std::fs::write(path, contents);
            }
        }
        desktop
    }

    pub(crate) fn idle(&self) -> bool {
        self.sharing.is_none()
    }

    pub(crate) fn active_session(&self) -> Option<&SessionTab> {
        match self.active_tab {
            ActiveTab::Home => None,
            ActiveTab::Session(id) => self.sessions.iter().find(|tab| tab.id == id),
        }
    }

    pub(crate) fn active_session_mut(&mut self) -> Option<&mut SessionTab> {
        match self.active_tab {
            ActiveTab::Home => None,
            ActiveTab::Session(id) => self.sessions.iter_mut().find(|tab| tab.id == id),
        }
    }

    pub(crate) fn active_viewer(&self) -> Option<&Viewer> {
        self.active_session().map(|tab| &tab.viewer)
    }

    pub(crate) fn active_viewer_mut(&mut self) -> Option<&mut Viewer> {
        self.active_session_mut().map(|tab| &mut tab.viewer)
    }

    pub(crate) fn is_controlling(&self) -> bool {
        self.active_viewer().is_some_and(|v| v.controlling)
    }

    pub(crate) fn set_active_bitrate_mbps(
        &mut self,
        mbps: Option<u16>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(viewer) = self.active_viewer_mut() {
            viewer.active_bitrate_mbps = mbps;
            viewer.connection.set_bitrate_mbps(mbps);
            if viewer.controlling {
                window.focus(&self.remote_focus);
            }
        }
        cx.notify();
    }

    pub(crate) fn set_locale(
        &mut self,
        locale: i18n::Locale,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.locale_menu_open = false;
        if i18n::locale() == locale {
            cx.notify();
            return;
        }
        i18n::set_locale(locale);
        let s = i18n::tr();
        self.code.update(cx, |input, cx| {
            input.set_placeholder(s.pairing_code_placeholder, window, cx);
        });
        self.account.username.update(cx, |input, cx| {
            input.set_placeholder(s.username_placeholder, window, cx);
        });
        self.account.password.update(cx, |input, cx| {
            input.set_placeholder(s.password, window, cx);
        });
        self.account.display_name.update(cx, |input, cx| {
            input.set_placeholder(s.display_name_placeholder, window, cx);
        });
        self.refresh_displays(window, cx);
        self.refresh_resolution_choices(window, cx);
        cx.notify();
    }

    pub(crate) fn set_privacy_screen(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.privacy_screen = enabled;
        if let Some(sharing) = &self.sharing {
            sharing.host.set_privacy_screen(enabled);
        }
        cx.notify();
    }

    pub(crate) fn privacy_screen_status_label(&self) -> Option<&'static str> {
        let connected = self
            .sharing
            .as_ref()
            .is_some_and(|sharing| !sharing.peers.is_empty());
        privacy_screen_status_label(self.privacy_screen, connected)
    }

    pub(crate) fn set_frame_pacing(
        &mut self,
        mode: viewer::FramePacingMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.frame_pacing = mode;
        if let Some(viewer) = self.active_viewer_mut() {
            viewer.frame_pacing = mode;
            viewer.connection.set_frame_pacing(mode);
            if viewer.controlling {
                window.focus(&self.remote_focus);
            }
        }
        cx.notify();
    }
}

pub(crate) fn session_target_key(address: SocketAddrV4, pairing_code: &str) -> String {
    format!("{address}|{}", pairing_code.trim())
}

pub(crate) fn find_live_session_by_target<'a, T>(
    tabs: &'a [T],
    target_key: &str,
    inspect: impl Fn(&T) -> (&str, bool),
) -> Option<usize> {
    tabs.iter().position(|tab| {
        let (key, closed) = inspect(tab);
        !closed && key == target_key
    })
}

pub(crate) fn next_active_tab_after_close(
    tab_ids: &[u64],
    closed_index: usize,
    current: ActiveTab,
) -> ActiveTab {
    let Some(&closed_id) = tab_ids.get(closed_index) else {
        return current;
    };
    if current != ActiveTab::Session(closed_id) {
        return current;
    }
    if let Some(&next_id) = tab_ids.get(closed_index + 1) {
        ActiveTab::Session(next_id)
    } else if closed_index > 0 {
        ActiveTab::Session(tab_ids[closed_index - 1])
    } else {
        ActiveTab::Home
    }
}

pub(crate) fn effective_audio_muted(
    tab_id: u64,
    user_muted: bool,
    is_closed: bool,
    active_tab: ActiveTab,
) -> bool {
    is_closed || active_tab != ActiveTab::Session(tab_id) || user_muted
}

pub(crate) fn privacy_screen_status_label(
    enabled: bool,
    viewer_connected: bool,
) -> Option<&'static str> {
    if !enabled {
        return None;
    }
    let s = i18n::tr();
    Some(if viewer_connected {
        s.privacy_screen_active
    } else {
        s.privacy_screen_armed
    })
}

pub(crate) fn update_reconnect_params(
    params: &mut SessionReconnectParams,
    next_resolution: Option<ViewerResolution>,
    next_fps_cap: Option<Option<u16>>,
) -> bool {
    let mut changed = false;
    if let Some(resolution) = next_resolution
        && params.resolution != resolution
    {
        params.resolution = resolution;
        changed = true;
    }
    if let Some(fps_cap) = next_fps_cap
        && params.fps_cap != fps_cap
    {
        params.fps_cap = fps_cap;
        changed = true;
    }
    changed
}

pub(crate) fn build_reconnect_stream_preference(
    params: &SessionReconnectParams,
    bitrate_mbps: Option<u16>,
    resolve_display: impl FnOnce(ViewerResolution) -> Option<DisplayPreference>,
) -> StreamPreference {
    let display = resolve_display(params.resolution).map(|mut pref| {
        if let Some(fps) = params.fps_cap {
            pref.frames_per_second = fps.clamp(30, 120);
        }
        pref
    });
    build_stream_preference(
        display,
        params.codec_preference,
        params.fps_cap,
        bitrate_mbps,
        params.hdr_enabled,
    )
}

pub(crate) fn build_stream_preference(
    display: Option<DisplayPreference>,
    codec: CodecPreference,
    fps_cap: Option<u16>,
    bitrate_mbps: Option<u16>,
    hdr: bool,
) -> StreamPreference {
    StreamPreference {
        display,
        codec,
        max_fps: fps_cap.map(|fps| fps.clamp(30, 120)),
        target_bitrate_mbps: bitrate_mbps.map(|mbps| mbps.clamp(5, 150)),
        supported_codecs: 0,
        hdr,
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use remoters_core::session::{CodecPreference, DisplayPreference};

    use super::{
        ActiveTab, SessionReconnectParams, build_reconnect_stream_preference,
        build_stream_preference, effective_audio_muted, find_live_session_by_target,
        next_active_tab_after_close, privacy_screen_status_label, session_target_key,
        update_reconnect_params,
    };
    use crate::{
        i18n::{Locale, with_locale_for_test},
        platform::display::{ViewerResolution, VirtualSpec},
    };

    #[test]
    fn reconnect_params_update_in_place_and_build_stream_preference() {
        let addr = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 8), 47821);
        let mut params = SessionReconnectParams {
            address: addr,
            pairing_code: "PAIR-CODE".into(),
            title: "Workstation".into(),
            codec_preference: CodecPreference::Hevc,
            hdr_enabled: true,
            resolution: ViewerResolution::MatchLocal,
            fps_cap: None,
        };

        // No-op when setting identical values.
        assert!(!update_reconnect_params(
            &mut params,
            Some(ViewerResolution::MatchLocal),
            None
        ));
        assert!(!update_reconnect_params(&mut params, None, Some(None)));

        // Switching to 1440p + 120 Hz updates params while keeping host identity and codec/HDR.
        let qhd = ViewerResolution::Fixed(VirtualSpec {
            width: 2560,
            height: 1440,
            fps: 60,
        });
        assert!(update_reconnect_params(&mut params, Some(qhd), None));
        assert!(update_reconnect_params(&mut params, None, Some(Some(120))));
        assert_eq!(params.address, addr);
        assert_eq!(params.pairing_code, "PAIR-CODE");
        assert_eq!(params.title, "Workstation");
        assert_eq!(params.codec_preference, CodecPreference::Hevc);
        assert!(params.hdr_enabled);

        let pref_120 =
            build_reconnect_stream_preference(&params, Some(80), |choice| match choice {
                ViewerResolution::Fixed(spec) => {
                    DisplayPreference::normalized(spec.width, spec.height, spec.fps as u16).ok()
                }
                ViewerResolution::MatchLocal => DisplayPreference::normalized(3024, 1900, 120).ok(),
                ViewerResolution::HostDefault => None,
            });
        assert_eq!(
            pref_120.display,
            Some(DisplayPreference {
                width: 2560,
                height: 1440,
                frames_per_second: 120,
            })
        );
        assert_eq!(pref_120.max_fps, Some(120));
        assert_eq!(pref_120.target_bitrate_mbps, Some(80));
        assert_eq!(pref_120.codec, CodecPreference::Hevc);
        assert!(pref_120.hdr);
        assert!(pref_120.validate().is_ok());

        // Switching to Host default clears DisplayPreference while retaining FPS cap.
        assert!(update_reconnect_params(
            &mut params,
            Some(ViewerResolution::HostDefault),
            Some(Some(90))
        ));
        let pref_host_default =
            build_reconnect_stream_preference(&params, None, |choice| match choice {
                ViewerResolution::HostDefault => None,
                _ => DisplayPreference::normalized(1920, 1080, 60).ok(),
            });
        assert_eq!(pref_host_default.display, None);
        assert_eq!(pref_host_default.max_fps, Some(90));
        assert_eq!(pref_host_default.target_bitrate_mbps, None);
        assert!(pref_host_default.validate().is_ok());
    }

    #[test]
    fn privacy_screen_status_label_reflects_enabled_and_connection_state() {
        with_locale_for_test(Locale::En, || {
            assert_eq!(privacy_screen_status_label(false, false), None);
            assert_eq!(privacy_screen_status_label(false, true), None);
            assert_eq!(
                privacy_screen_status_label(true, false),
                Some("Privacy screen ready")
            );
            assert_eq!(
                privacy_screen_status_label(true, true),
                Some("Privacy screen active")
            );
        });
        with_locale_for_test(Locale::ZhCn, || {
            assert_eq!(
                privacy_screen_status_label(true, false),
                Some("隐私屏保待命")
            );
            assert_eq!(
                privacy_screen_status_label(true, true),
                Some("隐私屏保生效中")
            );
        });
    }

    #[derive(Clone)]
    struct MockTab {
        id: u64,
        key: String,
        user_muted: bool,
        closed: bool,
    }

    #[test]
    fn build_stream_preference_defaults_and_explicit_choices_are_valid() {
        let defaults = build_stream_preference(None, CodecPreference::Auto, None, None, false);
        assert_eq!(defaults.display, None);
        assert_eq!(defaults.codec, CodecPreference::Auto);
        assert_eq!(defaults.max_fps, None);
        assert_eq!(defaults.target_bitrate_mbps, None);
        assert!(!defaults.hdr);
        assert!(defaults.validate().is_ok());

        let display = Some(DisplayPreference::new(2560, 1440, 120).expect("valid display"));
        let custom =
            build_stream_preference(display, CodecPreference::Hevc, Some(120), Some(80), true);
        assert_eq!(custom.display, display);
        assert_eq!(custom.codec, CodecPreference::Hevc);
        assert_eq!(custom.max_fps, Some(120));
        assert_eq!(custom.target_bitrate_mbps, Some(80));
        assert!(custom.hdr);
        assert!(custom.validate().is_ok());

        let av1 = build_stream_preference(None, CodecPreference::Av1, Some(120), Some(60), true);
        assert_eq!(av1.codec, CodecPreference::Av1);
        assert!(av1.hdr);
        assert!(av1.validate().is_ok());

        let h264 = build_stream_preference(None, CodecPreference::H264, Some(60), Some(25), false);
        assert_eq!(h264.codec, CodecPreference::H264);
        assert_eq!(h264.max_fps, Some(60));
        assert_eq!(h264.target_bitrate_mbps, Some(25));
        assert!(!h264.hdr);
        assert!(h264.validate().is_ok());
    }

    #[test]
    fn build_stream_preference_clamps_out_of_range_values() {
        let clamped =
            build_stream_preference(None, CodecPreference::Auto, Some(10), Some(500), false);
        assert_eq!(clamped.max_fps, Some(30));
        assert_eq!(clamped.target_bitrate_mbps, Some(150));
        assert!(clamped.validate().is_ok());
    }

    #[test]
    fn multi_tab_lifecycle_handles_add_switch_dedup_and_close_neighbors() {
        let host_a = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 47821);
        let host_b = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 47821);
        let host_c = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 30), 47821);

        let mut tabs = vec![
            MockTab {
                id: 1,
                key: session_target_key(host_a, "CODE-A"),
                user_muted: false,
                closed: false,
            },
            MockTab {
                id: 2,
                key: session_target_key(host_b, "CODE-B"),
                user_muted: true,
                closed: false,
            },
            MockTab {
                id: 3,
                key: session_target_key(host_c, "CODE-C"),
                user_muted: false,
                closed: true,
            },
        ];

        // Duplicate live target focuses existing tab; closed target is not reused.
        let key_a = session_target_key(host_a, " CODE-A ");
        assert_eq!(
            find_live_session_by_target(&tabs, &key_a, |t| (&t.key, t.closed)),
            Some(0)
        );
        let key_c = session_target_key(host_c, "CODE-C");
        assert_eq!(
            find_live_session_by_target(&tabs, &key_c, |t| (&t.key, t.closed)),
            None
        );

        let ids: Vec<u64> = tabs.iter().map(|t| t.id).collect();
        // Closing a background tab keeps the active tab unchanged.
        assert_eq!(
            next_active_tab_after_close(&ids, 2, ActiveTab::Session(1)),
            ActiveTab::Session(1)
        );
        // Closing the active middle tab activates its right neighbor.
        assert_eq!(
            next_active_tab_after_close(&ids, 1, ActiveTab::Session(2)),
            ActiveTab::Session(3)
        );
        // Closing the active rightmost tab activates its left neighbor.
        assert_eq!(
            next_active_tab_after_close(&ids, 2, ActiveTab::Session(3)),
            ActiveTab::Session(2)
        );
        // Closing the last remaining active tab returns to Home.
        tabs.truncate(1);
        assert_eq!(
            next_active_tab_after_close(&[1], 0, ActiveTab::Session(1)),
            ActiveTab::Home
        );
    }

    #[test]
    fn background_audio_mute_policy_mutes_inactive_and_restores_active_preference() {
        let tabs = [
            MockTab {
                id: 1,
                key: "a".into(),
                user_muted: false,
                closed: false,
            },
            MockTab {
                id: 2,
                key: "b".into(),
                user_muted: true,
                closed: false,
            },
            MockTab {
                id: 3,
                key: "c".into(),
                user_muted: false,
                closed: true,
            },
        ];

        // On Home: all sessions muted.
        for tab in &tabs {
            assert!(effective_audio_muted(
                tab.id,
                tab.user_muted,
                tab.closed,
                ActiveTab::Home
            ));
        }

        // Session 1 active: unmuted (user preference), Session 2 & 3 muted.
        assert!(!effective_audio_muted(
            1,
            false,
            false,
            ActiveTab::Session(1)
        ));
        assert!(effective_audio_muted(
            2,
            false,
            false,
            ActiveTab::Session(1)
        ));
        assert!(effective_audio_muted(3, false, true, ActiveTab::Session(1)));

        // Session 2 active with user_muted=true: stays muted; Session 1 now background-muted.
        assert!(effective_audio_muted(
            1,
            false,
            false,
            ActiveTab::Session(2)
        ));
        assert!(effective_audio_muted(2, true, false, ActiveTab::Session(2)));

        // Closed active tab stays muted.
        assert!(effective_audio_muted(3, false, true, ActiveTab::Session(3)));
    }
}
