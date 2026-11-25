#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddrV4},
    sync::mpsc::{Receiver, channel},
};

use anyhow::{Result, anyhow};
use eframe::egui::{
    self, Color32, Context, Event, Key, Modifiers, PointerButton, Pos2, Rect, RichText,
    TextureHandle, TextureOptions, Ui, Vec2,
};
use lan_remote_core::{
    Button, DEFAULT_PORT, Input, Point,
    lan::{self, Host, HostEvent, Pairing},
};

mod platform;
mod viewer;

#[derive(Default, PartialEq)]
enum Role {
    #[default]
    Connect,
    Share,
}

struct Sharing {
    host: Host,
    events: Receiver<HostEvent>,
}

struct App {
    role: Role,
    address: String,
    code: String,
    local_addresses: Vec<Ipv4Addr>,
    selected_ip: Option<Ipv4Addr>,
    port: String,
    view_only: bool,
    sharing: Option<Sharing>,
    viewer: Option<viewer::Connection>,
    texture: Option<TextureHandle>,
    remote_control_allowed: bool,
    controlling: bool,
    pressed_buttons: HashSet<Button>,
    modifiers: [bool; 4],
    status: String,
    error: bool,
    reveal: bool,
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1010.0, 700.0])
            .with_min_inner_size([690.0, 440.0]),
        ..Default::default()
    };
    eframe::run_native(
        "LAN Remote",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            cc.egui_ctx.style_mut(|style| {
                style.spacing.item_spacing = egui::vec2(10.0, 10.0);
                style.spacing.button_padding = egui::vec2(12.0, 6.0);
            });
            Ok(Box::new(App::new()))
        }),
    )
}

impl App {
    fn new() -> Self {
        let local_addresses = local_addresses();
        Self {
            role: Role::Connect,
            address: String::new(),
            code: String::new(),
            selected_ip: local_addresses.first().copied(),
            local_addresses,
            port: DEFAULT_PORT.to_string(),
            view_only: false,
            sharing: None,
            viewer: None,
            texture: None,
            remote_control_allowed: false,
            controlling: false,
            pressed_buttons: HashSet::new(),
            modifiers: [false; 4],
            status: String::new(),
            error: false,
            reveal: false,
        }
    }

    fn status(&mut self, message: impl Into<String>, error: bool) {
        self.status = message.into();
        self.error = error;
    }

    fn start_viewer(&mut self, ctx: &Context) -> Result<()> {
        let address = lan::endpoint(&self.address)?;
        let code = Pairing::parse(&self.code)?;
        self.viewer = Some(viewer::Connection::start(ctx, address, code)?);
        self.status(format!("Connecting to {address}…"), false);
        Ok(())
    }

    fn start_host(&mut self, ctx: &Context) -> Result<()> {
        let address = self
            .selected_ip
            .ok_or_else(|| anyhow!("Select a local IPv4 address."))?;
        let port = self.port.trim().parse::<u16>()?;
        platform::request_host_permissions(self.view_only)?;
        let (sender, events) = channel();
        let ctx = ctx.clone();
        let view_only = self.view_only;
        let host = Host::start(
            SocketAddrV4::new(address, port),
            view_only,
            move || platform::NativeDesktop::new(view_only),
            move |event| {
                let _ = sender.send(event);
                ctx.request_repaint();
            },
        )?;
        self.sharing = Some(Sharing { host, events });
        self.status("Waiting for a paired device.", false);
        Ok(())
    }

