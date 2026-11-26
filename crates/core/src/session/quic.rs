#[cfg(windows)]
#[path = "quic_windows.rs"]
mod windows_runtime;

mod aux;
mod aux_types;
mod host;
mod protocol;
mod transport;
mod viewer;

#[cfg(test)]
mod tests;

pub use aux::AuxSender;
pub use aux_types::{
    AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioFrame, ClipboardData, ClipboardImageFormat,
    CursorImage, CursorShape, CursorUpdate, MAX_AUDIO_SAMPLES, MAX_CLIPBOARD_IMAGE_BYTES,
    MAX_CLIPBOARD_TEXT_BYTES, MAX_CURSOR_DIMENSION, MAX_FILE_TRANSFER_BYTES, ReceivedFile,
    SessionAuxiliary, sanitize_relative_path, unique_destination,
};
pub use host::{
    HostInput, HostInputReceiver, HostSession, HostVideoSender, PendingHostSession, SessionHost,
};
pub use protocol::{
    CODEC_BIT_AV1, CODEC_BIT_H264, CODEC_BIT_HEVC, CodecPreference, DisplayPreference,
    StreamPreference, negotiate_codec,
};
pub use transport::TransportStats;
pub use viewer::ViewerSession;
