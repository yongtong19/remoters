use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
#[cfg(target_os = "macos")]
use enigo::Coordinate;
#[cfg(not(target_os = "linux"))]
use enigo::Keyboard;
use enigo::{Axis, Direction, Enigo, Mouse, Settings};
pub mod display;
pub mod linux;
pub mod wayland_portal;

#[cfg(target_os = "windows")]
unsafe extern "C" {
    fn remoters_move_pointer(x: i32, y: i32) -> bool;
}

use remoters_core::{Button, Input, Point};
use xcap::Monitor;

#[cfg(target_os = "macos")]
mod macos_mouse;
#[cfg(target_os = "macos")]
pub use macos_mouse::install_drag_handlers;

pub fn request_host_permissions(view_only: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    macos::request_screen_recording()?;
    #[cfg(target_os = "linux")]
    if wayland_portal::is_wayland_session() {
        return Ok(());
    }
    if !view_only {
        Enigo::new(&Settings::default())
            .map_err(|e| anyhow!("Could not enable keyboard and mouse input: {e}"))?;
    }
    Ok(())
}

pub struct NativeDesktop {
    monitor: Option<Monitor>,
    bounds: (i32, i32, u32, u32),
    bounds_at: Instant,
    input: Option<Enigo>,
    #[cfg(target_os = "linux")]
    wayland: Option<std::sync::Arc<wayland_portal::WaylandPortalSession>>,
    #[cfg(target_os = "macos")]
    middle_mouse: Option<macos_mouse::MiddleMouse>,
    keys: HashSet<u16>,
    buttons: HashSet<Button>,
    scroll_remainder: (i32, i32),
}

impl NativeDesktop {
    pub fn new(view_only: bool, monitor: Monitor) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let bounds = linux::monitor_physical_bounds(&monitor)?;
        #[cfg(not(target_os = "linux"))]
        let bounds = (
            monitor.x()?,
            monitor.y()?,
            monitor.width()?,
            monitor.height()?,
        );
        let input = if view_only {
            None
        } else {
            let settings = Settings {
                open_prompt_to_get_permissions: false,
                ..Settings::default()
            };
            Some(
                Enigo::new(&settings)
                    .map_err(|e| anyhow!("Input permission is unavailable: {e}"))?,
            )
        };
        #[cfg(target_os = "macos")]
        let middle_mouse = input
            .as_ref()
            .map(|input| macos_mouse::MiddleMouse::new(input.get_marker_value()))
            .transpose()?;
        Ok(Self {
            monitor: Some(monitor),
            bounds,
            bounds_at: Instant::now(),
            input,
            #[cfg(target_os = "linux")]
            wayland: None,
            #[cfg(target_os = "macos")]
            middle_mouse,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            scroll_remainder: (0, 0),
        })
    }

    #[cfg(target_os = "linux")]
    pub fn new_wayland(
        view_only: bool,
        portal: std::sync::Arc<wayland_portal::WaylandPortalSession>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        // Mutter and xdg-desktop-portal expect `NotifyPointerMotionAbsolute` coordinates
        // in the logical `stream.size()` space (e.g. 1920x1080 on a 3840x2160 @ 2x display),
        // not the physical PipeWire buffer dimensions passed by `sharing.rs`.
        let (bounds_w, bounds_h) =
            resolve_wayland_pointer_dimensions(portal.dimensions(), width, height);
        Ok(Self {
            monitor: None,
            bounds: (0, 0, bounds_w, bounds_h),
            bounds_at: Instant::now(),
            input: None,
            wayland: (!view_only && portal.input_enabled()).then_some(portal),
            keys: HashSet::new(),
            buttons: HashSet::new(),
            scroll_remainder: (0, 0),
        })
    }

    fn has_input(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self.wayland.is_some() {
            return true;
        }
        self.input.is_some()
    }

    fn monitor_bounds(&mut self) -> Result<(i32, i32, u32, u32)> {
        if let Some(monitor) = &self.monitor
            && self.bounds_at.elapsed() >= Duration::from_secs(2)
        {
            #[cfg(target_os = "linux")]
            {
                self.bounds = linux::monitor_physical_bounds(monitor)?;
            }
            #[cfg(not(target_os = "linux"))]
            {
                self.bounds = (
                    monitor.x()?,
                    monitor.y()?,
                    monitor.width()?,
                    monitor.height()?,
                );
            }
            self.bounds_at = Instant::now();
        }
        Ok(self.bounds)
    }

    fn move_pointer(&mut self, point: Point) -> Result<()> {
        if !self.has_input() {
            return Ok(());
        }
        let (origin_x, origin_y, w, h) = self.monitor_bounds()?;
        #[cfg(target_os = "linux")]
        if let Some(wayland) = &self.wayland {
            let (x, y) = map_wayland_pointer(point, w, h);
            wayland.pointer_motion_absolute(x, y);
            return Ok(());
        }
        if self.input.is_some() {
            let x = origin_x + map_axis(point.x, w);
            let y = origin_y + map_axis(point.y, h);
            #[cfg(target_os = "macos")]
            if let Some(middle) = self.middle_mouse.as_mut() {
                if self.buttons.len() == 1 && self.buttons.contains(&Button::Middle) {
                    return middle.drag((x, y), &self.keys);
                }
                middle.position = (x, y);
            }
            #[cfg(target_os = "macos")]
            if let Some(input) = &mut self.input {
                input.move_mouse(x, y, Coordinate::Abs)?;
            }
            #[cfg(target_os = "windows")]
            anyhow::ensure!(
                unsafe { remoters_move_pointer(x, y) },
                "Windows rejected remote pointer movement."
            );
            #[cfg(target_os = "linux")]
            if let Some(input) = &mut self.input {
                linux::move_pointer(input, x, y)?;
            }
        }
        Ok(())
    }
}

