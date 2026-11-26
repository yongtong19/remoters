use anyhow::{Context, Result, bail, ensure};

use crate::video::{Codec, Format, MAX_LONG_EDGE, MAX_SHORT_EDGE};

pub(super) const HELLO_SIZE: usize = 37;
pub(super) const INPUT_SIZE: usize = 24;
pub(super) const FRAME_HEADER: usize = 20;
pub(super) const KEYFRAME_REQUEST: u8 = 0;
pub(super) const BITRATE_REQUEST: u8 = 1;
pub(super) const INPUT_REQUEST: u8 = 2;
const MIN_FPS_CAP: u16 = 30;
const MAX_FPS_CAP: u16 = 120;
const MIN_BITRATE_MBPS: u16 = 5;
const MAX_BITRATE_MBPS: u16 = 150;

pub const CODEC_BIT_H264: u8 = 1;
pub const CODEC_BIT_HEVC: u8 = 2;
pub const CODEC_BIT_AV1: u8 = 4;
const ALL_CODEC_BITS: u8 = CODEC_BIT_H264 | CODEC_BIT_HEVC | CODEC_BIT_AV1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayPreference {
    pub width: u16,
    pub height: u16,
    pub frames_per_second: u16,
}

impl DisplayPreference {
    pub fn new(width: u16, height: u16, frames_per_second: u16) -> Result<Self> {
        ensure!(
            Format::supports_dimensions(width.into(), height.into())
                && (1..=120).contains(&frames_per_second),
            "Unsupported display preference."
        );
        Ok(Self {
            width,
            height,
            frames_per_second,
        })
    }

    pub fn normalized(width: u32, height: u32, frames_per_second: u16) -> Result<Self> {
        ensure!(
            width >= 2 && height >= 2,
            "Display dimensions must be at least 2x2."
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
        Self::new(
            dimension(width),
            dimension(height),
            frames_per_second.clamp(1, 120),
        )
    }

    pub(super) fn encode(preference: Option<Self>) -> Result<[u8; 6]> {
        let Some(preference) = preference else {
            return Ok([0; 6]);
        };
        let validated = Self::new(
            preference.width,
            preference.height,
            preference.frames_per_second,
        )?;
        let [w1, w2] = validated.width.to_be_bytes();
        let [h1, h2] = validated.height.to_be_bytes();
        let [f1, f2] = validated.frames_per_second.to_be_bytes();
        Ok([w1, w2, h1, h2, f1, f2])
    }

    pub(super) fn decode(bytes: [u8; 6]) -> Result<Option<Self>> {
        if bytes == [0; 6] {
            return Ok(None);
        }
        let [w1, w2, h1, h2, f1, f2] = bytes;
        Self::new(
            u16::from_be_bytes([w1, w2]),
            u16::from_be_bytes([h1, h2]),
            u16::from_be_bytes([f1, f2]),
        )
        .map(Some)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum CodecPreference {
    #[default]
    Auto = 0,
    H264 = 1,
    Hevc = 2,
    Av1 = 3,
}

impl CodecPreference {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Auto),
            1 => Some(Self::H264),
            2 => Some(Self::Hevc),
            3 => Some(Self::Av1),
            _ => None,
        }
    }
}

impl TryFrom<u8> for CodecPreference {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        Self::from_byte(value).with_context(|| format!("Unknown codec preference: {value}."))
    }
}

impl From<CodecPreference> for u8 {
    fn from(preference: CodecPreference) -> Self {
        preference as u8
    }
}

