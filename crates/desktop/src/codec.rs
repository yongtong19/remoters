use anyhow::{Result, ensure};
use remoters_core::video::{Codec, Format, MAX_LONG_EDGE, MAX_SHORT_EDGE};

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use apple::Decoder;
#[cfg(target_os = "macos")]
pub use macos::Encoder;
#[cfg(target_os = "windows")]
pub use windows::{Decoder, Encoder};

const FPS: u16 = 60;
const MAX_FPS: u16 = 120;

pub fn desktop_format(monitor: &xcap::Monitor, new_virtual: bool) -> Result<Format> {
    // A newly plugged display can briefly report the OS placeholder mode.
    let (width, height) = if new_virtual {
        (3840, 2160)
    } else {
        capture_dimensions(monitor)?
    };
    let fps = if new_virtual {
        FPS
    } else {
        match monitor.frequency() {
            Ok(hz) if hz.is_finite() && hz >= f32::from(FPS) => {
                (hz.round() as u16).clamp(FPS, MAX_FPS)
            }
            _ => FPS,
        }
    };
    display_format_with_fps(width, height, fps)
}

pub fn capture_dimensions(monitor: &xcap::Monitor) -> Result<(u32, u32)> {
    let dimensions = (monitor.width()?, monitor.height()?);
    // XCap returns logical screen points on macOS and device pixels on Windows.
    #[cfg(target_os = "macos")]
    let dimensions = {
        let scale = match monitor.scale_factor()? {
            factor if factor.is_finite() && factor >= 1.0 => factor,
            _ => 1.0,
        };
        (
            (dimensions.0 as f32 * scale).round() as u32,
            (dimensions.1 as f32 * scale).round() as u32,
        )
    };
    Ok(dimensions)
}

#[cfg(test)]
fn display_format(width: u32, height: u32) -> Result<Format> {
    display_format_with_fps(width, height, FPS)
}

fn display_format_with_fps(width: u32, height: u32, frames_per_second: u16) -> Result<Format> {
    ensure!(
        width >= 2 && height >= 2,
        "The display has invalid dimensions."
    );
    let limits = if width >= height {
        (MAX_LONG_EDGE, MAX_SHORT_EDGE)
    } else {
        (MAX_SHORT_EDGE, MAX_LONG_EDGE)
    };
    let scale = (f64::from(limits.0) / f64::from(width))
        .min(f64::from(limits.1) / f64::from(height))
        .min(1.0);
    let dimension = |pixels| ((f64::from(pixels) * scale).floor() as u16).max(2) & !1;
    let format = Format {
        codec: Codec::H264,
        width: dimension(width),
        height: dimension(height),
        frames_per_second,
    };
    format.validate()?;
    Ok(format)
}

pub fn target_bitrate(format: Format) -> u32 {
    let area = u32::from(format.width) * u32::from(format.height);
    let at_sixty: u64 = match area {
        0..=2_073_600 => 10_000_000,
        2_073_601..=3_686_400 => 16_000_000,
        _ => 24_000_000,
    };
    let fps = u64::from(format.frames_per_second);
    let scaled = if fps <= 60 {
        at_sixty * fps / 60
    } else {
        // High-refresh desktop updates are mostly incremental; scale modestly above 60 FPS
        // to keep per-frame burst sizes small over Wi-Fi LANs.
        at_sixty + at_sixty * 35 * (fps - 60) / (100 * 60)
    };
    scaled.max(2_000_000) as u32
}

#[cfg(test)]
mod tests {
    use super::display_format;

    #[test]
    fn keeps_native_pixels_until_the_4k_capture_limit() {
        for (source, expected) in [
            ((1920, 1080), (1920, 1080)),
            ((2560, 1440), (2560, 1440)),
            ((3840, 2160), (3840, 2160)),
            ((2160, 3840), (2160, 3840)),
            ((5120, 2880), (3840, 2160)),
        ] {
            let format = display_format(source.0, source.1).unwrap();
            assert_eq!((format.width, format.height), expected);
        }
    }
}