    fn poll(&mut self, ctx: &Context) {
        if let Some(viewer) = &self.viewer {
            let events: Vec<_> = viewer.events.try_iter().collect();
            let image = viewer.take_frame();
            for event in events {
                match event {
                    viewer::Event::Ready(allow) => {
                        self.remote_control_allowed = allow;
                        self.code.clear();
                        self.status("Paired. Waiting for the remote screen.", false);
                    }
                    viewer::Event::Closed(result) => {
                        self.end_viewer();
                        if let Err(err) = result {
                            self.status(format!("{err:#}"), true);
                        } else {
                            self.status("Disconnected.", false);
                        }
                        break;
                    }
                }
            }
            if let Some(image) = image.filter(|_| self.viewer.is_some()) {
                if let Some(texture) = &mut self.texture {
                    texture.set(image, TextureOptions::LINEAR);
                } else {
                    self.texture =
                        Some(ctx.load_texture("remote-screen", image, TextureOptions::LINEAR));
                    self.status(
                        if self.remote_control_allowed {
                            "Connected."
                        } else {
                            "Connected in view-only mode."
                        },
                        false,
                    );
                }
            }
        }

        if let Some(sharing) = &self.sharing {
            let events: Vec<_> = sharing.events.try_iter().collect();
            for event in events {
                match event {
                    HostEvent::Connected(peer) => {
                        self.status(format!("Sharing with {}.", peer.ip()), false)
                    }
                    HostEvent::Finished(Ok(())) => {
                        self.status("Session ended. Waiting for a paired device.", false)
                    }
                    HostEvent::Finished(Err(err)) => {
                        self.status(format!("Session ended. {err:#}"), true)
                    }
                    HostEvent::Stopped => {
                        self.sharing = None;
                        self.reveal = false;
                    }
                }
            }
        }
    }

    fn end_viewer(&mut self) {
        self.viewer = None;
        self.texture = None;
        self.remote_control_allowed = false;
        self.controlling = false;
        self.pressed_buttons.clear();
        self.modifiers.fill(false);
    }

    fn send(&mut self, input: Input) {
        if self
            .viewer
            .as_ref()
            .is_some_and(|viewer| !viewer.input(input))
        {
            self.end_viewer();
            self.status(
                "Input could not be delivered. The session was closed.",
                true,
            );
        }
    }

    fn release(&mut self) {
        self.send(Input::Release);
        self.controlling = false;
        self.pressed_buttons.clear();
        self.modifiers.fill(false);
    }

