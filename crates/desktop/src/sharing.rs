use std::{
    net::{SocketAddr, SocketAddrV4},
    thread,
    time::Instant,
};

use anyhow::{Context, Result};
use remoters_core::{
    session::{HostInput, HostSession, SessionHost},
    video::Format,
};
use tokio::{runtime::Builder, sync::oneshot};

use crate::{codec::Encoder, metrics::HostStats, platform::NativeDesktop};

#[derive(Debug)]
pub enum HostEvent {
    Connected(SocketAddr),
    Finished(Result<()>),
    Stopped,
}

pub struct Host {
    pub address: SocketAddrV4,
    pub code: String,
    _stop: oneshot::Sender<()>,
}

impl Host {
    pub fn start(
        address: SocketAddrV4,
        view_only: bool,
        format: Format,
        display_id: u32,
        event: impl Fn(HostEvent) + Send + 'static,
    ) -> Result<Self> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let host = {
            let _entered = runtime.enter();
            SessionHost::bind(address, format, view_only)
                .with_context(|| format!("Could not listen on {address}."))?
        };
        let address = host.address()?;
        let code = host.pairing_code();
        let (stop, mut stopped) = oneshot::channel();
        thread::Builder::new()
            .name("lan-host".into())
            .spawn(move || {
                runtime.block_on(async {
                    loop {
                        let result = tokio::select! {
                            _ = &mut stopped => break,
                            result = host.accept() => result,
                        };
                        let result = match result {
                            Ok(session) => tokio::select! {
                                _ = &mut stopped => break,
                                result = share(session, format, display_id, view_only, &event) => result,
                            },
                            Err(error) => Err(error),
                        };
                        event(HostEvent::Finished(result));
                    }
                    event(HostEvent::Stopped);
                });
            })?;
        Ok(Self {
            address,
            code,
            _stop: stop,
        })
    }
}

async fn share(
    session: HostSession,
    format: Format,
    display_id: u32,
    view_only: bool,
    event: &impl Fn(HostEvent),
) -> Result<()> {
    let monitor = crate::platform::display::monitor_by_id(display_id)?;
    let mut encoder = Encoder::start(format, &monitor)?;
    let keyframe = encoder.keyframe_trigger();
    let mut desktop = NativeDesktop::new(view_only, monitor)?;
    event(HostEvent::Connected(session.peer_address()));
    let (mut video, mut inputs) = session.split();
    let input_loop = async {
        while let Some(input) = inputs.next_input().await? {
            match input {
                HostInput::Remote(input) => desktop.input(input)?,
                HostInput::Keyframe => keyframe.request(),
            }
        }
        Ok(())
    };
    let video_loop = async {
        let mut stats = HostStats::new();
        while let Some(unit) = encoder.units.recv().await {
            let unit = unit?;
            let started = Instant::now();
            let sent = video.send(&unit).await?;
            stats.record(
                unit.bytes().len(),
                unit.is_random_access(),
                sent,
                started.elapsed(),
                video.rtt(),
            );
        }
        Err(anyhow::anyhow!("The desktop encoder stopped."))
    };
    tokio::select! {
        biased;
        result = input_loop => result,
        result = video_loop => result,
    }
}
