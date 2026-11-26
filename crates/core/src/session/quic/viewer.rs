use std::{collections::BTreeMap, net::SocketAddrV4, sync::Arc, time::Duration};

use anyhow::{Result, ensure};
use quinn::{Connection, Endpoint, RecvStream, SendStream, crypto::rustls::QuicClientConfig};
use rustls::crypto::ring::default_provider;
use tokio::{
    sync::{Mutex, mpsc},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep_until, timeout},
};

use super::{
    aux::{AuxSender, accept_aux_loop},
    aux_types::{AudioFrame, SessionAuxiliary, VIEWER_AUDIO_CHANNEL_CAPACITY},
    protocol::{
        BITRATE_REQUEST, DisplayPreference, FRAME_HEADER, HELLO_SIZE, INPUT_REQUEST, INPUT_SIZE,
        KEYFRAME_REQUEST, StreamPreference, encode_bitrate_mbps, parse_format,
    },
    transport::{
        ALPN, CLOSE_NORMAL, CONNECT_TIMEOUT, FRAME_EXPIRED, FRAME_TIMEOUT, MAX_VIDEO_STREAMS,
        REORDER_TIMEOUT, TransportStats, endpoint, normal_close, normal_read_close, transport,
        write_control,
    },
};
use crate::{
    Input,
    network::{self, Pairing},
    session::fec::{FecReassembler, PushOutcome},
    video::{Codec, Format, MAX_ACCESS_UNIT, ReceivedFrame, inspect},
};

pub struct ViewerSession {
    _endpoint: Endpoint,
    pub(super) connection: Connection,
    pub(super) control: Arc<Mutex<SendStream>>,
    frames: mpsc::Receiver<ReceivedFrame>,
    aux: Option<mpsc::Receiver<SessionAuxiliary>>,
    audio: Option<mpsc::Receiver<AudioFrame>>,
    worker: Option<JoinHandle<Result<()>>>,
    aux_worker: Option<JoinHandle<Result<()>>>,
    format: Format,
    can_control: bool,
    input_epoch: u64,
    movement: u64,
}

impl ViewerSession {
    pub async fn connect(address: SocketAddrV4, pairing: Pairing) -> Result<Self> {
        Self::connect_with_stream_preference(address, pairing, StreamPreference::default()).await
    }

    pub async fn connect_with_preference(
        address: SocketAddrV4,
        pairing: Pairing,
        preference: Option<DisplayPreference>,
    ) -> Result<Self> {
        Self::connect_with_stream_preference(
            address,
            pairing,
            StreamPreference {
                display: preference,
                ..StreamPreference::default()
            },
        )
        .await
    }

    pub async fn connect_with_stream_preference(
        address: SocketAddrV4,
        pairing: Pairing,
        preference: StreamPreference,
    ) -> Result<Self> {
        let encoded_preference = preference.encode()?;
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
            let mut hello = [0; HELLO_SIZE];
            hello[..24].copy_from_slice(pairing.secret());
            hello[24..].copy_from_slice(&encoded_preference);
            control.write_all(&hello).await?;
            let mut metadata = [0; 9];
            reply
                .read_exact(&mut metadata)
                .await
                .map_err(|error| match error {
                    quinn::ReadExactError::ReadError(quinn::ReadError::ConnectionLost(
                        quinn::ConnectionError::ApplicationClosed(ref close),
                    )) if !close.reason.is_empty() => {
                        anyhow::anyhow!(
                            "QUIC pairing was not accepted: {}",
                            String::from_utf8_lossy(&close.reason)
                        )
                    }
                    other => anyhow::Error::new(other).context("QUIC pairing was not accepted."),
                })?;
            let (format, can_control) = parse_format(metadata)?;
            Ok::<_, anyhow::Error>((connection, control, format, can_control))
        })
        .await??;
        let (output, frames) = mpsc::channel(3);
        let (aux_tx, aux_rx) = mpsc::channel(64);
        let (audio_tx, audio_rx) = mpsc::channel(VIEWER_AUDIO_CHANNEL_CAPACITY);
        let control = Arc::new(Mutex::new(control));
        let worker = tokio::spawn(read_video(
            connection.clone(),
            format.codec,
            output,
            control.clone(),
        ));
        let aux_worker = tokio::spawn(accept_aux_loop(
            connection.clone(),
            true,
            true,
            None,
            aux_tx,
            Some(audio_tx),
        ));
        Ok(Self {
            _endpoint: endpoint,
            connection,
            control,
            frames,
            aux: Some(aux_rx),
            audio: Some(audio_rx),
            worker: Some(worker),
            aux_worker: Some(aux_worker),
            format,
            can_control,
            input_epoch: 0,
            movement: 0,
        })
    }

    pub fn aux_sender(&self) -> AuxSender {
        AuxSender {
            connection: self.connection.clone(),
            cursor: None,
            audio: None,
        }
    }

    pub fn take_aux_receiver(&mut self) -> Option<mpsc::Receiver<SessionAuxiliary>> {
        self.aux.take()
    }

    pub fn take_audio_receiver(&mut self) -> Option<mpsc::Receiver<AudioFrame>> {
        self.audio.take()
    }

    pub async fn recv_aux(&mut self) -> Option<SessionAuxiliary> {
        let rx = self.aux.as_mut()?;
        rx.recv().await
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn can_control(&self) -> bool {
        self.can_control
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
            let mut packet = [0; 1 + INPUT_SIZE];
            packet[0] = INPUT_REQUEST;
            packet[1] = input_body.len() as u8;
            packet[2..10].copy_from_slice(&self.input_epoch.to_be_bytes());
            packet[10..10 + input_body.len()].copy_from_slice(&input_body);
            write_control(&self.control, &packet).await?;
        }
        Ok(())
    }

    pub async fn set_bitrate_mbps(&self, mbps: Option<u16>) -> Result<()> {
        let [hi, lo] = encode_bitrate_mbps(mbps)?.to_be_bytes();
        write_control(&self.control, &[BITRATE_REQUEST, hi, lo]).await
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
        if let Some(aux_worker) = &self.aux_worker {
            aux_worker.abort();
        }
    }
}

