#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    cell::Cell,
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    rc::Rc,
    sync::mpsc::{Receiver, channel},
    time::Duration,
};

use anyhow::{Result, anyhow, ensure};
use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Keystroke, Modifiers, Pixels,
    ScrollDelta, SharedString, Window, WindowBounds, WindowOptions, prelude::*, px, size,
};
use gpui_component::{IndexPath, Root, TitleBar, input::InputState, select::SelectState};
use lan_remote_core::{
    Button, DEFAULT_PORT, Input, Point,
    lan::{self, Host, HostEvent, Pairing},
};

mod platform;
mod ui;
mod viewer;

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum Role {
    #[default]
    Connect,
    Share,
}

struct Sharing {
    host: Host,
    events: Receiver<HostEvent>,
    peer: Option<SocketAddr>,
}

struct Viewer {
    address: SocketAddrV4,
    connection: viewer::Connection,
    can_control: bool,
    frame: Option<platform::VideoFrame>,
}

struct Desktop {
    role: Role,
    address: Entity<InputState>,
    code: Entity<InputState>,
    local_ip: Entity<SelectState<Vec<SharedString>>>,
    port: Entity<InputState>,
    view_only: bool,
    sharing: Option<Sharing>,
    viewer: Option<Viewer>,
    remote_focus: FocusHandle,
    screen_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    controlling: bool,
    pressed_buttons: HashSet<Button>,
    modifiers: [bool; 4],
    status: Option<(String, bool)>,
    reveal: bool,
}

fn main() {
    Application::new()
        .with_assets(gpui_component_assets::Assets)
        .run(|app: &mut App| {
            gpui_component::init(app);
            ui::configure_theme(app);
            let bounds = Bounds::centered(None, size(px(1110.), px(705.)), app);
            app.open_window(
                WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("LAN Remote".into()),
                        traffic_light_position: Some(gpui::point(px(9.), px(14.))),
                        ..TitleBar::title_bar_options()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(740.), px(540.))),
                    ..Default::default()
                },
                |window, app| {
                    let desktop = app.new(|cx| Desktop::new(window, cx));
                    app.new(|cx| Root::new(desktop, window, cx))
                },
            )
            .expect("open the LAN Remote window");
            app.activate(true);
            app.on_window_closed(|app| {
                if app.windows().is_empty() {
                    app.quit();
                }
            })
            .detach();
        });
}

