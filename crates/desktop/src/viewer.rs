use std::{
    net::SocketAddrV4,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use remoters_core::{Input, network::Pairing, session::ViewerSession, video::ReceivedFrame};
use tokio::{
    runtime::Builder,
    sync::{Notify, mpsc, oneshot},
};

use crate::{codec::Decoder, metrics::ViewerStats, platform::VideoFrame};

pub enum Event {
    Ready(bool),
    Closed(Result<()>),
}

pub struct Connection {
    sink: mpsc::Sender<Input>,
    pub events: Receiver<Event>,
    latest: Arc<Mutex<Option<(VideoFrame, Instant)>>>,
    stats: Arc<Mutex<ViewerStats>>,
    rtt_us: Arc<AtomicU64>,
    _stop: oneshot::Sender<()>,
}

impl Connection {
    pub fn start(
        address: SocketAddrV4,
        pairing: Pairing,
        wake: Arc<Notify>,
        _window: &gpui::Window,
    ) -> Result<Self> {
        #[cfg(target_os = "windows")]
        let device = _window
            .d3d11_device()
            .ok_or_else(|| anyhow::anyhow!("The window has no Direct3D video device."))?;
        let (sink, mut commands) = mpsc::channel(96);
        let (events, receiver) = channel();
        let latest = Arc::new(Mutex::new(None));
        let slot = latest.clone();
        let stats = Arc::new(Mutex::new(ViewerStats::new()));
        let worker_stats = stats.clone();
        let rtt_us = Arc::new(AtomicU64::new(0));
        let worker_rtt = rtt_us.clone();
        let (stop, mut stopped) = oneshot::channel();
        thread::Builder::new()
            .name("lan-viewer".into())
            .spawn(move || {
                let result = (|| {
                    Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(async {
                            let mut session = tokio::select! {
                                _ = &mut stopped => return Ok(()),
                                session = ViewerSession::connect(address, pairing) => session?,
                            };
                            let _ = events.send(Event::Ready(session.can_control()));
                            wake.notify_one();
                            let result = tokio::select! {
                                _ = &mut stopped => Ok(()),
                                result = run(
                                    &mut session,
                                    &mut commands,
                                    slot,
                                    wake.clone(),
                                    worker_stats,
                                    worker_rtt,
                                    #[cfg(target_os = "windows")]
                                    device,
                                ) => result,
                            };
                            session.disconnect().await;
                            result
                        })
                })();
                let _ = events.send(Event::Closed(result));
                wake.notify_one();
            })?;
        Ok(Self {
            sink,
            events: receiver,
            latest,
            stats,
            rtt_us,
            _stop: stop,
        })
    }

    pub fn take_frame(&self) -> Option<VideoFrame> {
        let (frame, decoded_at) = self.latest.lock().unwrap().take()?;
        let rtt = Duration::from_micros(self.rtt_us.load(Ordering::Relaxed));
        self.stats
            .lock()
            .unwrap()
            .record_present(decoded_at.elapsed(), rtt);
        Some(frame)
    }

    #[must_use]
    pub fn input(&self, event: Input) -> bool {
        match self.sink.try_send(event) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => matches!(event, Input::Move(_)),
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

async fn run(
    session: &mut ViewerSession,
    commands: &mut mpsc::Receiver<Input>,
    slot: Arc<Mutex<Option<(VideoFrame, Instant)>>>,
    wake: Arc<Notify>,
    stats: Arc<Mutex<ViewerStats>>,
    rtt_us: Arc<AtomicU64>,
    #[cfg(target_os = "windows")] device: gpui::ID3D11Device,
) -> Result<()> {
    let (decode_tx, mut decode_rx) = mpsc::channel::<ReceivedFrame>(4);
    let decode_slot = slot;
    let decode_wake = wake;
    let decode_stats = stats;
    let decoder_thread = thread::Builder::new()
        .name("video-decoder".into())
        .spawn(move || -> Result<()> {
            #[cfg(target_os = "macos")]
            let mut decoder = Decoder::default();
            #[cfg(target_os = "windows")]
            let mut decoder = Decoder::new(device);
            while let Some(frame) = decode_rx.blocking_recv() {
                let started = Instant::now();
                let decoded = decoder.decode(&frame)?;
                let elapsed = started.elapsed();
                if let Some(frame) = decoded {
                    *decode_slot.lock().unwrap() = Some((frame, Instant::now()));
                    decode_wake.notify_one();
                    decode_stats.lock().unwrap().record_decode(elapsed, true);
                } else {
                    decode_stats.lock().unwrap().record_decode(elapsed, false);
                }
            }
            Ok(())
        })?;

    let mut pending_frame: Option<ReceivedFrame> = None;
    let loop_result: Result<()> = loop {
        tokio::select! {
            biased;
            input = commands.recv() => {
                match input {
                    Some(input) => {
                        if let Err(error) = session.input(input).await {
                            break Err(error);
                        }
                    }
                    None => break Ok(()),
                }
            }
            permit = decode_tx.reserve(), if pending_frame.is_some() => {
                match permit {
                    Ok(permit) => permit.send(pending_frame.take().unwrap()),
                    Err(_) => break Ok(()),
                }
            }
            frame = session.recv(), if pending_frame.is_none() => {
                match frame {
                    Ok(Some(frame)) => {
                        rtt_us.store(session.rtt().as_micros() as u64, Ordering::Relaxed);
                        pending_frame = Some(frame);
                    }
                    Ok(None) => break Ok(()),
                    Err(error) => break Err(error),
                }
            }
        }
    };
    drop(decode_tx);
    let decode_result = decoder_thread
        .join()
        .unwrap_or_else(|_| Err(anyhow::anyhow!("The video decoder panicked.")));
    loop_result.and(decode_result)
}
