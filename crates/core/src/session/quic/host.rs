use std::{
    net::{SocketAddr, SocketAddrV4},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use quinn::{
    Connection, Endpoint, RecvStream, SendDatagramError, SendStream,
    crypto::rustls::QuicServerConfig,
};
use rustls::crypto::ring::default_provider;
use subtle::ConstantTimeEq;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::timeout,
};

use super::{
    aux::{AuxSender, accept_aux_loop, audio_send_loop, cursor_send_loop, recv_optional_aux},
    aux_types::{
        AudioFrame, AudioSendQueue, ClipboardData, CursorUpdate, ReceivedFile, SessionAuxiliary,
    },
    protocol::{
        BITRATE_REQUEST, DisplayPreference, FRAME_HEADER, HELLO_SIZE, INPUT_REQUEST, INPUT_SIZE,
        KEYFRAME_REQUEST, StreamPreference, decode_bitrate_mbps, format_packet,
    },
    transport::{
        ALPN, CLOSE_NORMAL, CLOSE_REJECTED, CONNECT_TIMEOUT, FEC_PACING_BURST_SHARDS,
        FRAME_EXPIRED, TransportStats, VIDEO_WRITE_TIMEOUT, endpoint, normal_close,
        normal_read_close, transport,
    },
};
use crate::{
    Input,
    network::{self, Pairing},
    session::fec,
    video::{AccessUnit, Format},
};

pub struct SessionHost {
    endpoint: Endpoint,
    pairing: Pairing,
    format: Option<Format>,
    view_only: bool,
}

impl SessionHost {
    pub fn bind(address: SocketAddrV4, format: Format, view_only: bool) -> Result<Self> {
        format.validate()?;
        Self::bind_internal(address, Some(format), view_only)
    }

    pub fn bind_dynamic(address: SocketAddrV4, view_only: bool) -> Result<Self> {
        Self::bind_internal(address, None, view_only)
    }

    fn bind_internal(
        address: SocketAddrV4,
        format: Option<Format>,
        view_only: bool,
    ) -> Result<Self> {
        ensure!(
            network::is_allowed_ip(*address.ip()),
            "Use a private, shared VPN (100.64.0.0/10), link-local, or loopback IPv4 address."
        );
        let (pairing, certificate, key) = Pairing::identity()?;
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        config.transport_config(transport(true));
        config.migration(false);
        let endpoint = endpoint(Some(config), address)?;
        Ok(Self {
            endpoint,
            pairing,
            format,
            view_only,
        })
    }

    pub fn address(&self) -> Result<SocketAddrV4> {
        match self.endpoint.local_addr()? {
            SocketAddr::V4(address) => Ok(address),
            _ => bail!("Expected an IPv4 QUIC endpoint."),
        }
    }

    pub fn pairing_code(&self) -> String {
        self.pairing.code()
    }

    pub async fn accept_pending(&self) -> Result<PendingHostSession> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .context("QUIC listener closed.")?;
        if !network::is_allowed_peer(incoming.remote_address()) {
            incoming.refuse();
            bail!(
                "The QUIC peer must use a private, shared VPN, link-local, or loopback IPv4 address."
            );
        }
        let (connection, reply, control, stream_preference) = timeout(CONNECT_TIMEOUT, async {
            let connection = incoming.await?;
            let setup = async {
                let (reply, mut control) = connection.accept_bi().await?;
                let mut hello = [0; HELLO_SIZE];
                control.read_exact(&mut hello).await?;
                ensure!(
                    bool::from(hello[..24].ct_eq(self.pairing.secret())),
                    "Invalid pairing code."
                );
                let pref_bytes: [u8; 13] = hello[24..].try_into()?;
                let stream_preference = StreamPreference::decode(pref_bytes)?;
                Ok::<_, anyhow::Error>((reply, control, stream_preference))
            };
            match setup.await {
                Ok((reply, control, stream_preference)) => {
                    Ok((connection, reply, control, stream_preference))
                }
                Err(error) => {
                    connection.close(CLOSE_REJECTED, b"pairing rejected");
                    Err(error)
                }
            }
        })
        .await??;
        Ok(PendingHostSession {
            connection,
            reply: Some(reply),
            control: Some(control),
            stream_preference,
            view_only: self.view_only,
            finished: false,
        })
    }

    pub async fn accept(&self) -> Result<HostSession> {
        let format = self.format.context(
            "SessionHost::accept requires a fixed format; use accept_pending with bind_dynamic.",
        )?;
        let pending = self.accept_pending().await?;
        pending.start(format).await
    }
}

