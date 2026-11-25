use std::{collections::HashSet, io::Cursor};

use anyhow::{Context, Result, anyhow, ensure};
use enigo::{Axis, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use image::{DynamicImage, ImageFormat, ImageReader, Limits, codecs::jpeg::JpegEncoder};
use lan_remote_core::{Button, Desktop, Input, Point};
use xcap::Monitor;

pub fn request_host_permissions(view_only: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    macos::request_screen_recording()?;
    if !view_only {
        Enigo::new(&Settings::default())
            .map_err(|e| anyhow!("Could not enable keyboard and mouse input: {e}"))?;
    }
    Ok(())
}

pub struct NativeDesktop {
    monitor: Monitor,
    #[cfg(target_os = "macos")]
    capture: macos::Capture,
    input: Option<Enigo>,
    keys: HashSet<u16>,
    buttons: HashSet<Button>,
    scroll_remainder: (i32, i32),
}

impl NativeDesktop {
    pub fn new(view_only: bool) -> Result<Self> {
        let monitor = Monitor::all()?
            .into_iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .context("No primary display was found.")?;
        #[cfg(target_os = "macos")]
        let capture = macos::Capture::new(&monitor)?;
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
        Ok(Self {
            monitor,
            #[cfg(target_os = "macos")]
            capture,
            input,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            scroll_remainder: (0, 0),
        })
    }

    fn move_pointer(&mut self, point: Point) -> Result<()> {
        if let Some(input) = self.input.as_mut() {
            let w = self.monitor.width()?;
            let h = self.monitor.height()?;
            let x = self.monitor.x()? + map_axis(point.x, w);
            let y = self.monitor.y()? + map_axis(point.y, h);
            input.move_mouse(x, y, Coordinate::Abs)?;
        }
        Ok(())
    }
}

fn map_axis(normalized: u16, pixels: u32) -> i32 {
    ((u64::from(normalized) * u64::from(pixels.saturating_sub(1)) + 32767) / 65535) as i32
}

impl Desktop for NativeDesktop {
    fn frame(&mut self) -> Result<Vec<u8>> {
        #[cfg(target_os = "macos")]
        let raw = self.capture.frame()?;
        #[cfg(target_os = "windows")]
        let raw = self.monitor.capture_image()?;
        let image = DynamicImage::ImageRgba8(raw)
            .thumbnail(1440, 1000)
            .into_rgb8();
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 68).encode_image(&image)?;
        Ok(jpeg)
    }

    fn input(&mut self, event: Input) -> Result<()> {
        match event {
            Input::Move(at) => self.move_pointer(at)?,
            Input::Button { at, button, down } => {
                self.move_pointer(at)?;
                if let Some(input) = self.input.as_mut() {
                    if down && !self.buttons.contains(&button) {
                        input.button(native_button(button), Direction::Press)?;
                        self.buttons.insert(button);
                    } else if !down && self.buttons.contains(&button) {
                        input.button(native_button(button), Direction::Release)?;
                        self.buttons.remove(&button);
                    }
                }
            }
            Input::Key { code, down } => {
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
        }
        Ok(())
    }

    fn release(&mut self) {
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

fn native_button(button: Button) -> enigo::Button {
    match button {
        Button::Left => enigo::Button::Left,
        Button::Right => enigo::Button::Right,
        Button::Middle => enigo::Button::Middle,
    }
}

pub fn decode_frame(bytes: Vec<u8>) -> Result<eframe::egui::ColorImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.into_rgba8();
    ensure!(
        image.width() > 0 && image.height() > 0,
        "The host sent an empty frame."
    );
    Ok(eframe::egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}

#[cfg(target_os = "windows")]
fn native_key(input: &mut Enigo, code: u16, direction: Direction) -> Result<bool> {
    input.key(enigo::Key::Other(code.into()), direction)?;
    Ok(true)
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
    use anyhow::{Context, Result, ensure};
    use image::RgbaImage;
    use screencapturekit::{
        prelude::{SCContentFilter, SCShareableContent, SCStreamConfiguration},
        screenshot_manager::{CGImageExt, SCScreenshotManager},
    };
    use xcap::Monitor;

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
            "Allow screen recording in System Settings, then reopen LAN Remote."
        );
        Ok(())
    }

    pub struct Capture {
        filter: SCContentFilter,
        configuration: SCStreamConfiguration,
    }

    impl Capture {
        pub fn new(monitor: &Monitor) -> Result<Self> {
            let id = monitor.id()?;
            let display = SCShareableContent::get()?
                .displays()
                .into_iter()
                .find(|d| d.display_id() == id)
                .context("The primary display is unavailable to ScreenCaptureKit.")?;
            let (width, height) = (display.width().max(1), display.height().max(1));
            let scale = (1440.0 / f64::from(width))
                .min(1000.0 / f64::from(height))
                .min(1.0);
            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build()?;
            let configuration = SCStreamConfiguration::new()
                .with_width((f64::from(width) * scale).round().max(1.0) as u32)
                .with_height((f64::from(height) * scale).round().max(1.0) as u32)
                .with_shows_cursor(true);
            Ok(Self {
                filter,
                configuration,
            })
        }

        pub fn frame(&self) -> Result<RgbaImage> {
            let frame = SCScreenshotManager::capture_image(&self.filter, &self.configuration)?;
            let (width, height) = (frame.width().try_into()?, frame.height().try_into()?);
            RgbaImage::from_raw(width, height, frame.rgba_data()?)
                .context("ScreenCaptureKit returned an invalid pixel buffer.")
        }
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
