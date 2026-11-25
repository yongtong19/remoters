use std::{io::Read, time::Duration};

use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use remoters_core::{
    network::Pairing,
    session::webrtc::{SessionHost, ViewerSession},
    video::{AccessUnit, AnnexBReader, Codec, Format, access_units},
};

const FORMAT: Format = Format {
    codec: Codec::Hevc,
    width: 3840,
    height: 2160,
    frames_per_second: 120,
};

// NAL headers used to exercise framing, without invoking a codec.
const KEY: &[u8] = &[
    0, 0, 0, 1, 70, 1, 0x50, // AUD
    0, 0, 1, 64, 1, 0x80, // VPS
    0, 0, 1, 66, 1, 0x80, // SPS
    0, 0, 1, 68, 1, 0x80, // PPS
    0, 0, 1, 38, 1, 0x80, // IDR
];
const DELTA: &[u8] = &[
    0, 0, 1, 70, 1, 0x50, // AUD
    0, 0, 1, 2, 1, 0x80, // trailing picture
];

#[test]
fn extracts_delimited_access_units_and_requires_parameter_sets_for_recovery() -> Result<()> {
    let recording = Bytes::from([KEY, DELTA].concat());
    let units: Vec<_> = access_units(Codec::Hevc, recording).collect::<Result<_>>()?;
    assert_eq!(units.len(), 2);
    assert_eq!(units[0].bytes().as_ref(), KEY);
    assert!(units[0].is_random_access());
    assert_eq!(units[1].bytes().as_ref(), DELTA);
    assert!(!units[1].is_random_access());
    let idr = AccessUnit::from_annex_b(Codec::Hevc, 0, Bytes::from_static(&[0, 0, 1, 38, 1]))?;
    assert!(!idr.is_random_access());
    Ok(())
}

#[test]
fn live_encoder_delimiters_can_span_read_boundaries() -> Result<()> {
    struct ByteAtATime<R>(R);
    impl<R: Read> Read for ByteAtATime<R> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let length = buffer.len().min(1);
            self.0.read(&mut buffer[..length])
        }
    }

    let bytes = [KEY, DELTA, KEY].concat();
    let mut source = AnnexBReader::new(ByteAtATime(bytes.as_slice()), Codec::Hevc);
    for (index, expected) in [KEY, DELTA, KEY].into_iter().enumerate() {
        let unit = source.next_unit()?.unwrap();
        assert_eq!(unit.sequence, index as u64);
        assert_eq!(unit.bytes().as_ref(), expected);
    }
    assert!(source.next_unit()?.is_none());
    Ok(())
}

#[test]
fn web_rtc_delivers_annex_b_and_skips_incomplete_reference_chains() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, true)?;
            let address = host.address()?;
            let pairing = Pairing::parse(&host.pairing_code())?;
            let server = tokio::spawn(async move {
                let mut sender = host.accept().await?;
                for (sequence, bytes, expected) in [
                    (0, KEY, true),
                    (2, DELTA, false),
                    (3, KEY, true),
                    (4, DELTA, true),
                ] {
                    let unit =
                        AccessUnit::from_annex_b(Codec::Hevc, sequence, Bytes::from_static(bytes))?;
                    assert_eq!(sender.send(&unit).await?, expected);
                }
                tokio::time::timeout(Duration::from_secs(2), sender.keyframe_requested()).await??;
                sender.finish().await
            });
            let mut receiver = ViewerSession::connect(address, pairing).await?;
            let mut origin = None;
            for sequence in [0, 3, 4] {
                let frame = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
                    .await??
                    .unwrap();
                assert_eq!(frame.codec, FORMAT.codec);
                let first = *origin.get_or_insert(frame.rtp_timestamp);
                assert_eq!(frame.rtp_timestamp - first, sequence * 750);
                let unit = AccessUnit::from_annex_b(
                    Codec::Hevc,
                    sequence,
                    Bytes::copy_from_slice(&frame.bytes),
                )?;
                assert_eq!(unit.is_random_access(), sequence != 4);
            }
            server.await??;
            Ok(())
        })
}