pub struct PendingHostSession {
    connection: Connection,
    reply: Option<SendStream>,
    control: Option<RecvStream>,
    stream_preference: StreamPreference,
    view_only: bool,
    finished: bool,
}

impl PendingHostSession {
    pub fn peer_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    pub fn display_preference(&self) -> Option<DisplayPreference> {
        self.stream_preference.display
    }

    pub fn stream_preference(&self) -> StreamPreference {
        self.stream_preference
    }

    pub async fn start(self, format: Format) -> Result<HostSession> {
        self.start_with_receive_dir(format, None).await
    }

    pub async fn start_with_receive_dir(
        self,
        format: Format,
        receive_dir: Option<PathBuf>,
    ) -> Result<HostSession> {
        self.start_with_options(format, None, receive_dir).await
    }

    pub async fn start_with_options(
        mut self,
        format: Format,
        view_only_override: Option<bool>,
        receive_dir: Option<PathBuf>,
    ) -> Result<HostSession> {
        format.validate()?;
        let effective_view_only = view_only_override.unwrap_or(self.view_only);
        let mut reply = self
            .reply
            .take()
            .context("Pending session already started.")?;
        let control = self
            .control
            .take()
            .context("Pending session already started.")?;
        reply.set_priority(10)?;
        reply
            .write_all(&format_packet(format, !effective_view_only))
            .await?;
        reply.finish()?;
        self.finished = true;
        let (incoming, input) = mpsc::channel(96);
        let (requests, feedback) = mpsc::channel(1);
        let (aux_tx, aux_rx) = mpsc::channel(64);
        let (cursor_tx, cursor_rx) = watch::channel(None);
        let audio_queue = AudioSendQueue::new();
        let worker = tokio::spawn(read_input(
            self.connection.clone(),
            control,
            effective_view_only,
            incoming,
            requests.clone(),
        ));
        let aux_worker = tokio::spawn(accept_aux_loop(
            self.connection.clone(),
            !effective_view_only,
            false,
            receive_dir,
            aux_tx,
            None,
        ));
        let cursor_worker = tokio::spawn(cursor_send_loop(self.connection.clone(), cursor_rx));
        let audio_worker = tokio::spawn(audio_send_loop(
            self.connection.clone(),
            audio_queue.clone(),
        ));
        let guard = Arc::new(HostConnectionGuard {
            connection: self.connection.clone(),
        });
        Ok(HostSession {
            video: HostVideoSender {
                _guard: guard.clone(),
                connection: self.connection.clone(),
                requests,
                cursor: cursor_tx,
                audio: audio_queue,
                previous: None,
                format,
            },
            inputs: HostInputReceiver {
                _guard: guard,
                worker,
                aux_worker,
                cursor_worker,
                audio_worker,
                input,
                feedback,
                aux: Some(aux_rx),
            },
        })
    }

    pub fn reject(mut self, reason: &[u8]) {
        self.finished = true;
        self.connection.close(CLOSE_REJECTED, reason);
    }
}

impl Drop for PendingHostSession {
    fn drop(&mut self) {
        if !self.finished {
            self.connection.close(CLOSE_REJECTED, b"session rejected");
        }
    }
}

#[derive(Debug)]
pub enum HostInput {
    Remote(Input),
    Keyframe,
    Bitrate(Option<u16>),
    Clipboard(ClipboardData),
    FileReceived(ReceivedFile),
}