impl Desktop {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("192.168.1.20:47821"));
        let code = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste the host’s code")
                .masked(true)
        });
        let port = cx.new(|cx| InputState::new(window, cx).default_value(DEFAULT_PORT.to_string()));
        let local_ip = cx
            .new(|cx| SelectState::new(local_addresses(), Some(IndexPath::default()), window, cx));

        cx.observe_window_activation(window, |desktop, window, cx| {
            if desktop.controlling && !window.is_window_active() {
                desktop.release(window, cx);
            }
        })
        .detach();
        cx.spawn_in(window, async move |handle, cx| {
            loop {
                let active = handle.update_in(cx, |desktop, window, cx| {
                    desktop.poll(window, cx);
                    desktop.viewer.is_some() || desktop.sharing.is_some()
                });
                let Ok(active) = active else { break };
                cx.background_executor()
                    .timer(Duration::from_millis(if active { 33 } else { 150 }))
                    .await;
            }
        })
        .detach();

        Self {
            role: Role::Connect,
            address,
            code,
            local_ip,
            port,
            view_only: false,
            sharing: None,
            viewer: None,
            remote_focus: cx.focus_handle(),
            screen_bounds: Default::default(),
            controlling: false,
            pressed_buttons: HashSet::new(),
            modifiers: [false; 4],
            status: None,
            reveal: false,
        }
    }

    fn idle(&self) -> bool {
        self.sharing.is_none() && self.viewer.is_none()
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let result: Result<Viewer> = (|| {
            let address = lan::endpoint(&self.address.read(cx).value())?;
            let pairing = Pairing::parse(&self.code.read(cx).value())?;
            Ok(Viewer {
                address,
                connection: viewer::Connection::start(address, pairing)?,
                can_control: false,
                frame: None,
            })
        })();
        match result {
            Ok(viewer) => {
                self.viewer = Some(viewer);
                self.status = None;
            }
            Err(err) => self.status = Some((format!("{err:#}"), true)),
        }
        cx.notify();
    }

    fn share(&mut self, cx: &mut Context<Self>) {
        let view_only = self.view_only;
        let result: Result<Sharing> = (|| {
            let address: Ipv4Addr = self
                .local_ip
                .read(cx)
                .selected_value()
                .ok_or_else(|| anyhow!("Select a local IPv4 address."))?
                .parse()?;
            let port = self
                .port
                .read(cx)
                .value()
                .trim()
                .parse::<u16>()
                .map_err(|_| anyhow!("Enter a port from 1 to 65535."))?;
            ensure!(port > 0, "Enter a port from 1 to 65535.");
            platform::request_host_permissions(view_only)?;
            let (sender, events) = channel();
            let host = Host::start(
                SocketAddrV4::new(address, port),
                view_only,
                move || platform::NativeDesktop::new(view_only),
                move |event| {
                    let _ = sender.send(event);
                },
            )?;
            Ok(Sharing {
                host,
                events,
                peer: None,
            })
        })();
        match result {
            Ok(sharing) => {
                self.sharing = Some(sharing);
                self.status = None;
            }
            Err(err) => self.status = Some((format!("{err:#}"), true)),
        }
        cx.notify();
    }

    fn refresh_addresses(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(viewer) = self.viewer.as_mut() {
            let mut closed = None;
            for event in viewer.connection.events.try_iter() {
                changed = true;
                match event {
                    viewer::Event::Ready(can_control) => {
                        viewer.can_control = can_control;
                        self.code
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    viewer::Event::Closed(result) => closed = Some(result),
                }
            }
            if let Some(result) = closed {
                self.finish_viewer(window);
                self.status = Some(match result {
                    Ok(()) => ("Disconnected.".into(), false),
                    Err(err) => (format!("{err:#}"), true),
                });
            } else if let Some(frame) = viewer.connection.take_frame() {
                if let Some(previous) = viewer.frame.replace(frame) {
                    let _ = window.drop_image(previous.image);
                }
                changed = true;
            }
        }

        if let Some(sharing) = self.sharing.as_mut() {
            let mut stopped = false;
            for event in sharing.events.try_iter() {
                changed = true;
                match event {
                    HostEvent::Connected(peer) => {
                        sharing.peer = Some(peer);
                        self.status = None;
                    }
                    HostEvent::Finished(result) => {
                        sharing.peer = None;
                        self.status = result
                            .err()
                            .map(|err| (format!("Session ended. {err:#}"), true));
                    }
                    HostEvent::Stopped => stopped = true,
                }
            }
            if stopped {
                self.sharing = None;
                self.reveal = false;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn finish_viewer(&mut self, window: &mut Window) {
        if let Some(viewer) = self.viewer.take() {
            if let Some(frame) = viewer.frame {
                let _ = window.drop_image(frame.image);
            }
        }
        self.controlling = false;
        self.pressed_buttons.clear();
        self.modifiers = [false; 4];
        self.screen_bounds.set(None);
    }

    fn send(&mut self, input: Input, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .viewer
            .as_ref()
            .is_some_and(|viewer| !viewer.connection.input(input))
        {
            self.finish_viewer(window);
            self.status = Some((
                "Input could not be delivered. The session was closed.".into(),
                true,
            ));
            cx.notify();
        }
    }

    fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send(Input::Release, window, cx);
        self.controlling = false;
        self.pressed_buttons.clear();
        self.modifiers = [false; 4];
        cx.notify();
    }

    fn change_modifiers(
        &mut self,
        modifiers: Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (index, (down, code)) in [
            (modifiers.shift, 0xA0),
            (modifiers.control, 0xA2),
            (modifiers.alt, 0xA4),
            (modifiers.platform, 0x5B),
        ]
        .into_iter()
        .enumerate()
        {
            if !self.controlling {
                break;
            }
            if self.modifiers[index] != down {
                self.modifiers[index] = down;
                self.send(Input::Key { code, down }, window, cx);
            }
        }
    }

    fn key(&mut self, key: &Keystroke, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.controlling {
            return;
        }
        if down && key.key == "escape" && key.modifiers.control && key.modifiers.alt {
            self.release(window, cx);
            cx.stop_propagation();
            return;
        }
        self.change_modifiers(key.modifiers, window, cx);
        if let Some(code) = wire_key(&key.key).filter(|_| self.controlling) {
            self.send(Input::Key { code, down }, window, cx);
        }
        cx.stop_propagation();
    }

    fn coordinates(&self, pos: gpui::Point<Pixels>, clamp: bool) -> Option<Point> {
        let area = self.screen_bounds.get()?;
        let frame = self.viewer.as_ref()?.frame.as_ref()?;
        let factor = (f32::from(area.size.width) / frame.width as f32)
            .min(f32::from(area.size.height) / frame.height as f32);
        if factor <= 0.0 {
            return None;
        }
        let w = frame.width as f32 * factor;
        let h = frame.height as f32 * factor;
        let x = (f32::from(pos.x - area.origin.x) - (f32::from(area.size.width) - w) / 2.0) / w;
        let y = (f32::from(pos.y - area.origin.y) - (f32::from(area.size.height) - h) / 2.0) / h;
        if !clamp && (!(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y)) {
            return None;
        }
        Some(Point {
            x: (x.clamp(0.0, 1.0) * 65535.0).round() as u16,
            y: (y.clamp(0.0, 1.0) * 65535.0).round() as u16,
        })
    }

    fn mouse_button(
        &mut self,
        button: Button,
        down: bool,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.controlling {
            return;
        }
        let Some(at) = self.coordinates(at, !down) else {
            return;
        };
        if down {
            window.focus(&self.remote_focus);
        }
        let changed = if down {
            self.pressed_buttons.insert(button)
        } else {
            self.pressed_buttons.remove(&button)
        };
        if changed {
            self.send(Input::Button { at, button, down }, window, cx);
        }
    }

    fn scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.controlling {
            return;
        }
        let Some(at) = self.coordinates(event.position, false) else {
            return;
        };
        let units = match event.delta {
            ScrollDelta::Pixels(delta) => delta.map(|v| f32::from(v) * 4.0),
            ScrollDelta::Lines(delta) => delta.map(|v| v * 120.0),
        };
        self.send(
            Input::Scroll {
                at,
                vertical: scroll_units(units.y),
                horizontal: scroll_units(-units.x),
            },
            window,
            cx,
        );
    }
}

fn local_addresses() -> Vec<SharedString> {
    let mut addresses: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            std::net::IpAddr::V4(ip) if lan::is_local(ip) && !ip.is_loopback() => Some(ip),
            _ => None,
        })
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    addresses.push(Ipv4Addr::LOCALHOST);
    addresses
        .into_iter()
        .map(|ip| ip.to_string().into())
        .collect()
}

fn scroll_units(amount: f32) -> i16 {
    amount.clamp(-1200.0, 1200.0).round() as i16
}

fn wire_key(key: &str) -> Option<u16> {
    if let [key @ (b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9')] = key.as_bytes() {
        return Some(u16::from(key.to_ascii_uppercase()));
    }
    if let Some(number) = key
        .strip_prefix('f')
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|n| (1..=12).contains(n))
    {
        return Some(0x6F + number);
    }
    Some(match key {
        "backspace" => 0x08,
        "tab" => 0x09,
        "enter" => 0x0D,
        "escape" => 0x1B,
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" => 0x2D,
        "delete" => 0x2E,
        ";" => 0xBA,
        "=" => 0xBB,
        "," => 0xBC,
        "-" => 0xBD,
        "." => 0xBE,
        "/" => 0xBF,
        "`" => 0xC0,
        "[" => 0xDB,
        "\\" => 0xDC,
        "]" => 0xDD,
        "'" => 0xDE,
        _ => return None,
    })
}
