use std::{
    net::SocketAddrV4,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, channel, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{ColorImage, Context};
use lan_remote_core::{
    Input,
    lan::{self, Pairing},
};

use crate::platform::decode_frame;

pub enum Event {
    Ready(bool),
    Closed(Result<()>),
}

enum Command {
    Input(Input),
    Stop,
}

pub struct Connection {
    sink: SyncSender<Command>,
    pub events: Receiver<Event>,
    latest: Arc<Mutex<Option<ColorImage>>>,
}

impl Connection {
    pub fn start(ctx: &Context, address: SocketAddrV4, pairing: Pairing) -> Result<Self> {
        let (sink, commands) = sync_channel(96);
        let (events, receiver) = channel();
        let latest = Arc::new(Mutex::new(None));
        let slot = latest.clone();
        let ctx = ctx.clone();
        thread::Builder::new()
            .name("lan-viewer".into())
            .spawn(move || {
                let result = run(
                    commands,
                    address,
                    pairing,
                    |status| {
                        let _ = events.send(Event::Ready(status));
                        ctx.request_repaint();
                    },
                    |frame| {
                        *slot.lock().unwrap() = Some(frame);
                        ctx.request_repaint();
                    },
                );
                let _ = events.send(Event::Closed(result));
                ctx.request_repaint();
            })?;
        Ok(Self {
            sink,
            events: receiver,
            latest,
        })
    }

    pub fn take_frame(&self) -> Option<ColorImage> {
        self.latest.lock().unwrap().take()
    }

    #[must_use]
    pub fn input(&self, event: Input) -> bool {
        match self.sink.try_send(Command::Input(event)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => matches!(event, Input::Move(_)),
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.sink.try_send(Command::Stop);
    }
}

fn run(
    commands: Receiver<Command>,
    address: SocketAddrV4,
    pairing: Pairing,
    status: impl Fn(bool),
    frame: impl Fn(ColorImage),
) -> Result<()> {
    let mut session = lan::connect(address, pairing)?;
    status(session.can_control());
    let mut next_frame = Instant::now();
    loop {
        // Keep the ordered input queue responsive and prevent movement from starving video.
        for _ in 0..96 {
            match commands.try_recv() {
                Ok(Command::Input(input)) => session.input(input)?,
                Ok(Command::Stop) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    session.disconnect();
                    return Ok(());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
            }
        }
        let now = Instant::now();
        if now >= next_frame {
            frame(decode_frame(session.frame()?)?);
            next_frame = now + Duration::from_millis(66);
        } else {
            match commands.recv_timeout(next_frame - now) {
                Ok(Command::Input(input)) => session.input(input)?,
                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    session.disconnect();
                    return Ok(());
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}
