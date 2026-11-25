use std::{
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use remoters_core::{
    Desktop, Input,
    lan::{self, Host, Pairing},
};

#[derive(Debug, PartialEq)]
enum Observed {
    Opened,
    Input(Input),
    Released,
}

struct TestDesktop(Sender<Observed>);

impl Desktop for TestDesktop {
    fn frame(&mut self) -> Result<Vec<u8>> {
        Ok(vec![0xFF, 0xD8, 0xFF, 0xD9])
    }

    fn input(&mut self, input: Input) -> Result<()> {
        self.0.send(Observed::Input(input))?;
        Ok(())
    }

    fn release(&mut self) {
        let _ = self.0.send(Observed::Released);
    }
}

fn host(view_only: bool) -> Result<(Host, Receiver<Observed>)> {
    let (sender, events) = mpsc::channel();
    let host = Host::start(
        "127.0.0.1:0".parse()?,
        view_only,
        move || {
            sender.send(Observed::Opened)?;
            Ok(TestDesktop(sender.clone()))
        },
        |_| {},
    )?;
    Ok((host, events))
}

#[test]
fn rejects_a_wrong_pin_or_secret_before_opening_the_desktop() -> Result<()> {
    let (host, observed) = host(false)?;
    for index in [0, 32] {
        let mut bytes = URL_SAFE_NO_PAD.decode(&host.code)?;
        bytes[index] ^= 1;
        let pairing = Pairing::parse(&URL_SAFE_NO_PAD.encode(bytes))?;
        assert!(lan::connect(host.address, pairing).is_err());
    }
    assert!(observed.try_recv().is_err());
    Ok(())
}

#[test]
fn sends_frames_and_releases_keys_when_the_viewer_drops() -> Result<()> {
    let (host, observed) = host(false)?;
    let mut viewer = lan::connect(host.address, Pairing::parse(&host.code)?)?;
    assert!(viewer.can_control());
    assert_eq!(viewer.frame()?, [0xFF, 0xD8, 0xFF, 0xD9]);
    let press = Input::Key {
        code: 0x41,
        down: true,
    };
    viewer.input(press)?;
    drop(viewer);
    for event in [Observed::Opened, Observed::Input(press), Observed::Released] {
        assert_eq!(observed.recv_timeout(Duration::from_secs(2))?, event);
    }
    Ok(())
}

#[test]
fn view_only_session_keeps_screen_access_and_drops_input() -> Result<()> {
    let (host, observed) = host(true)?;
    let mut viewer = lan::connect(host.address, Pairing::parse(&host.code)?)?;
    assert!(!viewer.can_control());
    viewer.input(Input::Key {
        code: 0x41,
        down: true,
    })?;
    assert!(!viewer.frame()?.is_empty());
    viewer.disconnect();
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(2))?,
        Observed::Opened
    );
    assert_eq!(
        observed.recv_timeout(Duration::from_secs(2))?,
        Observed::Released
    );
    Ok(())
}
