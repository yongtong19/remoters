use std::sync::Arc;

use anyhow::{Result, anyhow};
use remoters_core::session::CursorShape;
use xcap::Monitor;

use crate::platform::VideoFrame;

pub fn make_video_frame(surface: Arc<gpui::RenderImage>, width: u32, height: u32) -> VideoFrame {
    VideoFrame {
        surface,
        width,
        height,
    }
}

#[allow(dead_code)]
pub fn video_frame_dimensions(frame: &VideoFrame) -> (u32, u32) {
    (frame.width, frame.height)
}

#[allow(dead_code)]
pub fn list_displays() -> Result<Vec<Monitor>> {
    Monitor::all().map_err(|err| anyhow!("Could not enumerate Linux displays: {err}"))
}

#[allow(dead_code)]
pub fn display_label(monitor: &Monitor) -> String {
    let name = monitor.name().unwrap_or_else(|_| "Display".to_owned());
    match crate::codec::capture_dimensions(monitor) {
        Ok((width, height)) => format!("{name} · {width} × {height}"),
        Err(_) => name,
    }
}

pub fn monitor_physical_bounds(monitor: &Monitor) -> Result<(i32, i32, u32, u32)> {
    let scale = match monitor.scale_factor()? {
        factor if factor.is_finite() && factor >= 1.0 => factor,
        _ => 1.0,
    };
    let x = (monitor.x()? as f32 * scale).round() as i32;
    let y = (monitor.y()? as f32 * scale).round() as i32;
    let width = ((monitor.width()? as f32 * scale).round() as u32).max(1);
    let height = ((monitor.height()? as f32 * scale).round() as u32).max(1);
    Ok((x, y, width, height))
}

pub struct CursorDetector;

impl CursorDetector {
    pub fn new() -> Self {
        Self
    }

    pub fn poll(&self) -> CursorShape {
        if crate::platform::wayland_portal::is_embedded_cursor_active() {
            CursorShape::Hidden
        } else {
            CursorShape::Arrow
        }
    }
}

#[cfg(test)]
mod tests {
    use remoters_core::session::CursorShape;

    use super::CursorDetector;

    #[test]
    fn cursor_detector_defaults_to_arrow_when_no_embedded_wayland_cursor_is_active() {
        assert_eq!(CursorDetector::new().poll(), CursorShape::Arrow);
    }
}