enum ControlInput {
    Remote(Input),
    Bitrate(Option<u16>),
}

struct HostConnectionGuard {
    connection: Connection,
}

impl Drop for HostConnectionGuard {
    fn drop(&mut self) {
        self.connection.close(CLOSE_NORMAL, b"session ended");
    }
}

pub struct HostVideoSender {
    _guard: Arc<HostConnectionGuard>,
    pub(super) connection: Connection,
    requests: mpsc::Sender<()>,
    cursor: watch::Sender<Option<CursorUpdate>>,
    audio: Arc<AudioSendQueue>,
    previous: Option<u64>,
    format: Format,
}

impl HostVideoSender {
    pub fn aux_sender(&self) -> AuxSender {
        AuxSender {
            connection: self.connection.clone(),
            cursor: Some(self.cursor.clone()),
            audio: Some(self.audio.clone()),
        }
    }

    pub fn send_cursor(&self, update: CursorUpdate) -> Result<()> {
        update.validate()?;
        self.cursor
            .send(Some(update))
            .map_err(|_| anyhow::anyhow!("Host cursor channel closed."))
    }

    pub fn send_audio(&self, frame: AudioFrame) -> Result<()> {
        self.audio.push(frame)
    }

    pub fn peer_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    pub fn rtt(&self) -> Duration {
        self.connection.rtt()
    }

    pub fn transport_stats(&self) -> TransportStats {
        let stats = self.connection.stats();
        TransportStats {
            rx_bytes: stats.udp_rx.bytes,
            tx_bytes: stats.udp_tx.bytes,
            rtt: stats.path.rtt,
            lost_packets: stats.path.lost_packets,
        }
    }

