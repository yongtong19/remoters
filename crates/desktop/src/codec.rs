use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use anyhow::{Result, ensure};
use remoters_core::{
    session::{CODEC_BIT_AV1, CODEC_BIT_H264, CODEC_BIT_HEVC, CodecPreference, negotiate_codec},
    video::{Codec, Format, MAX_LONG_EDGE, MAX_SHORT_EDGE},
};

#[cfg(target_os = "macos")]
mod apple;
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use apple::Decoder;
#[cfg(target_os = "linux")]
pub use linux::{CursorDetector, Decoder, Encoder};
#[cfg(target_os = "macos")]
pub use macos::{CursorDetector, Encoder};
#[cfg(target_os = "windows")]
pub use windows::{CursorDetector, Decoder, Encoder};

const FPS: u16 = 60;
const MAX_FPS: u16 = 120;

#[derive(Clone)]
pub struct BitrateControl(Arc<AtomicU32>);

impl BitrateControl {
    pub(crate) fn from_raw(raw: Arc<AtomicU32>) -> Self {
        Self(raw)
    }

    pub fn set(&self, mbps: Option<u16>) {
        self.0
            .store(encode_bitrate_override(mbps), Ordering::Release);
    }

    #[allow(dead_code)]
    pub fn raw(&self) -> Arc<AtomicU32> {
        self.0.clone()
    }
}

pub(crate) fn encode_bitrate_override(mbps: Option<u16>) -> u32 {
    mbps.map_or(0, |m| u32::from(m.clamp(5, 150)))
}

pub(crate) fn decode_bitrate_override(raw: u32) -> Option<u16> {
    u16::try_from(raw).ok().filter(|&m| m > 0)
}

pub fn supports_av1_encode() -> bool {
    #[cfg(target_os = "macos")]
    {
        false
    }
    #[cfg(target_os = "windows")]
    {
        windows::supports_av1_hardware_encoder()
    }
    #[cfg(target_os = "linux")]
    {
        linux::probe_av1_encoder()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn supports_av1_decode() -> bool {
    #[cfg(target_os = "macos")]
    {
        apple::supports_av1_hardware_decoder()
    }
    #[cfg(target_os = "windows")]
    {
        windows::supports_av1_hardware_decoder()
    }
    #[cfg(target_os = "linux")]
    {
        linux::probe_av1_decoder()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn supports_hevc_encode() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "windows")]
    {
        windows::supports_hevc_hardware_encoder()
    }
    #[cfg(target_os = "linux")]
    {
        linux::probe_hevc_encoder()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn supports_hevc_decode() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "windows")]
    {
        windows::supports_hevc_hardware_decoder()
    }
    #[cfg(target_os = "linux")]
    {
        linux::probe_hevc_decoder()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn supports_hdr_encode() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "windows")]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        linux::probe_hdr_encoder()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn supports_hdr_decode() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "windows")]
    {
        true
    }
    #[cfg(target_os = "linux")]
    {
        false
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

pub fn host_supported_codec_mask() -> u8 {
    codec_mask_from_flags(supports_av1_encode(), supports_hevc_encode())
}

pub fn viewer_supported_codec_mask() -> u8 {
    codec_mask_from_flags(supports_av1_decode(), supports_hevc_decode())
}

const fn codec_mask_from_flags(av1: bool, hevc: bool) -> u8 {
    CODEC_BIT_H264 | if hevc { CODEC_BIT_HEVC } else { 0 } | if av1 { CODEC_BIT_AV1 } else { 0 }
}

pub fn resolve_host_codec(preference: CodecPreference, viewer_supported_mask: u8) -> Codec {
    negotiate_codec(
        preference,
        viewer_supported_mask,
        host_supported_codec_mask(),
    )
}

pub fn resolve_hdr_enabled(codec: Codec, requested_hdr: bool) -> bool {
    resolve_hdr_enabled_with(codec, requested_hdr, supports_hdr_encode())
}

const fn resolve_hdr_enabled_with(
    codec: Codec,
    requested_hdr: bool,
    hdr_encode_supported: bool,
) -> bool {
    requested_hdr && hdr_encode_supported && matches!(codec, Codec::Hevc | Codec::Av1)
}

#[allow(dead_code)]
pub fn resolve_viewer_codec_preference(preference: CodecPreference) -> CodecPreference {
    resolve_viewer_codec_preference_with(preference, supports_av1_decode(), supports_hevc_decode())
}

#[allow(dead_code)]
fn resolve_viewer_codec_preference_with(
    preference: CodecPreference,
    av1_decode_supported: bool,
    hevc_decode_supported: bool,
) -> CodecPreference {
    match preference {
        CodecPreference::Auto => {
            if av1_decode_supported {
                CodecPreference::Av1
            } else if hevc_decode_supported {
                CodecPreference::Hevc
            } else {
                CodecPreference::H264
            }
        }
        other => other,
    }
}

pub fn desktop_format(monitor: &xcap::Monitor, new_virtual: bool) -> Result<Format> {
    desktop_format_with(monitor, new_virtual, Codec::H264, None)
}

pub fn desktop_format_with(
    monitor: &xcap::Monitor,
    new_virtual: bool,
    codec: Codec,
    max_fps: Option<u16>,
) -> Result<Format> {
    desktop_format_with_hdr(monitor, new_virtual, codec, max_fps, false)
}

pub fn desktop_format_with_hdr(
    monitor: &xcap::Monitor,
    new_virtual: bool,
    codec: Codec,
    max_fps: Option<u16>,
    hdr: bool,
) -> Result<Format> {
    // A newly plugged display can briefly report the OS placeholder mode.
    let (width, height) = if new_virtual {
        (3840, 2160)
    } else {
        capture_dimensions(monitor)?
    };
    let mut fps = if new_virtual {
        FPS
    } else {
        match monitor.frequency() {
            Ok(hz) if hz.is_finite() && hz >= f32::from(FPS) => {
                (hz.round() as u16).clamp(FPS, MAX_FPS)
            }
            _ => FPS,
        }
    };
    if let Some(cap) = max_fps {
        fps = fps.min(cap.clamp(30, MAX_FPS));
    }
    display_format_with_fps_codec_and_hdr(width, height, fps, codec, hdr)
}

pub fn virtual_format(width: u32, height: u32, frames_per_second: u16) -> Result<Format> {
    virtual_format_with(width, height, frames_per_second, Codec::H264)
}

pub fn virtual_format_with(
    width: u32,
    height: u32,
    frames_per_second: u16,
    codec: Codec,
) -> Result<Format> {
    virtual_format_with_hdr(width, height, frames_per_second, codec, false)
}

pub fn virtual_format_with_hdr(
    width: u32,
    height: u32,
    frames_per_second: u16,
    codec: Codec,
    hdr: bool,
) -> Result<Format> {
    display_format_with_fps_codec_and_hdr(
        width,
        height,
        frames_per_second.clamp(30, MAX_FPS),
        codec,
        hdr,
    )
}

pub fn capture_dimensions(monitor: &xcap::Monitor) -> Result<(u32, u32)> {
    let dimensions = (monitor.width()?, monitor.height()?);
    // XCap returns logical screen points on macOS and Linux, and device pixels on Windows.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
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
    display_format_with_fps_codec_and_hdr(width, height, FPS, Codec::H264, false)
}

fn display_format_with_fps_codec_and_hdr(
    width: u32,
    height: u32,
    frames_per_second: u16,
    codec: Codec,
    hdr: bool,
) -> Result<Format> {
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
        codec,
        width: dimension(width),
        height: dimension(height),
        frames_per_second,
        hdr,
    };
    format.validate()?;
    Ok(format)
}