fn map_axis(normalized: u16, pixels: u32) -> i32 {
    ((u64::from(normalized) * u64::from(pixels.saturating_sub(1)) + 32767) / 65535) as i32
}

#[cfg(any(target_os = "linux", test))]
fn resolve_wayland_pointer_dimensions(
    portal_dimensions: Option<(u32, u32)>,
    fallback_width: u32,
    fallback_height: u32,
) -> (u32, u32) {
    match portal_dimensions {
        Some((w, h)) if w >= 2 && h >= 2 => (w, h),
        _ => (fallback_width.max(2), fallback_height.max(2)),
    }
}

#[cfg(any(target_os = "linux", test))]
fn map_wayland_pointer(point: Point, width: u32, height: u32) -> (f64, f64) {
    let x = (f64::from(point.x) / 65535.0) * f64::from(width.saturating_sub(1));
    let y = (f64::from(point.y) / 65535.0) * f64::from(height.saturating_sub(1));
    (x, y)
}

impl NativeDesktop {
    pub fn input(&mut self, event: Input) -> Result<()> {
        match event {
            Input::Move(at) => self.move_pointer(at)?,
            Input::Button { at, button, down } => {
                self.move_pointer(at)?;
                if self.has_input() && down != self.buttons.contains(&button) {
                    self.button(button, down)?;
                    if down {
                        self.buttons.insert(button);
                    } else {
                        self.buttons.remove(&button);
                    }
                }
            }
            Input::Key { code, down } => {
                #[cfg(target_os = "linux")]
                if let Some(wayland) = &self.wayland {
                    if down {
                        if wayland.keyboard_vk(code, true) {
                            self.keys.insert(code);
                        }
                    } else if self.keys.contains(&code) {
                        wayland.keyboard_vk(code, false);
                        self.keys.remove(&code);
                    }
                    return Ok(());
                }
                if let Some(input) = self.input.as_mut() {
                    if down {
                        if native_key(input, code, Direction::Press)? {
                            self.keys.insert(code);
                        }
                    } else if !down && self.keys.contains(&code) {
                        native_key(input, code, Direction::Release)?;
                        self.keys.remove(&code);
                    }
                }
            }
            Input::Scroll {
                at,
                vertical,
                horizontal,
            } => {
                self.move_pointer(at)?;
                #[cfg(target_os = "linux")]
                if let Some(wayland) = &self.wayland {
                    self.scroll_remainder.0 -= i32::from(vertical);
                    self.scroll_remainder.1 += i32::from(horizontal);
                    let (vertical, horizontal) = self.scroll_remainder;
                    self.scroll_remainder = (vertical % 120, horizontal % 120);
                    wayland.pointer_scroll_discrete(vertical / 120, horizontal / 120);
                    return Ok(());
                }
                if let Some(input) = self.input.as_mut() {
                    self.scroll_remainder.0 -= i32::from(vertical);
                    self.scroll_remainder.1 += i32::from(horizontal);
                    let (vertical, horizontal) = self.scroll_remainder;
                    self.scroll_remainder = (vertical % 120, horizontal % 120);
                    if vertical.abs() >= 120 {
                        input.scroll(vertical / 120, Axis::Vertical)?;
                    }
                    if horizontal.abs() >= 120 {
                        input.scroll(horizontal / 120, Axis::Horizontal)?;
                    }
                }
            }
            Input::Release => self.release(),
            Input::Gamepad(_) => {}
        }
        Ok(())
    }