#[test]
fn video_stream_requires_both_certificate_pin_and_secret() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, true)?;
            let address = host.address()?;
            let code = URL_SAFE_NO_PAD.decode(host.pairing_code())?;
            let server = tokio::spawn(async move {
                for _ in 0..2 {
                    assert!(host.accept().await.is_err());
                }
            });
            for byte in [0, 32] {
                let mut invalid = code.clone();
                invalid[byte] ^= 1;
                let pairing = Pairing::parse(&URL_SAFE_NO_PAD.encode(invalid))?;
                assert!(ViewerSession::connect(address, pairing).await.is_err());
            }
            tokio::time::timeout(Duration::from_secs(8), server).await??;
            Ok(())
        })
}

#[test]
fn web_rtc_reassembles_fragmented_hevc_slices_with_the_same_timestamp() -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, true)?;
            let address = host.address()?;
            let pairing = Pairing::parse(&host.pairing_code())?;
            let mut data = KEY.to_vec();
            data.extend(std::iter::repeat_n(0xaa, 2800));
            data.extend_from_slice(&[0, 0, 1, 38, 1]);
            data.extend(std::iter::repeat_n(0xbb, 3400));
            let key = AccessUnit::from_annex_b(Codec::Hevc, 0, data.into())?;
            let sender = tokio::spawn(async move {
                let mut session = host.accept().await?;
                assert!(session.send(&key).await?);
                let delta = AccessUnit::from_annex_b(Codec::Hevc, 1, Bytes::from_static(DELTA))?;
                assert!(session.send(&delta).await?);
                session.finish().await
            });
            let mut receiver = ViewerSession::connect(address, pairing).await?;
            let first = timeout_frame(&mut receiver).await?;
            assert!(first.bytes.ends_with(&[0xbb; 3400]));
            assert!(
                first
                    .bytes
                    .windows(2800)
                    .any(|window| window == [0xaa; 2800])
            );
            let second = timeout_frame(&mut receiver).await?;
            assert_eq!(second.rtp_timestamp, first.rtp_timestamp + 750);
            sender.await??;
            Ok(())
        })
}

async fn timeout_frame(
    receiver: &mut ViewerSession,
) -> Result<remoters_core::video::ReceivedFrame> {
    Ok(
        tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await??
            .expect("sender should deliver a video frame"),
    )
}

#[test]
fn web_rtc_control_is_ordered_and_respects_view_only() -> Result<()> {
    use remoters_core::{Button, Input, Point};
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            for view_only in [false, true] {
                let move_to = Point { x: 1234, y: 5678 };
                let events = [
                    Input::Move(move_to),
                    Input::Button {
                        at: move_to,
                        button: Button::Left,
                        down: true,
                    },
                    Input::Key {
                        code: 0x41,
                        down: true,
                    },
                    Input::Scroll {
                        at: move_to,
                        vertical: -120,
                        horizontal: 0,
                    },
                    Input::Release,
                ];
                let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, view_only)?;
                let address = host.address()?;
                let pairing = Pairing::parse(&host.pairing_code())?;
                let sender = tokio::spawn(async move {
                    let mut session = host.accept().await?;
                    let mut input = Vec::new();
                    while let Some(event) = session.input().await? {
                        input.push(event);
                    }
                    assert_eq!(input, if view_only { &[][..] } else { &events });
                    Ok::<_, anyhow::Error>(())
                });
                let receiver = ViewerSession::connect(address, pairing).await?;
                assert_eq!(receiver.can_control(), !view_only);
                for event in events {
                    receiver.input(event).await?;
                }
                receiver.disconnect().await;
                tokio::time::timeout(Duration::from_secs(3), sender).await???;
            }
            Ok(())
        })
}