    pub async fn send(&mut self, unit: &AccessUnit) -> Result<bool> {
        ensure!(
            self.previous.is_none_or(|last| unit.sequence > last),
            "Video sequence must increase."
        );
        if self.previous.and_then(|last| last.checked_add(1)) != Some(unit.sequence)
            && !unit.is_random_access()
        {
            let _ = self.requests.try_send(());
            return Ok(false);
        }
        let timestamp = unit.rtp_timestamp().unwrap_or_else(|| {
            unit.sequence.saturating_mul(90_000) / u64::from(self.format.frames_per_second)
        });
        let host_latency_us = unit.host_latency_us();

        // Fast 0-RTT path: shard + GF(2^8) Reed-Solomon FEC over QUIC datagrams whenever
        // the frame fits within the datagram shard budget. Oversized frames or unsupported
        // peers fall through seamlessly to unidirectional QUIC streams below.
        if let Some(max_dgram) = self.connection.max_datagram_size()
            && let Some(packets) = fec::encode_frame_datagrams(
                unit.sequence,
                timestamp,
                host_latency_us,
                unit.bytes(),
                max_dgram,
            )?
        {
            let total_shards = packets.len();
            let mut fallback_to_stream = false;
            for (idx, pkt) in packets.into_iter().enumerate() {
                let send_res = if self.connection.datagram_send_buffer_space() < pkt.len() {
                    match timeout(VIDEO_WRITE_TIMEOUT, self.connection.send_datagram_wait(pkt))
                        .await
                    {
                        Ok(res) => res,
                        Err(_) => {
                            let _ = self.requests.try_send(());
                            return Ok(false);
                        }
                    }
                } else {
                    self.connection.send_datagram(pkt)
                };
                match send_res {
                    Ok(()) => {
                        if total_shards > FEC_PACING_BURST_SHARDS
                            && (idx + 1) % FEC_PACING_BURST_SHARDS == 0
                        {
                            tokio::task::yield_now().await;
                        }
                    }
                    Err(
                        SendDatagramError::TooLarge
                        | SendDatagramError::Disabled
                        | SendDatagramError::UnsupportedByPeer,
                    ) => {
                        fallback_to_stream = true;
                        break;
                    }
                    Err(SendDatagramError::ConnectionLost(error)) => return Err(error.into()),
                }
            }
            if !fallback_to_stream {
                self.previous = Some(unit.sequence);
                return Ok(true);
            }
        }

        let Ok(opened) = timeout(VIDEO_WRITE_TIMEOUT, self.connection.open_uni()).await else {
            let _ = self.requests.try_send(());
            return Ok(false);
        };
        let mut stream = opened?;
        let mut header = [0; FRAME_HEADER];
        header[..8].copy_from_slice(&unit.sequence.to_be_bytes());
        header[8..16].copy_from_slice(&timestamp.to_be_bytes());
        header[16..20].copy_from_slice(&host_latency_us.to_be_bytes());
        let result = timeout(VIDEO_WRITE_TIMEOUT, async {
            stream.write_all(&header).await?;
            stream.write_chunk(unit.bytes().clone()).await?;
            stream.finish()?;
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if result.is_err() {
            let _ = stream.reset(FRAME_EXPIRED);
        }
        match result {
            Ok(Ok(())) => {
                self.previous = Some(unit.sequence);
                Ok(true)
            }
            Ok(Err(error))
                if matches!(error.downcast_ref::<quinn::WriteError>(),
                    Some(quinn::WriteError::Stopped(code)) if *code == FRAME_EXPIRED) =>
            {
                let _ = self.requests.try_send(());
                Ok(false)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                let _ = self.requests.try_send(());
                Ok(false)
            }
        }
    }
}

pub struct HostInputReceiver {
    _guard: Arc<HostConnectionGuard>,
    worker: JoinHandle<Result<()>>,
    aux_worker: JoinHandle<Result<()>>,
    cursor_worker: JoinHandle<Result<()>>,
    audio_worker: JoinHandle<Result<()>>,
    input: mpsc::Receiver<ControlInput>,
    feedback: mpsc::Receiver<()>,
    aux: Option<mpsc::Receiver<SessionAuxiliary>>,
}

impl HostInputReceiver {
    pub async fn next_input(&mut self) -> Result<Option<HostInput>> {
        loop {
            tokio::select! {
                biased;
                Some(()) = self.feedback.recv() => return Ok(Some(HostInput::Keyframe)),
                aux = recv_optional_aux(&mut self.aux), if self.aux.is_some() => {
                    if let Some(item) = aux {
                        match item {
                            SessionAuxiliary::Clipboard(data) => {
                                return Ok(Some(HostInput::Clipboard(data)));
                            }
                            SessionAuxiliary::FileReceived(file) => {
                                return Ok(Some(HostInput::FileReceived(file)));
                            }
                            SessionAuxiliary::Cursor(_) => continue,
                        }
                    }
                    self.aux = None;
                }
                event = self.input.recv() => return Ok(match event {
                    Some(ControlInput::Remote(event)) => Some(HostInput::Remote(event)),
                    Some(ControlInput::Bitrate(mbps)) => Some(HostInput::Bitrate(mbps)),
                    None => {
                        (&mut self.worker).await??;
                        None
                    }
                }),
            }
        }
    }
}

impl Drop for HostInputReceiver {
    fn drop(&mut self) {
        self.worker.abort();
        self.aux_worker.abort();
        self.cursor_worker.abort();
        self.audio_worker.abort();
    }
}

pub struct HostSession {
    pub(super) video: HostVideoSender,
    inputs: HostInputReceiver,
}

impl HostSession {
    pub fn split(self) -> (HostVideoSender, HostInputReceiver) {
        (self.video, self.inputs)
    }

    pub fn aux_sender(&self) -> AuxSender {
        self.video.aux_sender()
    }

    pub fn send_cursor(&self, update: CursorUpdate) -> Result<()> {
        self.video.send_cursor(update)
    }

    pub fn send_audio(&self, frame: AudioFrame) -> Result<()> {
        self.video.send_audio(frame)
    }

    pub fn peer_address(&self) -> SocketAddr {
        self.video.peer_address()
    }

    pub fn rtt(&self) -> Duration {
        self.video.rtt()
    }

    pub async fn next_input(&mut self) -> Result<Option<HostInput>> {
        self.inputs.next_input().await
    }

    pub async fn send(&mut self, unit: &AccessUnit) -> Result<bool> {
        self.video.send(unit).await
    }
}

async fn read_input(
    connection: Connection,
    mut control: RecvStream,
    view_only: bool,
    input: mpsc::Sender<ControlInput>,
    feedback: mpsc::Sender<()>,
) -> Result<()> {
    let mut buf = [0; 1 + INPUT_SIZE];
    let mut filled = 0;
    let mut epoch = 0u64;
    let mut latest_movement = 0;
    let mut early = None;
    loop {
        tokio::select! {
            biased;
            read = control.read(&mut buf[filled..]) => {
                let count = match read {
                    Ok(Some(count)) => count,
                    Ok(None) => {
                        ensure!(filled == 0, "Truncated QUIC control message.");
                        return Ok(());
                    }
                    Err(error) if normal_read_close(&error) => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                filled += count;
                let mut offset = 0usize;
                while offset < filled {
                    let kind = buf[offset];
                    let msg_len = match kind {
                        KEYFRAME_REQUEST => 1,
                        BITRATE_REQUEST => 3,
                        INPUT_REQUEST => 1 + INPUT_SIZE,
                        _ => bail!("Unknown QUIC control message: {kind}."),
                    };
                    if filled - offset < msg_len {
                        break;
                    }
                    let msg = &buf[offset..offset + msg_len];
                    offset += msg_len;
                    match kind {
                        KEYFRAME_REQUEST => {
                            let _ = feedback.try_send(());
                        }
                        BITRATE_REQUEST => {
                            let mbps = decode_bitrate_mbps(u16::from_be_bytes([msg[1], msg[2]]))?;
                            input.send(ControlInput::Bitrate(mbps)).await?;
                        }
                        INPUT_REQUEST => {
                            let packet = &msg[1..];
                            let length = usize::from(packet[0]);
                            ensure!(
                                (1..=INPUT_SIZE - 9).contains(&length),
                                "Invalid QUIC input length."
                            );
                            let parsed = Input::parse(&packet[9..9 + length])?;
                            let next = u64::from_be_bytes(packet[1..9].try_into()?);
                            ensure!(
                                !matches!(parsed, Input::Move(_)) && epoch.checked_add(1) == Some(next),
                                "Invalid ordered input."
                            );
                            epoch = next;
                            if !view_only {
                                input.send(ControlInput::Remote(parsed)).await?;
                            }
                            if let Some((event_epoch, index, event)) = early {
                                if event_epoch <= epoch { early = None; }
                                if event_epoch == epoch && index > latest_movement && !view_only {
                                    let _ = input.try_send(ControlInput::Remote(event));
                                    latest_movement = index;
                                }
                            }
                        }
                        _ => unreachable!(),
                    }
                }
                if offset > 0 {
                    buf.copy_within(offset..filled, 0);
                    filled -= offset;
                }
            }
            datagram = connection.read_datagram(), if !view_only => {
                let bytes = match datagram {
                    Ok(bytes) => bytes,
                    Err(error) if normal_close(&error) => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                if bytes.len() < 16 { continue; }
                let event_epoch = u64::from_be_bytes(bytes[..8].try_into()?);
                let index = u64::from_be_bytes(bytes[8..16].try_into()?);
                let Ok(event @ Input::Move(_)) = Input::parse(&bytes[16..]) else { continue };
                if event_epoch < epoch || index <= latest_movement { continue; }
                if event_epoch == epoch {
                    let _ = input.try_send(ControlInput::Remote(event));
                    latest_movement = index;
                } else if early.is_none_or(|(_, previous, _)| previous < index) {
                    // A datagram can overtake its button press or release.
                    early = Some((event_epoch, index, event));
                }
            }
        }
    }
}