/// Negotiates the stream codec using viewer/host capability masks with priority
/// `AV1 > HEVC > H.264`.
pub fn negotiate_codec(
    preference: CodecPreference,
    viewer_supported_mask: u8,
    host_supported_mask: u8,
) -> Codec {
    let viewer_mask = if viewer_supported_mask == 0 {
        CODEC_BIT_H264 | CODEC_BIT_HEVC | CODEC_BIT_AV1
    } else {
        viewer_supported_mask | CODEC_BIT_H264
    };
    let common = viewer_mask & (host_supported_mask | CODEC_BIT_H264);
    match preference {
        CodecPreference::Av1 if (common & CODEC_BIT_AV1) != 0 => return Codec::Av1,
        CodecPreference::Hevc if (common & CODEC_BIT_HEVC) != 0 => return Codec::Hevc,
        CodecPreference::H264 if (common & CODEC_BIT_H264) != 0 => return Codec::H264,
        _ => {}
    }
    if (common & CODEC_BIT_AV1) != 0 {
        Codec::Av1
    } else if (common & CODEC_BIT_HEVC) != 0 {
        Codec::Hevc
    } else {
        Codec::H264
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StreamPreference {
    pub display: Option<DisplayPreference>,
    pub codec: CodecPreference,
    pub max_fps: Option<u16>,
    pub target_bitrate_mbps: Option<u16>,
    pub supported_codecs: u8,
    pub hdr: bool,
}

impl StreamPreference {
    pub fn new(
        display: Option<DisplayPreference>,
        codec: CodecPreference,
        max_fps: Option<u16>,
        target_bitrate_mbps: Option<u16>,
    ) -> Result<Self> {
        let preference = Self {
            display,
            codec,
            max_fps,
            target_bitrate_mbps,
            supported_codecs: 0,
            hdr: false,
        };
        preference.validate()?;
        Ok(preference)
    }

    pub const fn with_supported_codecs(mut self, supported_codecs: u8) -> Self {
        self.supported_codecs = supported_codecs;
        self
    }

    pub const fn with_hdr(mut self, hdr: bool) -> Self {
        self.hdr = hdr;
        self
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(display) = self.display {
            DisplayPreference::new(display.width, display.height, display.frames_per_second)?;
        }
        if let Some(max_fps) = self.max_fps {
            ensure!(
                (MIN_FPS_CAP..=MAX_FPS_CAP).contains(&max_fps),
                "Unsupported max FPS preference: {max_fps} (expected {MIN_FPS_CAP}..={MAX_FPS_CAP})."
            );
        }
        if let Some(mbps) = self.target_bitrate_mbps {
            validate_bitrate_mbps(mbps)?;
        }
        ensure!(
            self.supported_codecs & !ALL_CODEC_BITS == 0,
            "Unsupported codec capability mask: {:#x}.",
            self.supported_codecs
        );
        Ok(())
    }

    pub(super) fn encode(&self) -> Result<[u8; 13]> {
        self.validate()?;
        let display = DisplayPreference::encode(self.display)?;
        let [f1, f2] = self.max_fps.unwrap_or(0).to_be_bytes();
        let [b1, b2] = self.target_bitrate_mbps.unwrap_or(0).to_be_bytes();
        let mut out = [0u8; 13];
        out[..6].copy_from_slice(&display);
        out[6] = u8::from(self.codec);
        out[7] = f1;
        out[8] = f2;
        out[9] = b1;
        out[10] = b2;
        out[11] = self.supported_codecs;
        out[12] = u8::from(self.hdr);
        Ok(out)
    }

    pub(super) fn decode(bytes: [u8; 13]) -> Result<Self> {
        let display_bytes: [u8; 6] = bytes[..6].try_into()?;
        let display = DisplayPreference::decode(display_bytes)?;
        let codec = CodecPreference::try_from(bytes[6])?;
        let max_fps = match u16::from_be_bytes([bytes[7], bytes[8]]) {
            0 => None,
            fps => Some(fps),
        };
        let target_bitrate_mbps = match u16::from_be_bytes([bytes[9], bytes[10]]) {
            0 => None,
            mbps => Some(mbps),
        };
        ensure!(bytes[12] <= 1, "Invalid HDR preference: {}.", bytes[12]);
        let preference = Self {
            display,
            codec,
            max_fps,
            target_bitrate_mbps,
            supported_codecs: bytes[11],
            hdr: bytes[12] == 1,
        };
        preference.validate()?;
        Ok(preference)
    }
}

fn validate_bitrate_mbps(mbps: u16) -> Result<()> {
    ensure!(
        (MIN_BITRATE_MBPS..=MAX_BITRATE_MBPS).contains(&mbps),
        "Bitrate {mbps} Mbps must be between {MIN_BITRATE_MBPS} and {MAX_BITRATE_MBPS} Mbps."
    );
    Ok(())
}

pub(super) fn encode_bitrate_mbps(mbps: Option<u16>) -> Result<u16> {
    match mbps {
        None => Ok(0),
        Some(value) => {
            validate_bitrate_mbps(value)?;
            Ok(value)
        }
    }
}

pub(super) fn decode_bitrate_mbps(raw: u16) -> Result<Option<u16>> {
    match raw {
        0 => Ok(None),
        value => {
            validate_bitrate_mbps(value)?;
            Ok(Some(value))
        }
    }
}

pub(super) fn format_packet(format: Format, can_control: bool) -> [u8; 9] {
    let [w1, w2] = format.width.to_be_bytes();
    let [h1, h2] = format.height.to_be_bytes();
    let [f1, f2] = format.frames_per_second.to_be_bytes();
    [
        format.codec as u8,
        u8::from(can_control),
        w1,
        w2,
        h1,
        h2,
        f1,
        f2,
        u8::from(format.hdr),
    ]
}

pub(super) fn parse_format(bytes: [u8; 9]) -> Result<(Format, bool)> {
    let [codec, control, w1, w2, h1, h2, f1, f2, hdr] = bytes;
    let codec = match codec {
        1 => Codec::H264,
        2 => Codec::Hevc,
        3 => Codec::Av1,
        _ => bail!("Unsupported video codec."),
    };
    ensure!(control <= 1 && hdr <= 1, "Invalid session options.");
    let format = Format {
        codec,
        width: u16::from_be_bytes([w1, w2]),
        height: u16::from_be_bytes([h1, h2]),
        frames_per_second: u16::from_be_bytes([f1, f2]),
        hdr: hdr == 1,
    };
    format.validate()?;
    Ok((format, control == 1))
}
