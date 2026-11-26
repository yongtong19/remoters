//! Unified desktop capture facade over DXGI Desktop Duplication (DDA) and
//! Windows Graphics Capture (WGC).

use anyhow::Result;
use gpui::ID3D11Texture2D;

use super::{
    capture_dda::{DdaCapture, DdaSlotLease},
    capture_wgc::{WgcCapture, WgcSlotLease},
};

#[allow(dead_code)]
pub(super) enum InputLease {
    Dda(DdaSlotLease),
    Wgc(WgcSlotLease),
}

pub(super) struct AcquiredInput {
    pub(super) texture: ID3D11Texture2D,
    pub(super) source_width: i32,
    pub(super) source_height: i32,
    pub(super) capture_stamp: i64,
    pub(super) present_100ns: i64,
    pub(super) arrived_100ns: i64,
    pub(super) _lease: InputLease,
}

pub(super) enum CaptureBackend {
    Dda(DdaCapture),
    Wgc(WgcCapture),
}

impl CaptureBackend {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Dda(_) => "dda",
            Self::Wgc(_) => "wgc",
        }
    }

    pub(super) fn acquire(&mut self) -> Result<Option<AcquiredInput>> {
        match self {
            Self::Dda(dda) => dda.acquire(),
            Self::Wgc(wgc) => wgc.acquire(),
        }
    }
}
