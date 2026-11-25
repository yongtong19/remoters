use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use remoters_core::{
    Button, Input, Point,
    network::Pairing,
    session::{HostInput, SessionHost, ViewerSession},
    video::{AccessUnit, Codec, Format},
};
use std::time::Duration;
use tokio::time::timeout;

const FORMAT: Format = Format {
    codec: Codec::H264,
    width: 3840,
    height: 2160,
    frames_per_second: 60,
};
const KEYFRAME: &[u8] = &[
    0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3,
];
const DELTA: &[u8] = &[0, 0, 0, 1, 0x41, 4];

#[tokio::test]
async fn quic_delivers_timestamps_recovers_after_gap_and_routes_middle_drag() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let (mut sender, mut receiver) = tokio::try_join!(host.accept(), ViewerSession::connect(address, ticket))?;
        let unit = |sequence, bytes: &'static [u8]| -> Result<AccessUnit> {
            Ok(AccessUnit::from_annex_b(Codec::H264, sequence, bytes.into())?
                .with_rtp_timestamp(1_417 * sequence))
        };
        assert!(sender.send(&unit(0,KEYFRAME)?).await?);
        assert!(sender.send(&unit(1,DELTA)?).await?);
        assert!(!sender.send(&unit(3,DELTA)?).await?);
        assert!(matches!(sender.next_input().await?, Some(HostInput::Keyframe)));
        assert!(sender.send(&unit(4,KEYFRAME)?).await?);
        for sequence in [0,1,4] {
            let frame = receiver.recv().await?.unwrap();
            assert_eq!(frame.rtp_timestamp, 1_417 * sequence);
            assert_eq!(&*frame.bytes, if sequence == 1 { DELTA } else { KEYFRAME });
        }

        let at = Point { x: 25, y: 10 };
        let drag = Point { x: 525, y: 210 };
        let down = Input::Button { at, button: Button::Middle, down: true };
        receiver.input(down).await?;
        receiver.input(Input::Move(drag)).await?;
        assert!(matches!(sender.next_input().await?, Some(HostInput::Remote(event)) if event == down));
        assert!(matches!(sender.next_input().await?, Some(HostInput::Remote(Input::Move(point))) if point == drag));
        let up = Input::Button { at: drag, button: Button::Middle, down: false };
        receiver.input(up).await?;
        assert!(matches!(sender.next_input().await?, Some(HostInput::Remote(event)) if event == up));
        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    }).await?
}

#[tokio::test]
async fn quic_rejects_modified_pin_and_modified_secret() -> Result<()> {
    for offset in [0, 32] {
        timeout(Duration::from_secs(6), async {
            let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, true)?;
            let mut bytes = URL_SAFE_NO_PAD.decode(host.pairing_code())?;
            bytes[offset] ^= 1;
            let ticket = Pairing::parse(&URL_SAFE_NO_PAD.encode(bytes))?;
            let (host, viewer) = tokio::join!(
                host.accept(),
                ViewerSession::connect(host.address()?, ticket)
            );
            assert!(viewer.is_err());
            assert!(host.is_err());
            Ok::<_, anyhow::Error>(())
        })
        .await??;
    }
    Ok(())
}

#[tokio::test]
async fn quic_split_host_delivers_video_and_input_concurrently() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, false)?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let (session, mut receiver) = tokio::try_join!(
            host.accept(),
            ViewerSession::connect(host.address()?, ticket)
        )?;
        let (mut video, mut inputs) = session.split();
        let unit = AccessUnit::from_annex_b(Codec::H264, 0, KEYFRAME.into())?;
        let move_event = Input::Move(Point { x: 100, y: 200 });
        receiver.input(move_event).await?;
        let (sent, frame, received_input) =
            tokio::try_join!(video.send(&unit), receiver.recv(), inputs.next_input())?;
        assert!(sent);
        assert!(frame.is_some());
        assert!(matches!(
            received_input,
            Some(HostInput::Remote(event)) if event == move_event
        ));
        receiver.disconnect().await;
        assert!(inputs.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_view_only_negotiates_no_input() -> Result<()> {
    let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, true)?;
    let ticket = Pairing::parse(&host.pairing_code())?;
    let (mut sender, mut receiver) = tokio::try_join!(
        host.accept(),
        ViewerSession::connect(host.address()?, ticket)
    )?;
    assert!(!receiver.can_control());
    receiver.input(Input::Release).await?;
    assert!(
        timeout(Duration::from_millis(100), sender.next_input())
            .await
            .is_err()
    );
    Ok(())
}
