//! In-process VideoToolbox codec sessions on macOS.

mod decoder;
mod encoder;
mod ffi;
mod rate_control;
#[cfg(test)]
mod tests;

pub use decoder::{Decoder, supports_av1_hardware_decoder};
pub(super) use encoder::HardwareEncoder;
