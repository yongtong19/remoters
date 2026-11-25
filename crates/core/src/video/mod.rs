//! Compressed video formats and Annex B access units.
mod annex_b;

use std::sync::Arc;

use anyhow::{Result, ensure};
use bytes::Bytes;

pub(crate) use annex_b::inspect;
pub use annex_b::{AnnexBReader, access_units, nal_units};

/// Maximum capture dimensions for either landscape or portrait displays.
pub const MAX_LONG_EDGE: u32 = 3840;
pub const MAX_SHORT_EDGE: u32 = 2160;
pub const MAX_ACCESS_UNIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Codec {
    H264 = 1,
    Hevc = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Format {
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    pub frames_per_second: u16,
}

impl Format {
    pub fn validate(self) -> Result<()> {
        ensure!(
            Self::supports_dimensions(self.width.into(), self.height.into())
                && (1..=120).contains(&self.frames_per_second),
            "Unsupported video format."
        );
        Ok(())
    }

    pub fn supports_dimensions(width: u32, height: u32) -> bool {
        (2..=MAX_SHORT_EDGE).contains(&width.min(height))
            && width.max(height) <= MAX_LONG_EDGE
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
    }
}

/// One Annex B access unit in decode order. A random-access unit must carry
/// its parameter sets so the decoder can start or recover at that unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessUnit {
    pub sequence: u64,
    bytes: Bytes,
    random_access: bool,
    rtp_timestamp: Option<u64>,
}

impl AccessUnit {
    pub fn from_annex_b(codec: Codec, sequence: u64, bytes: Bytes) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_ACCESS_UNIT,
            "Invalid video access unit length."
        );
        let random_access = annex_b::inspect(codec, &bytes)?;
        Ok(Self {
            sequence,
            bytes,
            random_access,
            rtp_timestamp: None,
        })
    }

    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Marks an access unit with a capture-based timestamp in the 90 kHz video clock.
    pub fn with_rtp_timestamp(mut self, timestamp: u64) -> Self {
        self.rtp_timestamp = Some(timestamp);
        self
    }

    pub(crate) fn rtp_timestamp(&self) -> Option<u64> {
        self.rtp_timestamp
    }

    pub fn is_random_access(&self) -> bool {
        self.random_access
    }
}

/// Reassembled video as delivered by WebRTC. Timestamps use the RTP 90 kHz
/// video clock and have a session-specific origin.
pub struct ReceivedFrame {
    pub codec: Codec,
    pub rtp_timestamp: u64,
    pub bytes: Arc<[u8]>,
}