    fn connect_panel(&mut self, ui: &mut Ui, ctx: &Context) {
        ui.horizontal(|ui| {
            ui.add_enabled(
                self.viewer.is_none(),
                egui::TextEdit::singleline(&mut self.address)
                    .hint_text("Host IPv4 address[:port]")
                    .desired_width(202.0),
            )
            .on_hover_text("Host IPv4 address");
            ui.add_enabled(
                self.viewer.is_none(),
                egui::TextEdit::singleline(&mut self.code)
                    .hint_text("Pairing code")
                    .password(true)
                    .desired_width(205.0),
            )
            .on_hover_text("Code shown on the host");
            if self.viewer.is_none() {
                if ui.button("Connect").clicked() {
                    if let Err(err) = self.start_viewer(ctx) {
                        self.status(format!("{err:#}"), true);
                    }
                }
            } else if ui.button("Disconnect").clicked() {
                self.end_viewer();
                self.status("Disconnected.", false);
            }
            let label = if self.controlling {
                "Release input"
            } else {
                "Take control"
            };
            if ui
                .add_enabled(
                    self.texture.is_some() && self.remote_control_allowed,
                    egui::Button::new(label),
                )
                .clicked()
            {
                if self.controlling {
                    self.release();
                } else {
                    self.controlling = true;
                }
            }
        });
        if self.controlling {
            ui.label("Ctrl + Alt / Option + Esc releases keyboard and mouse.");
        }
        let (surface, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        ui.painter()
            .rect_filled(surface, 10.0, Color32::from_rgb(16, 20, 27));
        if let Some(texture) = &self.texture {
            let size = texture.size_vec2();
            let scale = (surface.width() / size.x).min(surface.height() / size.y);
            let image = Rect::from_center_size(surface.center(), size * scale);
            ui.painter().image(
                texture.id(),
                image,
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            if self.controlling {
                self.forward_input(ctx, image);
            }
        } else {
            ui.painter().text(
                surface.center(),
                egui::Align2::CENTER_CENTER,
                "Remote screen",
                egui::FontId::proportional(19.0),
                Color32::from_rgb(133, 148, 169),
            );
        }
    }

    fn share_panel(&mut self, ui: &mut Ui, ctx: &Context) {
        ui.heading("Share this screen");
        ui.label("Choose the address on the same network as the viewing device.");
        ui.add_enabled_ui(self.sharing.is_none(), |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("listen-address")
                    .selected_text(
                        self.selected_ip
                            .map_or("No local address".into(), |ip| ip.to_string()),
                    )
                    .show_ui(ui, |ui| {
                        for &ip in &self.local_addresses {
                            ui.selectable_value(&mut self.selected_ip, Some(ip), ip.to_string());
                        }
                    });
                if ui.button("Refresh").clicked() {
                    self.local_addresses = local_addresses();
                    if self
                        .selected_ip
                        .is_none_or(|ip| !self.local_addresses.contains(&ip))
                    {
                        self.selected_ip = self.local_addresses.first().copied();
                    }
                }
                ui.label("Port");
                ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(70.0));
            });
            ui.checkbox(&mut self.view_only, "View only");
        });
        if let Some(sharing) = &self.sharing {
            ui.horizontal(|ui| {
                ui.monospace(sharing.host.address.to_string());
                if ui.button("Copy address").clicked() {
                    ctx.copy_text(sharing.host.address.to_string());
                }
                if ui.button("Copy pairing code").clicked() {
                    ctx.copy_text(sharing.host.code.clone());
                }
                if ui
                    .button(if self.reveal {
                        "Hide code"
                    } else {
                        "Show code"
                    })
                    .clicked()
                {
                    self.reveal = !self.reveal;
                }
            });
            if self.reveal {
                ui.monospace(&sharing.host.code);
            }
            if ui.button("Stop sharing").clicked() {
                self.sharing = None;
                self.reveal = false;
                self.status("Sharing stopped.", false);
            }
        } else if ui.button("Start sharing").clicked() {
            if let Err(err) = self.start_host(ctx) {
                self.status(format!("{err:#}"), true);
            }
        }
    }

    fn forward_input(&mut self, ctx: &Context, image: Rect) {
        let (focused, modifiers, events, pointer) = ctx.input(|state| {
            (
                state.focused,
                state.modifiers,
                state.events.clone(),
                state.pointer.hover_pos(),
            )
        });
        if !focused || events.iter().any(|event| matches!(event,
            Event::Key { key: Key::Escape, pressed: true, modifiers, .. } if modifiers.ctrl && modifiers.alt
        )) {
            self.release();
            return;
        }

        for event in events {
            if !self.controlling {
                break;
            }
            match event {
                Event::PointerMoved(pos) => {
                    if image.contains(pos) || !self.pressed_buttons.is_empty() {
                        self.send(Input::Move(normalize(image, pos)));
                    }
                }
                Event::PointerGone if !self.pressed_buttons.is_empty() => self.release(),
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let Some(button) = mouse_button(button) else {
                        continue;
                    };
                    if (pressed && image.contains(pos) && self.pressed_buttons.insert(button))
                        || (!pressed && self.pressed_buttons.remove(&button))
                    {
                        self.sync_modifiers(modifiers);
                        self.send(Input::Button {
                            at: normalize(image, pos),
                            button,
                            down: pressed,
                        });
                    }
                }
                Event::MouseWheel { unit, delta, .. } => {
                    if let Some(pos) = pointer.filter(|pos| image.contains(*pos)) {
                        let factor = match unit {
                            egui::MouseWheelUnit::Point => 4.0,
                            egui::MouseWheelUnit::Line => 120.0,
                            egui::MouseWheelUnit::Page => 960.0,
                        };
                        self.send(Input::Scroll {
                            at: normalize(image, pos),
                            vertical: wheel(delta.y, factor),
                            horizontal: wheel(-delta.x, factor),
                        });
                    }
                }
                Event::Key {
                    key,
                    physical_key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    self.sync_modifiers(modifiers);
                    if let Some(code) = wire_key(physical_key.unwrap_or(key)) {
                        self.send(Input::Key {
                            code,
                            down: pressed,
                        });
                    }
                }
                _ => {}
            }
        }
        if self.controlling {
            self.sync_modifiers(modifiers);
        }
    }

    fn sync_modifiers(&mut self, modifiers: Modifiers) {
        for (i, (down, code)) in [
            (modifiers.shift, 0xA0),
            (modifiers.ctrl, 0xA2),
            (modifiers.alt, 0xA4),
            (modifiers.mac_cmd, 0x5B),
        ]
        .into_iter()
        .enumerate()
        {
            if self.modifiers[i] != down {
                self.modifiers[i] = down;
                self.send(Input::Key { code, down });
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        egui::TopBottomPanel::top("navigation").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("LAN Remote").size(21.0).strong());
                ui.add_space(12.0);
                ui.add_enabled_ui(self.viewer.is_none() && self.sharing.is_none(), |ui| {
                    let before = self.role == Role::Connect;
                    ui.selectable_value(&mut self.role, Role::Connect, "Connect");
                    ui.selectable_value(&mut self.role, Role::Share, "Share");
                    if before != (self.role == Role::Connect) {
                        self.status.clear();
                    }
                });
            });
        });
        if !self.status.is_empty() {
            egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
                let message = RichText::new(&self.status).small();
                ui.label(if self.error {
                    message.color(Color32::from_rgb(247, 133, 131))
                } else {
                    message
                });
            });
        }
        egui::CentralPanel::default().show(ctx, |ui| match self.role {
            Role::Connect => self.connect_panel(ui, ctx),
            Role::Share => self.share_panel(ui, ctx),
        });
    }
}

