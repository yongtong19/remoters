//! Linux desktop capture, hardware/software encoding, and video decoding.

#[cfg(target_os = "linux")]
mod capture;
#[cfg(target_os = "linux")]
mod decoder;
#[cfg(target_os = "linux")]
mod encoder;
#[cfg(any(target_os = "linux", test))]
mod ffmpeg_args;
#[cfg(any(target_os = "linux", test))]
mod ivf;
#[cfg(any(target_os = "linux", test))]
mod nvenc;
#[cfg(any(target_os = "linux", test))]
mod nvenc_ffi;
#[cfg(any(target_os = "linux", test))]
mod pipewire;
#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub use crate::platform::linux::CursorDetector;

#[cfg(target_os = "linux")]
pub use decoder::Decoder;
#[cfg(target_os = "linux")]
pub use encoder::Encoder;
#[cfg(test)]
pub use ffmpeg_args::{
    EncoderArgsConfig, LinuxDecodeBackend, LinuxVideoBackend, build_decoder_args,
    build_decoder_args_for, build_encoder_args, build_encoder_args_with_input,
    low_latency_vbv_kbps, resolve_backend_from_env,
};
#[cfg(target_os = "linux")]
pub use ffmpeg_args::{
    probe_av1_decoder, probe_av1_encoder, probe_hdr_encoder, probe_hevc_decoder, probe_hevc_encoder,
};

/// Converts packed 32-bit RGBA pixel data to BGRA in place for GPUI's `RenderImage`.
#[cfg(test)]
pub fn rgba_to_bgra(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

/// Returns a BGRA copy of packed 32-bit RGBA pixel data.
#[cfg(test)]
pub fn rgba_bytes_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut bgra = rgba.to_vec();
    rgba_to_bgra(&mut bgra);
    bgra
}