    fn button(&mut self, button: Button, down: bool) -> Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(wayland) = &self.wayland {
            wayland.pointer_button(button, down);
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        if button == Button::Middle
            && let Some(middle) = self.middle_mouse.as_ref()
        {
            return middle.button(down, &self.keys);
        }
        if let Some(input) = self.input.as_mut() {
            let direction = if down {
                Direction::Press
            } else {
                Direction::Release
            };
            input.button(native_button(button), direction)?;
        }
        Ok(())
    }

    fn release(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(wayland) = &self.wayland {
            for code in self.keys.drain() {
                wayland.keyboard_vk(code, false);
            }
            for button in self.buttons.drain() {
                wayland.pointer_button(button, false);
            }
            self.scroll_remainder = (0, 0);
            return;
        }
        #[cfg(target_os = "macos")]
        if self.buttons.contains(&Button::Middle) && self.button(Button::Middle, false).is_ok() {
            self.buttons.remove(&Button::Middle);
        }
        if let Some(input) = self.input.as_mut() {
            self.keys
                .retain(|code| native_key(input, *code, Direction::Release).is_err());
            self.buttons.retain(|button| {
                input
                    .button(native_button(*button), Direction::Release)
                    .is_err()
            });
        }
        self.scroll_remainder = (0, 0);
    }
}

impl Drop for NativeDesktop {
    fn drop(&mut self) {
        self.release();
    }
}

pub fn primary_monitor() -> Result<Monitor> {
    let mut monitors = Monitor::all()?;
    if let Some(pos) = monitors
        .iter()
        .position(|monitor| monitor.is_primary().unwrap_or(false))
    {
        return Ok(monitors.swap_remove(pos));
    }
    monitors
        .into_iter()
        .next()
        .context("No primary display was found.")
}

fn native_button(button: Button) -> enigo::Button {
    match button {
        Button::Left => enigo::Button::Left,
        Button::Right => enigo::Button::Right,
        Button::Middle => enigo::Button::Middle,
    }
}

pub struct VideoFrame {
    #[cfg(target_os = "windows")]
    pub surface: gpui::ID3D11Texture2D,
    #[cfg(target_os = "macos")]
    pub surface: screencapturekit::cv::CVPixelBuffer,
    #[cfg(target_os = "linux")]
    pub surface: std::sync::Arc<gpui::RenderImage>,
    pub width: u32,
    pub height: u32,
}

#[cfg(target_os = "windows")]
fn native_key(input: &mut Enigo, code: u16, direction: Direction) -> Result<bool> {
    input.key(enigo::Key::Other(code.into()), direction)?;
    Ok(true)
}

#[cfg(target_os = "linux")]
fn native_key(input: &mut Enigo, code: u16, direction: Direction) -> Result<bool> {
    linux::inject_key(input, code, direction)
}