struct Picture {
    sequence: u64,
    timestamp: u64,
    host_latency_us: u32,
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
    let host_latency_us = u32::from_be_bytes(bytes[16..20].try_into()?);
    let bytes = bytes[FRAME_HEADER..].to_vec();
    Ok(Some(Picture {
        sequence,
        timestamp,
        host_latency_us,
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
    let mut fec = FecReassembler::default();
    let mut pending = BTreeMap::<u64, Picture>::new();
    let mut previous = None::<u64>;
    let mut synchronized = false;
    let mut missing_since = None::<Instant>;
    let mut last_request = None::<Instant>;
    loop {
        let next_deadline = match (
            missing_since.map(|since| since + REORDER_TIMEOUT),
            fec.next_expiry().map(Instant::from_std),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        let wait = async {
            if let Some(deadline) = next_deadline {
                sleep_until(deadline).await;
            } else {
                std::future::pending().await
            }
        };
        tokio::select! {
            biased;
            datagram = connection.read_datagram() => match datagram {
                Ok(bytes) => {
                    let (outcome, evicted) =
                        fec.push_datagram(&bytes, std::time::Instant::now(), previous)?;
                    if let PushOutcome::Completed(frame) = outcome
                        && previous.is_none_or(|last| frame.sequence > last)
                    {
                        pending.entry(frame.sequence).or_insert(Picture {
                            sequence: frame.sequence,
                            timestamp: frame.timestamp,
                            host_latency_us: frame.host_latency_us,
                            bytes: frame.bytes,
                        });
                    }
                    if evicted {
                        synchronized = false;
                        request_keyframe(&feedback, &mut last_request).await?;
                    }
                }
                Err(error) if normal_close(&error) => return Ok(()),
                Err(error) => return Err(error.into()),
            },
            stream = connection.accept_uni(), if readers.len() + pending.len() < MAX_VIDEO_STREAMS as usize => match stream {
                Ok(stream) => { readers.spawn(picture(stream)); }
                Err(error) if normal_close(&error) => return Ok(()),
                Err(error) => return Err(error.into()),
            },
            Some(result) = readers.join_next() => {
                match result?? {
                    Some(picture) if previous.is_none_or(|last| picture.sequence > last) => {
                        fec.mark_completed(picture.sequence);
                        pending.entry(picture.sequence).or_insert(picture);
                    }
                    Some(picture) => {
                        fec.mark_completed(picture.sequence);
                    }
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
            fec.advance_delivered(sequence);
            if ((synchronized && contiguous) || starts_chain)
                && output
                    .try_send(ReceivedFrame {
                        codec,
                        rtp_timestamp: picture.timestamp,
                        host_latency_us: picture.host_latency_us,
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
        if fec.expire(std::time::Instant::now()) > 0 {
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
        write_control(feedback, &[KEYFRAME_REQUEST]).await?;
        *last_request = Some(Instant::now());
    }
    Ok(())
}
