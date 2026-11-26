//! Windows desktop capture, GPU surface conversion, and hardware video
//! encoding/decoding (direct NVENC SDK + Media Foundation MFT fallback).

mod capture;
mod capture_dda;
mod capture_wgc;
mod common;
mod d3d_convert;
mod mft_decoder;
mod mft_encoder;
mod nvenc;
mod nvenc_ffi;
mod pipeline;
#[cfg(test)]
mod tests;

pub use common::{
    CursorDetector, supports_av1_hardware_decoder, supports_av1_hardware_encoder,
    supports_hevc_hardware_decoder, supports_hevc_hardware_encoder,
};
pub use mft_decoder::Decoder;
#[allow(unused_imports)]
pub use pipeline::{Encoder, KeyframeTrigger};
