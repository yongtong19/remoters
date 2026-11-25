//! Direct session transports. The desktop applications use QUIC.
mod quic;
pub mod webrtc;

pub use quic::{
    HostInput, HostInputReceiver, HostSession, HostVideoSender, SessionHost, ViewerSession,
};
