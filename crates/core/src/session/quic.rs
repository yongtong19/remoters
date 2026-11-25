#[cfg(windows)]
#[path = "quic_windows.rs"]
mod windows_runtime;

use std::{
    collections::BTreeMap,
    net::{SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use quinn::{
    AckFrequencyConfig, Connection, Endpoint, RecvStream, SendStream, TransportConfig, VarInt,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::crypto::ring::default_provider;
use subtle::ConstantTimeEq;
use tokio::{
    sync::{Mutex, mpsc},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep_until, timeout},
};

use crate::{
    Input,
    network::{self, Pairing},
    video::{AccessUnit, Codec, Format, MAX_ACCESS_UNIT, ReceivedFrame, inspect},
};

const ALPN: &[u8] = b"remoters-desktop/1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(7);
const VIDEO_WRITE_TIMEOUT: Duration = Duration::from_millis(30);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
const FRAME_TIMEOUT: Duration = Duration::from_millis(100);
const REORDER_TIMEOUT: Duration = Duration::from_millis(25);
const MAX_VIDEO_STREAMS: u32 = 8;
const LAN_VIDEO_INITIAL_WINDOW: u64 = 192 * 1024;
const INPUT_SIZE: usize = 18;
const FRAME_HEADER: usize = 16;
const KEYFRAME_REQUEST: u8 = 0;
const CLOSE_NORMAL: VarInt = VarInt::from_u32(0);
const CLOSE_REJECTED: VarInt = VarInt::from_u32(1);
const FRAME_EXPIRED: VarInt = VarInt::from_u32(2);

fn transport(server: bool) -> Arc<TransportConfig> {
    let mut acknowledgements = AckFrequencyConfig::default();
    acknowledgements.max_ack_delay(Some(Duration::from_millis(2)));
    let mut config = TransportConfig::default();
    config
        .ack_frequency_config(Some(acknowledgements))
        .initial_rtt(Duration::from_millis(10))
        .max_concurrent_bidi_streams(u32::from(server).into())
        .max_concurrent_uni_streams(if server { 0 } else { MAX_VIDEO_STREAMS }.into())
        .stream_receive_window(((MAX_ACCESS_UNIT + FRAME_HEADER) as u32).into())
        .receive_window(((2 * MAX_ACCESS_UNIT) as u32).into())
        .send_window((2 * MAX_ACCESS_UNIT) as u64)
        .datagram_receive_buffer_size(Some(if server { 16 * 1024 } else { 1024 }))
        .datagram_send_buffer_size(if server { 0 } else { 4 * 1024 })
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
    if server {
        // On a direct LAN (especially Wi-Fi), BBR avoids halving cwnd on isolated packet
        // loss and a 192 KiB initial window lets an entire 4K frame transmit in one flight.
        let mut congestion = quinn::congestion::BbrConfig::default();
        congestion.initial_window(LAN_VIDEO_INITIAL_WINDOW);
        config.congestion_controller_factory(Arc::new(congestion));
    }
    Arc::new(config)
}

fn endpoint(
    server: Option<quinn::ServerConfig>,
    address: SocketAddrV4,
) -> std::io::Result<Endpoint> {
    #[cfg(windows)]
    let runtime = windows_runtime::WindowsRuntime;
    #[cfg(not(windows))]
    let runtime = quinn::TokioRuntime;
    Endpoint::new(
        quinn::EndpointConfig::default(),
        server,
        std::net::UdpSocket::bind(address)?,
        Arc::new(runtime),
    )
}

pub struct SessionHost {
    endpoint: Endpoint,
    pairing: Pairing,
    format: Format,
    view_only: bool,
}

impl SessionHost {
    pub fn bind(address: SocketAddrV4, format: Format, view_only: bool) -> Result<Self> {
        ensure!(
            network::is_local(*address.ip()),
            "Use a local IPv4 address."
        );
        format.validate()?;
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

    pub async fn accept(&self) -> Result<HostSession> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .context("QUIC listener closed.")?;
        if !network::is_local_peer(incoming.remote_address()) {
            incoming.refuse();
            bail!("The QUIC peer must use a local IPv4 address.");
        }
        let (connection, control) = timeout(CONNECT_TIMEOUT, async {
            let connection = incoming.await?;
            let setup = async {
                let (mut reply, mut control) = connection.accept_bi().await?;
                let mut secret = [0; 24];
                control.read_exact(&mut secret).await?;
                ensure!(
                    bool::from(secret.ct_eq(self.pairing.secret())),
                    "Invalid pairing code."
                );
                reply.set_priority(10)?;
                reply
                    .write_all(&format_packet(self.format, !self.view_only))
                    .await?;
                reply.finish()?;
                Ok::<_, anyhow::Error>(control)
            };
            match setup.await {
                Ok(control) => Ok((connection, control)),
                Err(error) => {
                    connection.close(CLOSE_REJECTED, b"pairing rejected");
                    Err(error)
                }
            }
        })
        .await??;
        let (incoming, input) = mpsc::channel(96);
        let (requests, feedback) = mpsc::channel(1);
        let worker = tokio::spawn(read_input(
            connection.clone(),
            control,
            self.view_only,
            incoming,
            requests.clone(),
        ));
        let guard = Arc::new(HostConnectionGuard {
            connection: connection.clone(),
        });
        Ok(HostSession {
            video: HostVideoSender {
                _guard: guard.clone(),
                connection,
                requests,
                previous: None,
                format: self.format,
            },
            inputs: HostInputReceiver {
                _guard: guard,
                worker,
                input,
                feedback,
            },
        })
    }
}

fn format_packet(format: Format, can_control: bool) -> [u8; 8] {
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
    ]
}

fn parse_format(bytes: [u8; 8]) -> Result<(Format, bool)> {
    let [codec, control, w1, w2, h1, h2, f1, f2] = bytes;
    let codec = match codec {
        1 => Codec::H264,
        2 => Codec::Hevc,
        _ => bail!("Unsupported video codec."),
    };
    ensure!(control <= 1, "Invalid session options.");
    let format = Format {
        codec,
        width: u16::from_be_bytes([w1, w2]),
        height: u16::from_be_bytes([h1, h2]),
        frames_per_second: u16::from_be_bytes([f1, f2]),
    };
    format.validate()?;
    Ok((format, control == 1))
}

#[derive(Debug)]
pub enum HostInput {
    Remote(Input),
    Keyframe,
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
    connection: Connection,
    requests: mpsc::Sender<()>,
    previous: Option<u64>,
    format: Format,
}

impl HostVideoSender {
    pub fn peer_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    pub fn rtt(&self) -> Duration {
        self.connection.rtt()
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
        let Ok(opened) = timeout(VIDEO_WRITE_TIMEOUT, self.connection.open_uni()).await else {
            let _ = self.requests.try_send(());
            return Ok(false);
        };
        let mut stream = opened?;
        let timestamp = unit.rtp_timestamp().unwrap_or_else(|| {
            unit.sequence.saturating_mul(90_000) / u64::from(self.format.frames_per_second)
        });
        let mut header = [0; FRAME_HEADER];
        header[..8].copy_from_slice(&unit.sequence.to_be_bytes());
        header[8..].copy_from_slice(&timestamp.to_be_bytes());
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
    input: mpsc::Receiver<Input>,
    feedback: mpsc::Receiver<()>,
}

impl HostInputReceiver {
    pub async fn next_input(&mut self) -> Result<Option<HostInput>> {
        tokio::select! {
            biased;
            Some(()) = self.feedback.recv() => Ok(Some(HostInput::Keyframe)),
            event = self.input.recv() => Ok(match event {
                Some(event) => Some(HostInput::Remote(event)),
                None => {
                    (&mut self.worker).await??;
                    None
                }
            }),
        }
    }
}

impl Drop for HostInputReceiver {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

pub struct HostSession {
    video: HostVideoSender,
    inputs: HostInputReceiver,
}

impl HostSession {
    pub fn split(self) -> (HostVideoSender, HostInputReceiver) {
        (self.video, self.inputs)
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

pub struct ViewerSession {
    _endpoint: Endpoint,
    connection: Connection,
    control: Arc<Mutex<SendStream>>,
    frames: mpsc::Receiver<ReceivedFrame>,
    worker: Option<JoinHandle<Result<()>>>,
    can_control: bool,
    input_epoch: u64,
    movement: u64,
}

impl ViewerSession {
    pub async fn connect(address: SocketAddrV4, pairing: Pairing) -> Result<Self> {
        let local = network::local_route(address)?;
        let mut endpoint = endpoint(None, SocketAddrV4::new(local, 0))?;
        let provider = Arc::new(default_provider());
        let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(pairing.verifier(provider))
            .with_no_client_auth();
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
        config.transport_config(transport(false));
        endpoint.set_default_client_config(config);
        let (connection, control, format, can_control) = timeout(CONNECT_TIMEOUT, async {
            let connection = endpoint
                .connect(address.into(), "lan-remote.local")?
                .await?;
            let (mut control, mut reply) = connection.open_bi().await?;
            control.set_priority(10)?;
            control.write_all(pairing.secret()).await?;
            let mut metadata = [0; 8];
            reply
                .read_exact(&mut metadata)
                .await
                .context("QUIC pairing was not accepted.")?;
            let (format, can_control) = parse_format(metadata)?;
            Ok::<_, anyhow::Error>((connection, control, format, can_control))
        })
        .await??;
        let (output, frames) = mpsc::channel(3);
        let control = Arc::new(Mutex::new(control));
        let worker = tokio::spawn(read_video(
            connection.clone(),
            format.codec,
            output,
            control.clone(),
        ));
        Ok(Self {
            _endpoint: endpoint,
            connection,
            control,
            frames,
            worker: Some(worker),
            can_control,
            input_epoch: 0,
            movement: 0,
        })
    }

    pub fn can_control(&self) -> bool {
        self.can_control
    }

    pub fn rtt(&self) -> Duration {
        self.connection.rtt()
    }

    pub async fn input(&mut self, input: Input) -> Result<()> {
        if !self.can_control {
            return Ok(());
        }
        let input_body = input.encode();
        if matches!(input, Input::Move(_)) {
            self.movement += 1;
            let mut packet = Vec::with_capacity(16 + input_body.len());
            packet.extend_from_slice(&self.input_epoch.to_be_bytes());
            packet.extend_from_slice(&self.movement.to_be_bytes());
            packet.extend_from_slice(&input_body);
            self.connection.send_datagram(packet.into())?;
        } else {
            self.input_epoch += 1;
            let mut packet = [0; INPUT_SIZE];
            packet[0] = input_body.len() as u8;
            packet[1..9].copy_from_slice(&self.input_epoch.to_be_bytes());
            packet[9..9 + input_body.len()].copy_from_slice(&input_body);
            write_control(&self.control, &packet).await?;
        }
        Ok(())
    }

    pub async fn disconnect(self) {
        self.connection.close(CLOSE_NORMAL, b"viewer disconnected");
        let _ = timeout(Duration::from_millis(25), self._endpoint.wait_idle()).await;
    }

    pub async fn recv(&mut self) -> Result<Option<ReceivedFrame>> {
        let frame = self.frames.recv().await;
        if frame.is_none()
            && let Some(worker) = self.worker.take()
        {
            worker.await??;
        }
        Ok(frame)
    }
}

impl Drop for ViewerSession {
    fn drop(&mut self) {
        self.connection.close(CLOSE_NORMAL, b"viewer disconnected");
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

async fn write_control(control: &Mutex<SendStream>, packet: &[u8; INPUT_SIZE]) -> Result<()> {
    timeout(CONTROL_TIMEOUT, async {
        control.lock().await.write_all(packet).await
    })
    .await??;
    Ok(())
}

async fn read_input(
    connection: Connection,
    mut control: RecvStream,
    view_only: bool,
    input: mpsc::Sender<Input>,
    feedback: mpsc::Sender<()>,
) -> Result<()> {
    let mut packet = [0; INPUT_SIZE];
    let mut filled = 0;
    let mut epoch = 0u64;
    let mut latest_movement = 0;
    let mut early = None;
    loop {
        tokio::select! {
            biased;
            read = control.read(&mut packet[filled..]) => {
                let count = match read {
                    Ok(Some(count)) => count,
                    Ok(None) => return Ok(()),
                    Err(error) if normal_read_close(&error) => return Ok(()),
                    Err(error) => return Err(error.into()),
                };
                filled += count;
                if filled != INPUT_SIZE { continue; }
                filled = 0;
                let length = usize::from(packet[0]);
                if length == usize::from(KEYFRAME_REQUEST) {
                    let _ = feedback.try_send(());
                    continue;
                }
                ensure!(length <= INPUT_SIZE - 9, "Invalid QUIC input length.");
                let parsed = Input::parse(&packet[9..9 + length])?;
                let next = u64::from_be_bytes(packet[1..9].try_into()?);
                ensure!(!matches!(parsed, Input::Move(_)) && epoch.checked_add(1) == Some(next), "Invalid ordered input.");
                epoch = next;
                if !view_only { input.send(parsed).await?; }
                if let Some((event_epoch, index, event)) = early {
                    if event_epoch <= epoch { early = None; }
                    if event_epoch == epoch && index > latest_movement && !view_only {
                        let _ = input.try_send(event);
                        latest_movement = index;
                    }
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
                    let _ = input.try_send(event);
                    latest_movement = index;
                } else if early.is_none_or(|(_, previous, _)| previous < index) {
                    // A datagram can overtake its button press or release.
                    early = Some((event_epoch, index, event));
                }
            }
        }
    }
}

fn normal_read_close(error: &quinn::ReadError) -> bool {
    matches!(error, quinn::ReadError::ConnectionLost(error) if normal_close(error))
}

fn normal_close(error: &quinn::ConnectionError) -> bool {
    matches!(error, quinn::ConnectionError::ApplicationClosed(close) if close.error_code == CLOSE_NORMAL)
}

struct Picture {
    sequence: u64,
    timestamp: u64,
    bytes: Vec<u8>,
}

async fn picture(mut stream: RecvStream) -> Result<Option<Picture>> {
    let bytes = match timeout(
        FRAME_TIMEOUT,
        stream.read_to_end(MAX_ACCESS_UNIT + FRAME_HEADER),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(_)))) => return Ok(None),
        Ok(Err(quinn::ReadToEndError::Read(error))) if normal_read_close(&error) => {
            return Ok(None);
        }
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => {
            let _ = stream.stop(FRAME_EXPIRED);
            return Ok(None);
        }
    };
    ensure!(bytes.len() > FRAME_HEADER, "Truncated QUIC video frame.");
    let sequence = u64::from_be_bytes(bytes[..8].try_into()?);
    let timestamp = u64::from_be_bytes(bytes[8..16].try_into()?);
    let bytes = bytes[FRAME_HEADER..].to_vec();
    Ok(Some(Picture {
        sequence,
        timestamp,
        bytes,
    }))
}

async fn read_video(
    connection: Connection,
    codec: Codec,
    output: mpsc::Sender<ReceivedFrame>,
    feedback: Arc<Mutex<SendStream>>,
) -> Result<()> {
    let mut readers = JoinSet::new();
    let mut pending = BTreeMap::<u64, Picture>::new();
    let mut previous = None::<u64>;
    let mut synchronized = false;
    let mut missing_since = None::<Instant>;
    let mut last_request = None::<Instant>;
    loop {
        let wait = async {
            if let Some(since) = missing_since {
                sleep_until(since + REORDER_TIMEOUT).await;
            } else {
                std::future::pending().await
            }
        };
        tokio::select! {
            stream = connection.accept_uni(), if readers.len() + pending.len() < MAX_VIDEO_STREAMS as usize => match stream {
                Ok(stream) => { readers.spawn(picture(stream)); }
                Err(error) if normal_close(&error) => return Ok(()),
                Err(error) => return Err(error.into()),
            },
            Some(result) = readers.join_next() => {
                match result?? {
                    Some(picture) if previous.is_none_or(|last| picture.sequence > last) => {
                        pending.entry(picture.sequence).or_insert(picture);
                    }
                    Some(_) => {}
                    None if connection.close_reason().is_some_and(|error| normal_close(&error)) => {
                        return Ok(());
                    }
                    None => request_keyframe(&feedback, &mut last_request).await?,
                }
            }
            _ = wait => {},
        }
        while let Some((&sequence, first)) = pending.first_key_value() {
            let starts_chain = inspect(codec, &first.bytes)?;
            let contiguous = previous.and_then(|last| last.checked_add(1)) == Some(sequence);
            if !contiguous
                && !starts_chain
                && readers.len() + pending.len() < MAX_VIDEO_STREAMS as usize
            {
                let since = missing_since.get_or_insert_with(Instant::now);
                if since.elapsed() < REORDER_TIMEOUT {
                    break;
                }
            }
            let picture = pending.pop_first().unwrap().1;
            missing_since = None;
            previous = Some(sequence);
            if ((synchronized && contiguous) || starts_chain)
                && output
                    .try_send(ReceivedFrame {
                        codec,
                        rtp_timestamp: picture.timestamp,
                        bytes: picture.bytes.into(),
                    })
                    .is_ok()
            {
                synchronized = true;
                continue;
            }
            // Do not decode any inter-frame reference until a fresh keyframe arrives.
            synchronized = false;
            request_keyframe(&feedback, &mut last_request).await?;
        }
    }
}

async fn request_keyframe(
    feedback: &Mutex<SendStream>,
    last_request: &mut Option<Instant>,
) -> Result<()> {
    if last_request.is_none_or(|sent| sent.elapsed() >= Duration::from_millis(120)) {
        write_control(feedback, &[KEYFRAME_REQUEST; INPUT_SIZE]).await?;
        *last_request = Some(Instant::now());
    }
    Ok(())
}
