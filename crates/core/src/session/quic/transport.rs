use std::{
    net::{SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use quinn::{AckFrequencyConfig, Endpoint, SendStream, TransportConfig, VarInt};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::{sync::Mutex, time::timeout};

use super::protocol::FRAME_HEADER;
use crate::{session::congestion::RealtimeVideoConfig, video::MAX_ACCESS_UNIT};

pub(super) const ALPN: &[u8] = b"remoters-desktop/7";
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
pub(super) const VIDEO_WRITE_TIMEOUT: Duration = Duration::from_millis(120);
pub(super) const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
pub(super) const FRAME_TIMEOUT: Duration = Duration::from_millis(100);
pub(super) const REORDER_TIMEOUT: Duration = Duration::from_millis(25);
pub(super) const MAX_VIDEO_STREAMS: u32 = 8;
pub(super) const MAX_BIDI_STREAMS: u32 = 8;
pub(super) const DATAGRAM_BUFFER_SIZE: usize = 4 * 1024 * 1024;
pub(super) const UDP_SOCKET_BUFFER_SIZES: [usize; 3] =
    [4 * 1024 * 1024, 2 * 1024 * 1024, 1 * 1024 * 1024];
pub(super) const FEC_PACING_BURST_SHARDS: usize = 32;
pub(super) const CLOSE_NORMAL: VarInt = VarInt::from_u32(0);
pub(super) const CLOSE_REJECTED: VarInt = VarInt::from_u32(1);
pub(super) const FRAME_EXPIRED: VarInt = VarInt::from_u32(2);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransportStats {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rtt: Duration,
    pub lost_packets: u64,
}

pub(super) fn transport(server: bool) -> Arc<TransportConfig> {
    let mut acknowledgements = AckFrequencyConfig::default();
    acknowledgements.max_ack_delay(Some(Duration::from_millis(2)));
    let mut config = TransportConfig::default();
    config
        .ack_frequency_config(Some(acknowledgements))
        .initial_rtt(Duration::from_millis(10))
        .max_concurrent_bidi_streams(MAX_BIDI_STREAMS.into())
        .max_concurrent_uni_streams(if server { 0 } else { MAX_VIDEO_STREAMS }.into())
        .stream_receive_window(((MAX_ACCESS_UNIT + FRAME_HEADER) as u32).into())
        .receive_window(((2 * MAX_ACCESS_UNIT) as u32).into())
        .send_window((2 * MAX_ACCESS_UNIT) as u64)
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER_SIZE))
        .datagram_send_buffer_size(DATAGRAM_BUFFER_SIZE)
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
    if server {
        // Use a real-time video congestion controller with a 512 KiB initial window,
        // 256 KiB floor, and no ProbeRtt / idle cwnd collapse after static desktop silence.
        config.congestion_controller_factory(Arc::new(RealtimeVideoConfig::default()));
    }
    Arc::new(config)
}

pub(super) fn bind_udp_socket(address: SocketAddrV4) -> std::io::Result<std::net::UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    for size in UDP_SOCKET_BUFFER_SIZES {
        if socket.set_send_buffer_size(size).is_ok() {
            break;
        }
    }
    for size in UDP_SOCKET_BUFFER_SIZES {
        if socket.set_recv_buffer_size(size).is_ok() {
            break;
        }
    }
    socket.bind(&SockAddr::from(SocketAddr::V4(address)))?;
    Ok(socket.into())
}

pub(super) fn endpoint(
    server: Option<quinn::ServerConfig>,
    address: SocketAddrV4,
) -> std::io::Result<Endpoint> {
    #[cfg(windows)]
    let runtime = super::windows_runtime::WindowsRuntime;
    #[cfg(not(windows))]
    let runtime = quinn::TokioRuntime;
    Endpoint::new(
        quinn::EndpointConfig::default(),
        server,
        bind_udp_socket(address)?,
        Arc::new(runtime),
    )
}

pub(super) async fn write_control(control: &Mutex<SendStream>, packet: &[u8]) -> Result<()> {
    timeout(CONTROL_TIMEOUT, async {
        control.lock().await.write_all(packet).await
    })
    .await??;
    Ok(())
}

pub(super) fn normal_read_close(error: &quinn::ReadError) -> bool {
    matches!(error, quinn::ReadError::ConnectionLost(error) if normal_close(error))
}

pub(super) fn normal_close(error: &quinn::ConnectionError) -> bool {
    matches!(error, quinn::ConnectionError::ApplicationClosed(close) if close.error_code == CLOSE_NORMAL)
}