#[cfg(target_os = "macos")]
fn native_key(input: &mut Enigo, code: u16, direction: Direction) -> Result<bool> {
    if let Some(key) = macos::scan_code(code) {
        input.raw(key, direction)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Result, ensure};
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    pub fn request_screen_recording() -> Result<()> {
        // CoreGraphics refines these C Boolean return values to a single byte.
        let granted = unsafe { CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() };
        ensure!(
            granted,
            "Allow screen recording in System Settings, then reopen Remoters."
        );
        Ok(())
    }

    pub fn scan_code(code: u16) -> Option<u16> {
        // VK values in the v0.1 wire format to ANSI CGKeyCode positions.
        Some(match code {
            0x41 => 0,
            0x53 => 1,
            0x44 => 2,
            0x46 => 3,
            0x48 => 4,
            0x47 => 5,
            0x5A => 6,
            0x58 => 7,
            0x43 => 8,
            0x56 => 9,
            0x42 => 11,
            0x51 => 12,
            0x57 => 13,
            0x45 => 14,
            0x52 => 15,
            0x59 => 16,
            0x54 => 17,
            0x31 => 18,
            0x32 => 19,
            0x33 => 20,
            0x34 => 21,
            0x36 => 22,
            0x35 => 23,
            0xBB => 24,
            0x39 => 25,
            0x37 => 26,
            0xBD => 27,
            0x38 => 28,
            0x30 => 29,
            0xDD => 30,
            0x4F => 31,
            0x55 => 32,
            0xDB => 33,
            0x49 => 34,
            0x50 => 35,
            0x0D => 36,
            0x4C => 37,
            0x4A => 38,
            0xDE => 39,
            0x4B => 40,
            0xBA => 41,
            0xDC => 42,
            0xBC => 43,
            0xBF => 44,
            0x4E => 45,
            0x4D => 46,
            0xBE => 47,
            0x09 => 48,
            0x20 => 49,
            0xC0 => 50,
            0x08 => 51,
            0x1B => 53,
            0x5B => 55,
            0x5C => 54,
            0x10 | 0xA0 => 56,
            0xA1 => 60,
            0x11 | 0xA2 => 59,
            0xA3 => 62,
            0x12 | 0xA4 => 58,
            0xA5 => 61,
            0x14 => 57,
            0x6E => 65,
            0x6A => 67,
            0x6B => 69,
            0x90 => 71,
            0x6F => 75,
            0x6D => 78,
            0x60 => 82,
            0x61 => 83,
            0x62 => 84,
            0x63 => 85,
            0x64 => 86,
            0x65 => 87,
            0x66 => 88,
            0x67 => 89,
            0x68 => 91,
            0x69 => 92,
            0x74 => 96,
            0x75 => 97,
            0x76 => 98,
            0x72 => 99,
            0x77 => 100,
            0x78 => 101,
            0x7A => 103,
            0x79 => 109,
            0x7B => 111,
            0x24 => 115,
            0x21 => 116,
            0x2E => 117,
            0x73 => 118,
            0x23 => 119,
            0x71 => 120,
            0x22 => 121,
            0x70 => 122,
            0x25 => 123,
            0x27 => 124,
            0x28 => 125,
            0x26 => 126,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use remoters_core::Point;

    use super::{map_axis, map_wayland_pointer, resolve_wayland_pointer_dimensions};

    #[test]
    fn wayland_pointer_dimensions_prefer_portal_logical_stream_size() {
        // 4K physical PipeWire stream at 2.0x HiDPI scaling reports 1920x1080 logical size.
        assert_eq!(
            resolve_wayland_pointer_dimensions(Some((1920, 1080)), 3840, 2160),
            (1920, 1080)
        );
        // Fractional scaling (e.g. 2560x1440 @ 1.5x -> 1707x960) also uses portal logical size.
        assert_eq!(
            resolve_wayland_pointer_dimensions(Some((1707, 960)), 2560, 1440),
            (1707, 960)
        );
        // Falls back to physical dimensions when portal omits or zeroes stream size.
        assert_eq!(
            resolve_wayland_pointer_dimensions(None, 3840, 2160),
            (3840, 2160)
        );
        assert_eq!(
            resolve_wayland_pointer_dimensions(Some((0, 0)), 1920, 1080),
            (1920, 1080)
        );
        assert_eq!(resolve_wayland_pointer_dimensions(None, 1, 1), (2, 2));
    }

    #[test]
    fn maps_normalized_points_into_wayland_logical_bounds() {
        let (w, h) = resolve_wayland_pointer_dimensions(Some((1920, 1080)), 3840, 2160);
        assert_eq!(map_wayland_pointer(Point { x: 0, y: 0 }, w, h), (0.0, 0.0));
        assert_eq!(
            map_wayland_pointer(Point { x: 65535, y: 65535 }, w, h),
            (1919.0, 1079.0)
        );
        let (mid_x, mid_y) = map_wayland_pointer(Point { x: 32768, y: 32768 }, w, h);
        assert!((mid_x - 959.5).abs() < 0.05);
        assert!((mid_y - 539.5).abs() < 0.05);
        // Must never exceed logical bounds on 2x HiDPI displays.
        assert!(mid_x < 1920.0 && mid_y < 1080.0);
        assert_eq!(map_axis(0, 1920), 0);
        assert_eq!(map_axis(65535, 1920), 1919);
    }
}
