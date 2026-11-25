use std::io::{Read, Write};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use subtle::ConstantTimeEq;

use crate::protocol::{self, Input};

/// A backend is constructed after the viewer proves it has the pairing code.
pub trait Desktop {
    fn frame(&mut self) -> Result<Vec<u8>>;
    fn input(&mut self, event: Input) -> Result<()>;
    fn release(&mut self);
}

/// The viewer protocol accepts any authenticated, ordered byte stream.
pub struct Viewer<S> {
    io: S,
    can_control: bool,
}

impl<S: Read + Write> Viewer<S> {
    pub(crate) fn authenticate(mut io: S, secret: &[u8; 24]) -> Result<Self> {
        protocol::write(&mut io, protocol::PAIR, secret)?;
        let packet = protocol::read(&mut io, 256)?;
        let can_control = match (packet.kind, packet.body.as_slice()) {
            (protocol::READY, [mode @ 0..=1]) => *mode == 1,
            (protocol::ERROR, b"pairing") => bail!("The pairing code was rejected by the host."),
            (protocol::ERROR, b"desktop") => bail!("The host cannot access its desktop."),
            _ => bail!("The host did not accept the session."),
        };
        Ok(Self { io, can_control })
    }

    pub fn can_control(&self) -> bool {
        self.can_control
    }

    pub fn frame(&mut self) -> Result<Vec<u8>> {
        protocol::write(&mut self.io, protocol::REQUEST_FRAME, &[])?;
        let packet = protocol::read(&mut self.io, protocol::MAX_FRAME)?;
        match (packet.kind, packet.body.as_slice()) {
            (protocol::FRAME, [_, ..]) => Ok(packet.body),
            (protocol::ERROR, b"desktop") => bail!("The host cannot access its desktop."),
            _ => bail!("Expected a desktop frame."),
        }
    }

    pub fn input(&mut self, event: Input) -> Result<()> {
        if self.can_control {
            event.write(&mut self.io)?;
        }
        Ok(())
    }

    pub fn disconnect(mut self) {
        let _ = self.input(Input::Release);
        let _ = protocol::write(&mut self.io, protocol::DISCONNECT, &[]);
    }
}

pub(crate) fn serve<S: Read + Write, D: Desktop>(
    mut io: S,
    secret: &[u8; 24],
    view_only: bool,
    make_desktop: impl FnOnce() -> Result<D>,
) -> Result<()> {
    let packet = protocol::read(&mut io, 32)?;
    if packet.kind != protocol::PAIR || !bool::from(packet.body.as_slice().ct_eq(secret)) {
        protocol::write(&mut io, protocol::ERROR, b"pairing")?;
        bail!("A viewer supplied an invalid pairing code.");
    }

    let mut desktop = match make_desktop() {
        Ok(desktop) => desktop,
        Err(error) => {
            let _ = protocol::write(&mut io, protocol::ERROR, b"desktop");
            return Err(error);
        }
    };
    let result = drive(&mut io, &mut desktop, view_only);
    desktop.release();
    result
}

fn drive(io: &mut (impl Read + Write), desktop: &mut impl Desktop, view_only: bool) -> Result<()> {
    protocol::write(io, protocol::READY, &[u8::from(!view_only)])?;
    let mut earliest_frame = Instant::now();
    loop {
        let packet = protocol::read(io, 32)?;
        match (packet.kind, packet.body.as_slice()) {
            (protocol::DISCONNECT, []) => return Ok(()),
            (protocol::REQUEST_FRAME, []) => {
                std::thread::sleep(earliest_frame.saturating_duration_since(Instant::now()));
                earliest_frame = Instant::now() + Duration::from_millis(66);
                let jpeg = match desktop.frame() {
                    Ok(jpeg) => jpeg,
                    Err(error) => {
                        let _ = protocol::write(io, protocol::ERROR, b"desktop");
                        return Err(error);
                    }
                };
                protocol::write(io, protocol::FRAME, &jpeg)?;
            }
            _ => {
                let input = Input::parse(&packet)?;
                if !view_only {
                    if input == Input::Release {
                        desktop.release();
                    } else {
                        desktop.input(input)?;
                    }
                }
            }
        }
    }
}