pub fn target_bitrate(format: Format) -> u32 {
    let area = u32::from(format.width) * u32::from(format.height);
    let at_sixty: u64 = match area {
        0..=2_073_600 => 20_000_000,
        2_073_601..=3_686_400 => 35_000_000,
        _ => 50_000_000,
    };
    let fps = u64::from(format.frames_per_second);
    let scaled = if fps <= 60 {
        at_sixty * fps / 60
    } else {
        // High-refresh desktop updates are mostly incremental; scale modestly above 60 FPS
        // to keep per-frame burst sizes small over Wi-Fi LANs.
        at_sixty + at_sixty * 45 * (fps - 60) / (100 * 60)
    };
    scaled.max(5_000_000) as u32
}

pub fn effective_bitrate(format: Format, override_mbps: Option<u16>) -> u32 {
    match override_mbps {
        Some(mbps) => u32::from(mbps.clamp(5, 150)) * 1_000_000,
        None => target_bitrate(format),
    }
}

#[cfg(test)]
mod tests {
    use remoters_core::{
        session::{CODEC_BIT_H264, CodecPreference, negotiate_codec},
        video::{Codec, Format},
    };

    use super::{
        codec_mask_from_flags, display_format, effective_bitrate, resolve_hdr_enabled_with,
        resolve_viewer_codec_preference_with, target_bitrate, virtual_format_with,
        virtual_format_with_hdr,
    };

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
            assert!(!format.hdr);
        }
    }

    #[test]
    fn raises_auto_bitrates_for_sharp_desktop_streaming() {
        let fhd_60 = Format {
            codec: Codec::Hevc,
            width: 1920,
            height: 1080,
            frames_per_second: 60,
            hdr: false,
        };
        assert_eq!(target_bitrate(fhd_60), 20_000_000);

        let qhd_60 = Format {
            codec: Codec::Hevc,
            width: 2560,
            height: 1440,
            frames_per_second: 60,
            hdr: false,
        };
        assert_eq!(target_bitrate(qhd_60), 35_000_000);

        let uhd_60 = Format {
            codec: Codec::Hevc,
            width: 3840,
            height: 2160,
            frames_per_second: 60,
            hdr: false,
        };
        assert_eq!(target_bitrate(uhd_60), 50_000_000);

        let retina_120 = Format {
            codec: Codec::Hevc,
            width: 3024,
            height: 1900,
            frames_per_second: 120,
            hdr: false,
        };
        assert_eq!(target_bitrate(retina_120), 72_500_000);

        let low_fps = Format {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            frames_per_second: 1,
            hdr: false,
        };
        assert_eq!(target_bitrate(low_fps), 5_000_000);
    }

    #[test]
    fn effective_bitrate_applies_and_clamps_manual_overrides() {
        let format = virtual_format_with(2560, 1440, 60, Codec::Hevc).unwrap();
        assert_eq!(format.codec, Codec::Hevc);
        assert!(!format.hdr);
        assert_eq!(effective_bitrate(format, None), 35_000_000);
        assert_eq!(effective_bitrate(format, Some(80)), 80_000_000);
        assert_eq!(effective_bitrate(format, Some(1)), 5_000_000);
        assert_eq!(effective_bitrate(format, Some(300)), 150_000_000);

        let av1_hdr = virtual_format_with_hdr(3840, 2160, 120, Codec::Av1, true).unwrap();
        assert_eq!(av1_hdr.codec, Codec::Av1);
        assert!(av1_hdr.hdr);
    }

    #[test]
    fn resolves_av1_gt_hevc_gt_h264_and_hdr_across_device_profiles() {
        let rtx_5070_ti_host = codec_mask_from_flags(true, true);
        let mac_host = codec_mask_from_flags(false, true);
        let m4_viewer = codec_mask_from_flags(true, true);
        let m2_pro_viewer = codec_mask_from_flags(false, true);
        let h264_only = codec_mask_from_flags(false, false);
        assert_eq!(h264_only, CODEC_BIT_H264);

        // M4 viewer + RTX 5070 Ti host: Auto prefers AV1 > HEVC > H.264.
        assert_eq!(
            negotiate_codec(CodecPreference::Auto, m4_viewer, rtx_5070_ti_host),
            Codec::Av1
        );
        assert_eq!(
            negotiate_codec(CodecPreference::Av1, m4_viewer, rtx_5070_ti_host),
            Codec::Av1
        );
        assert_eq!(
            negotiate_codec(CodecPreference::Hevc, m4_viewer, rtx_5070_ti_host),
            Codec::Hevc
        );
        assert_eq!(
            negotiate_codec(CodecPreference::H264, m4_viewer, rtx_5070_ti_host),
            Codec::H264
        );

        // M2 Pro viewer (!av1_decode) + RTX 5070 Ti host (av1_encode): falls back to HEVC.
        assert_eq!(
            negotiate_codec(CodecPreference::Auto, m2_pro_viewer, rtx_5070_ti_host),
            Codec::Hevc
        );
        assert_eq!(
            negotiate_codec(CodecPreference::Av1, m2_pro_viewer, rtx_5070_ti_host),
            Codec::Hevc
        );

        // M4 viewer (av1_decode) + macOS host (!av1_encode): falls back to HEVC.
        assert_eq!(
            negotiate_codec(CodecPreference::Auto, m4_viewer, mac_host),
            Codec::Hevc
        );

        // H.264-only fallback on either side.
        assert_eq!(
            negotiate_codec(CodecPreference::Auto, h264_only, rtx_5070_ti_host),
            Codec::H264
        );
        assert_eq!(
            negotiate_codec(CodecPreference::Av1, m4_viewer, h264_only),
            Codec::H264
        );

        // Viewer Auto preference reflects AV1 > HEVC > H.264.
        assert_eq!(
            resolve_viewer_codec_preference_with(CodecPreference::Auto, true, true),
            CodecPreference::Av1
        );
        assert_eq!(
            resolve_viewer_codec_preference_with(CodecPreference::Auto, false, true),
            CodecPreference::Hevc
        );
        assert_eq!(
            resolve_viewer_codec_preference_with(CodecPreference::Auto, false, false),
            CodecPreference::H264
        );
        assert_eq!(
            resolve_viewer_codec_preference_with(CodecPreference::Hevc, true, false),
            CodecPreference::Hevc
        );
        assert_eq!(
            resolve_viewer_codec_preference_with(CodecPreference::H264, true, true),
            CodecPreference::H264
        );

        // HDR requires requested_hdr && supports_hdr_encode && HEVC/AV1.
        assert!(resolve_hdr_enabled_with(Codec::Av1, true, true));
        assert!(resolve_hdr_enabled_with(Codec::Hevc, true, true));
        assert!(!resolve_hdr_enabled_with(Codec::H264, true, true));
        assert!(!resolve_hdr_enabled_with(Codec::Av1, false, true));
        assert!(!resolve_hdr_enabled_with(Codec::Av1, true, false));
    }
}