fn local_addresses() -> Vec<Ipv4Addr> {
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
}

fn normalize(rect: Rect, pos: Pos2) -> Point {
    let Vec2 { x, y } = (pos - rect.min) / rect.size();
    Point {
        x: (x.clamp(0.0, 1.0) * 65535.0).round() as u16,
        y: (y.clamp(0.0, 1.0) * 65535.0).round() as u16,
    }
}

fn wheel(value: f32, factor: f32) -> i16 {
    (value * factor).clamp(-1200.0, 1200.0).round() as i16
}

fn mouse_button(button: PointerButton) -> Option<Button> {
    match button {
        PointerButton::Primary => Some(Button::Left),
        PointerButton::Secondary => Some(Button::Right),
        PointerButton::Middle => Some(Button::Middle),
        _ => None,
    }
}

fn wire_key(key: Key) -> Option<u16> {
    let name = key.name().as_bytes();
    match name {
        [code @ (b'A'..=b'Z' | b'0'..=b'9')] => return Some(u16::from(*code)),
        [b'F', n @ b'1'..=b'9'] => return Some(0x6F + u16::from(n - b'0')),
        [b'F', b'1', n @ b'0'..=b'2'] => return Some(0x79 + u16::from(n - b'0')),
        _ => {}
    }
    Some(match key {
        Key::Backspace => 0x08,
        Key::Tab => 0x09,
        Key::Enter => 0x0D,
        Key::Escape => 0x1B,
        Key::Space => 0x20,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::End => 0x23,
        Key::Home => 0x24,
        Key::ArrowLeft => 0x25,
        Key::ArrowUp => 0x26,
        Key::ArrowRight => 0x27,
        Key::ArrowDown => 0x28,
        Key::Insert => 0x2D,
        Key::Delete => 0x2E,
        Key::Semicolon | Key::Colon => 0xBA,
        Key::Equals | Key::Plus => 0xBB,
        Key::Comma => 0xBC,
        Key::Minus => 0xBD,
        Key::Period => 0xBE,
        Key::Slash | Key::Questionmark => 0xBF,
        Key::Backtick => 0xC0,
        Key::OpenBracket | Key::OpenCurlyBracket => 0xDB,
        Key::Backslash | Key::Pipe => 0xDC,
        Key::CloseBracket | Key::CloseCurlyBracket => 0xDD,
        Key::Quote => 0xDE,
        _ => return None,
    })
}
