//! Drives str0m's ICE, DTLS, RTP, RTCP, and data channel state.
mod video;

use std::{
    net::Ipv4Addr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use str0m::{
    Candidate, Event, IceConnectionState, Input, Output, Rtc,
    change::{SdpAnswer, SdpOffer, SdpPendingOffer},
    channel::ChannelId,
    format::Codec as RtcCodec,
    media::{Direction, MediaKind, MediaTime, Mid},
    net::{Protocol, Receive},
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{sleep_until, timeout},
};

use crate::{
    Input as DesktopInput,
    network::is_local_peer,
    protocol,
    video::{AccessUnit, Codec, Format, ReceivedFrame},
};
use video::VideoOutput;

const CONTROL: &str = "remoters-control";
const RTP_CLOCK: u64 = 90_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(7);
const DRAIN_TIME: Duration = Duration::from_millis(400);

pub(crate) struct Peer {
    rtc: Rtc,
    socket: UdpSocket,
}

impl Peer {
    pub(crate) async fn bind(ip: Ipv4Addr, codec: Option<Codec>) -> Result<Self> {
        let socket = UdpSocket::bind((ip, 0)).await?;
        let mut config = Rtc::builder()
            .clear_codecs()
            .enable_h265(codec != Some(Codec::H264))
            .set_crypto_provider(Arc::new(str0m::crypto::from_feature_flags()));
        if codec != Some(Codec::Hevc) {
            // Constrained Baseline, non-interleaved packetization, up to 4K60.
            config
                .codec_config()
                .add_h264(108.into(), Some(109.into()), true, 0x42e034);
        }
        let mut rtc = config.build(Instant::now());
        rtc.add_local_candidate(Candidate::host(socket.local_addr()?, "udp")?)
            .context("Could not add the local WebRTC candidate.")?;
        Ok(Self { rtc, socket })
    }

    pub(crate) fn accept_offer(&mut self, offer: &str) -> Result<String> {
        let offer = SdpOffer::from_sdp_string(offer)?;
        Ok(self.rtc.sdp_api().accept_offer(offer)?.to_sdp_string())
    }

    pub(crate) fn video_offer(&mut self) -> Result<(String, SdpPendingOffer)> {
        let mut change = self.rtc.sdp_api();
        change.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
        change.add_channel(CONTROL.into());
        let (offer, pending) = change
            .apply()
            .context("Could not create a session offer.")?;
        Ok((offer.to_sdp_string(), pending))
    }

    pub(crate) fn accept_answer(&mut self, pending: SdpPendingOffer, answer: &str) -> Result<()> {
        self.rtc
            .sdp_api()
            .accept_answer(pending, SdpAnswer::from_sdp_string(answer)?)?;
        Ok(())
    }

    pub(crate) async fn publish(
        self,
        format: Format,
        requests: mpsc::Sender<()>,
        input: mpsc::Sender<DesktopInput>,
        can_control: bool,
    ) -> Result<Connection> {
        let role = Role::Publish {
            format,
            mid: None,
            requests,
            input,
            can_control,
        };
        start(self.rtc, self.socket, role).await
    }

    pub(crate) async fn subscribe(self, frames: mpsc::Sender<ReceivedFrame>) -> Result<Connection> {
        start(
            self.rtc,
            self.socket,
            Role::Subscribe(VideoOutput::new(frames)),
        )
        .await
    }
}

pub(crate) struct Connection {
    pub(crate) control: mpsc::Sender<Command>,
    pub(crate) worker: JoinHandle<Result<()>>,
    pub(crate) can_control: bool,
}

pub(crate) enum Command {
    Frame(AccessUnit),
    Input(DesktopInput),
    Finish,
}

enum Role {
    Publish {
        format: Format,
        mid: Option<Mid>,
        requests: mpsc::Sender<()>,
        input: mpsc::Sender<DesktopInput>,
        can_control: bool,
    },
    Subscribe(VideoOutput),
}

async fn start(rtc: Rtc, socket: UdpSocket, role: Role) -> Result<Connection> {
    let (control, commands) = mpsc::channel(2);
    let (connected, ready) = oneshot::channel();
    let worker = tokio::spawn(run(rtc, socket, role, commands, connected));
    match timeout(CONNECT_TIMEOUT, ready).await {
        Ok(Ok(can_control)) => Ok(Connection {
            control,
            worker,
            can_control,
        }),
        Ok(Err(_)) => {
            worker.await??;
            bail!("WebRTC closed before connecting.");
        }
        Err(error) => {
            worker.abort();
            Err(error.into())
        }
    }
}

async fn run(
    mut rtc: Rtc,
    socket: UdpSocket,
    mut role: Role,
    mut commands: mpsc::Receiver<Command>,
    connected: oneshot::Sender<bool>,
) -> Result<()> {
    let local = socket.local_addr()?;
    let mut ready = Some(connected);
    let mut control = None;
    let mut closing = false;
    let mut drain = None::<Instant>;
    let mut packet = [0; 2048];
    loop {
        let deadline = loop {
            match rtc.poll_output()? {
                Output::Transmit(tx) => {
                    if is_local_peer(tx.destination) {
                        socket.send_to(tx.contents.as_ref(), tx.destination).await?;
                    }
                }
                Output::Event(Event::IceConnectionStateChange(
                    IceConnectionState::Disconnected,
                )) => {
                    return Ok(());
                }
                Output::Event(Event::Closed) => closing = true,
                Output::Event(Event::ChannelOpen(id, label))
                    if label == CONTROL && control.is_none() =>
                {
                    control = Some(id);
                    if let Role::Publish { can_control, .. } = role {
                        send_control(&mut rtc, id, &[protocol::READY, u8::from(can_control)])?;
                        if let Some(ready) = ready.take() {
                            let _ = ready.send(can_control);
                        }
                    }
                }
                Output::Event(Event::ChannelData(message))
                    if message.binary && Some(message.id) == control =>
                {
                    match (&role, message.data.as_slice()) {
                        (Role::Subscribe(_), [protocol::READY, allowed @ 0..=1]) => {
                            if let Some(ready) = ready.take() {
                                let _ = ready.send(*allowed == 1);
                            }
                        }
                        (Role::Publish { .. }, [protocol::DISCONNECT]) => {
                            rtc.close()?;
                            closing = true;
                        }
                        (
                            Role::Publish {
                                can_control: true,
                                input,
                                ..
                            },
                            bytes,
                        ) => {
                            let event = DesktopInput::parse(bytes)?;
                            if let Err(error) = input.try_send(event) {
                                ensure!(
                                    matches!(
                                        error,
                                        mpsc::error::TrySendError::Full(DesktopInput::Move(_))
                                    ),
                                    "The host input queue stopped accepting control events."
                                );
                            }
                        }
                        _ => {}
                    }
                }
                Output::Event(event) => handle_event(&mut role, event)?,
                Output::Timeout(when) => break when,
            }
        };

        if closing {
            return Ok(());
        }
        let now = Instant::now();
        let deadline = match &mut role {
            Role::Subscribe(output) => output
                .flush_due(now)
                .map_or(deadline, |end| end.min(deadline)),
            Role::Publish { .. } => deadline,
        };
        if drain.is_some_and(|end| end <= now) {
            rtc.close()?;
            closing = true;
            continue;
        }
        if let Role::Subscribe(output) = &mut role
            && output.request_keyframe(&mut rtc, now)
        {
            continue;
        }
        let deadline = drain.map_or(deadline, |end| end.min(deadline));
        tokio::select! {
            result = socket.recv_from(&mut packet) => {
                let (len, remote) = result?;
                if is_local_peer(remote)
                    && let Ok(receive) = Receive::new(Protocol::Udp, remote, local, &packet[..len])
                {
                    rtc.handle_input(Input::Receive(Instant::now(), receive))?;
                }
            }
            command = commands.recv(), if drain.is_none() => {
                match command {
                    Some(Command::Frame(unit)) => write_frame(&mut rtc, &role, unit)?,
                    Some(Command::Input(event)) => {
                        send_control(
                            &mut rtc,
                            control.context("The control channel is not open.")?,
                            &event.encode(),
                        )?;
                    }
                    Some(Command::Finish) | None if matches!(role, Role::Subscribe(_)) => {
                        if let Some(id) = control {
                            send_control(&mut rtc, id, &[protocol::DISCONNECT])?;
                        }
                        drain = Some(Instant::now() + DRAIN_TIME);
                    }
                    Some(Command::Finish) => drain = Some(Instant::now() + DRAIN_TIME),
                    None => {
                        rtc.close()?;
                        closing = true;
                    }
                }
            }
            _ = sleep_until(deadline.into()) => {
                rtc.handle_input(Input::Timeout(Instant::now()))?;
            }
        }
    }
}

fn send_control(rtc: &mut Rtc, id: ChannelId, bytes: &[u8]) -> Result<()> {
    let mut channel = rtc
        .channel(id)
        .context("The WebRTC control channel is unavailable.")?;
    ensure!(
        channel.write(true, bytes)?,
        "The WebRTC control channel is full."
    );
    Ok(())
}

fn handle_event(role: &mut Role, event: Event) -> Result<()> {
    match (role, event) {
        (Role::Publish { mid, .. }, Event::MediaAdded(media))
            if media.kind == MediaKind::Video && media.direction == Direction::SendOnly =>
        {
            *mid = Some(media.mid);
        }
        (Role::Publish { requests, .. }, Event::KeyframeRequest(_)) => {
            let _ = requests.try_send(());
        }
        (Role::Subscribe(output), Event::MediaData(media)) => output.media(media)?,
        _ => {}
    }
    Ok(())
}

fn write_frame(rtc: &mut Rtc, role: &Role, unit: AccessUnit) -> Result<()> {
    let Role::Publish {
        format,
        mid: Some(mid),
        ..
    } = role
    else {
        bail!("No outgoing video track was negotiated.");
    };
    let writer = rtc
        .writer(*mid)
        .context("The WebRTC video track is unavailable.")?;
    let codec = match format.codec {
        Codec::H264 => RtcCodec::H264,
        Codec::Hevc => RtcCodec::H265,
    };
    let pt = writer
        .payload_params()
        .find(|params| params.spec().codec == codec)
        .context("The peer did not negotiate the selected video codec.")?
        .pt();
    let clock = match unit.rtp_timestamp() {
        Some(clock) => clock,
        None => {
            unit.sequence
                .checked_mul(RTP_CLOCK)
                .context("Video timestamp overflow.")?
                / u64::from(format.frames_per_second)
        }
    };
    writer.write(
        pt,
        Instant::now(),
        MediaTime::from_90khz(clock),
        unit.bytes().as_ref(),
    )?;
    Ok(())
}
