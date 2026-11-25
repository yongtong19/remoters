//! Establishes direct remote desktop sessions and exposes their media and input.
use std::{
    net::{SocketAddr, SocketAddrV4},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::{sync::mpsc, task::JoinHandle, time::timeout};

use crate::{
    Input as DesktopInput,
    network::{
        self, Pairing,
        signaling::{self, SignalingHost},
    },
    video::{AccessUnit, Format, ReceivedFrame},
    webrtc::{Command, Peer},
};

const SEND_TIMEOUT: Duration = Duration::from_millis(100);

pub struct SessionHost {
    signal: SignalingHost,
    format: Format,
    view_only: bool,
}

impl SessionHost {
    pub fn bind(address: SocketAddrV4, format: Format, view_only: bool) -> Result<Self> {
        format.validate()?;
        Ok(Self {
            signal: SignalingHost::bind(address)?,
            format,
            view_only,
        })
    }

    pub fn address(&self) -> Result<SocketAddrV4> {
        self.signal.address()
    }

    pub fn pairing_code(&self) -> String {
        self.signal.pairing_code()
    }

    pub async fn accept(&self) -> Result<HostSession> {
        let mut peer = Peer::bind(*self.address()?.ip(), Some(self.format.codec)).await?;
        let address = self.signal.accept(|sdp| peer.accept_offer(&sdp)).await?;
        let (requests, feedback) = mpsc::channel(1);
        let (input, incoming) = mpsc::channel(96);
        let connection = peer
            .publish(self.format, requests.clone(), input, !self.view_only)
            .await?;
        Ok(HostSession {
            control: connection.control,
            worker: connection.worker,
            feedback,
            previous: None,
            requests,
            address,
            incoming,
        })
    }
}

pub enum HostInput {
    Remote(DesktopInput),
    Keyframe,
}

pub struct HostSession {
    control: mpsc::Sender<Command>,
    worker: JoinHandle<Result<()>>,
    feedback: mpsc::Receiver<()>,
    previous: Option<u64>,
    requests: mpsc::Sender<()>,
    address: SocketAddr,
    incoming: mpsc::Receiver<DesktopInput>,
}

impl HostSession {
    pub fn peer_address(&self) -> SocketAddr {
        self.address
    }

    /// The host is the authority for whether a session admits input.
    pub async fn input(&mut self) -> Result<Option<DesktopInput>> {
        if let Some(input) = self.incoming.recv().await {
            return Ok(Some(input));
        }
        (&mut self.worker).await??;
        Ok(None)
    }

    pub async fn next_input(&mut self) -> Result<Option<HostInput>> {
        tokio::select! {
            event = self.incoming.recv() => {
                if let Some(event) = event {
                    Ok(Some(HostInput::Remote(event)))
                } else {
                    (&mut self.worker).await??;
                    Ok(None)
                }
            }
            Some(()) = self.feedback.recv() => Ok(Some(HostInput::Keyframe)),
        }
    }

    /// Returns false when the queue is full or a fresh keyframe is needed.
    pub async fn send(&mut self, unit: &AccessUnit) -> Result<bool> {
        ensure!(
            self.previous
                .is_none_or(|previous| unit.sequence > previous),
            "Video sequence numbers must increase."
        );
        if self.previous.and_then(|previous| previous.checked_add(1)) != Some(unit.sequence)
            && !unit.is_random_access()
        {
            let _ = self.requests.try_send(());
            return Ok(false);
        }
        match timeout(
            SEND_TIMEOUT,
            self.control.send(Command::Frame(unit.clone())),
        )
        .await
        {
            Ok(result) => {
                result.context("The WebRTC sender stopped.")?;
                self.previous = Some(unit.sequence);
                Ok(true)
            }
            Err(_) => {
                let _ = self.requests.try_send(());
                Ok(false)
            }
        }
    }

    /// Waits for RTCP loss feedback or a request after a locally dropped frame.
    pub async fn keyframe_requested(&mut self) -> Result<()> {
        self.feedback
            .recv()
            .await
            .context("The WebRTC sender stopped.")
    }

    /// Gives WebRTC a short window to flush packets and handle feedback.
    pub async fn finish(self) -> Result<()> {
        self.control
            .send(Command::Finish)
            .await
            .context("The WebRTC sender stopped.")?;
        self.worker.await?
    }
}

pub struct ViewerSession {
    control: mpsc::Sender<Command>,
    can_control: bool,
    worker: Option<JoinHandle<Result<()>>>,
    frames: mpsc::Receiver<ReceivedFrame>,
}

impl ViewerSession {
    pub async fn connect(address: SocketAddrV4, pairing: Pairing) -> Result<Self> {
        let local = network::local_route(address)?;
        let mut peer = Peer::bind(local, None).await?;
        let (offer, pending) = peer.video_offer()?;
        let answer = signaling::offer(address, &pairing, offer).await?;
        peer.accept_answer(pending, &answer)?;
        let (out, frames) = mpsc::channel(8);
        let connection = peer.subscribe(out).await?;
        Ok(Self {
            control: connection.control,
            can_control: connection.can_control,
            worker: Some(connection.worker),
            frames,
        })
    }

    pub fn can_control(&self) -> bool {
        self.can_control
    }

    pub async fn input(&self, input: DesktopInput) -> Result<()> {
        if self.can_control {
            timeout(SEND_TIMEOUT, self.control.send(Command::Input(input)))
                .await?
                .context("The WebRTC session stopped.")?;
        }
        Ok(())
    }

    pub async fn disconnect(self) {
        let _ = timeout(SEND_TIMEOUT, self.control.send(Command::Finish)).await;
        drop(self.control);
        if let Some(worker) = self.worker {
            let _ = worker.await;
        }
    }

    pub async fn recv(&mut self) -> Result<Option<ReceivedFrame>> {
        if let Some(frame) = self.frames.recv().await {
            return Ok(Some(frame));
        }
        if let Some(worker) = self.worker.take() {
            worker.await??;
        }
        Ok(None)
    }
}
