use gpui::{Context, Keystroke, Modifiers, Pixels, ScrollDelta, Window};
use remoters_core::{Button, Input, Point};

use crate::i18n;

use super::state::Desktop;

impl Desktop {
    pub(crate) fn send(&mut self, input: Input, window: &mut Window, cx: &mut Context<Self>) {
        match input {
            Input::Move(at) => {
                if self.last_sent_move == Some(at) {
                    return;
                }
                self.last_sent_move = Some(at);
            }
            _ => {
                self.last_sent_move = None;
            }
        }
        let delivery_failed = self
            .active_session()
            .is_some_and(|tab| tab.closed_notice.is_none() && !tab.viewer.connection.input(input));
        if delivery_failed {
            if let Some(tab) = self.active_session_mut() {
                tab.viewer.controlling = false;
                tab.viewer.session_menu_open = false;
                tab.viewer.menu_drag = None;
                tab.closed_notice = Some((i18n::tr().session_disconnected.into(), true));
            }
            self.pressed_buttons.clear();
            self.modifiers = [false; 4];
            self.last_sent_move = None;
            self.screen_bounds.set(None);
            self.sync_session_audio_mute();
            let _ = window;
            cx.notify();
        }
    }

    pub(crate) fn release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(viewer) = self.active_viewer_mut() {
            viewer.auto_control_armed = false;
        }
        self.send(Input::Release, window, cx);
        if let Some(viewer) = self.active_viewer_mut() {
            viewer.controlling = false;
        }
        self.pressed_buttons.clear();
        self.modifiers = [false; 4];
        self.last_sent_move = None;
        cx.notify();
    }

    pub(crate) fn change_modifiers(
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
            if !self.is_controlling() {
                break;
            }
            if self.modifiers[index] != down {
                self.modifiers[index] = down;
                self.send(Input::Key { code, down }, window, cx);
            }
        }
    }

    pub(crate) fn key(
        &mut self,
        key: &Keystroke,
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = self.active_viewer() else {
            return;
        };
        if !viewer.controlling || viewer.menu_drag.is_some() {
            return;
        }
        if down && key.key == "escape" && key.modifiers.control && key.modifiers.alt {
            self.release(window, cx);
            cx.stop_propagation();
            return;
        }
        self.change_modifiers(key.modifiers, window, cx);
        if let Some(code) = wire_key(&key.key).filter(|_| self.is_controlling()) {
            self.send(Input::Key { code, down }, window, cx);
        }
        cx.stop_propagation();
    }

    pub(crate) fn coordinates(&self, pos: gpui::Point<Pixels>, clamp: bool) -> Option<Point> {
        let area = self.screen_bounds.get()?;
        let frame = self.active_viewer()?.frame.as_ref()?;
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

    pub(crate) fn mouse_button(
        &mut self,
        button: Button,
        down: bool,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = self.active_viewer() else {
            return;
        };
        if !viewer.controlling || viewer.menu_drag.is_some() {
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

    pub(crate) fn scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(viewer) = self.active_viewer() else {
            return;
        };
        if !viewer.controlling || viewer.menu_drag.is_some() {
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
