use anyhow::Result;
use remoters_core::{
    Button, CODEC_BIT_AV1, CODEC_BIT_H264, CODEC_BIT_HEVC, GamepadState, Input, Point,
    network::Pairing,
    session::{
        AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioFrame, ClipboardData, ClipboardImageFormat,
        CodecPreference, CursorImage, CursorShape, CursorUpdate, DisplayPreference, HostInput,
        MAX_AUDIO_SAMPLES, SessionAuxiliary, SessionHost, StreamPreference, ViewerSession,
        negotiate_codec,
    },
    video::{AccessUnit, Codec, Format},
};
use std::time::Duration;
use tokio::time::timeout;

const FORMAT: Format = Format {
    codec: Codec::H264,
    width: 3840,
    height: 2160,
    frames_per_second: 60,
    hdr: false,
};
const KEYFRAME: &[u8] = &[
    0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3,
];
const DELTA: &[u8] = &[0, 0, 0, 1, 0x41, 4];
const HEVC_KEYFRAME: &[u8] = &[
    0, 0, 0, 1, 0x40, 0x01, 1, 0, 0, 0, 1, 0x42, 0x01, 2, 0, 0, 0, 1, 0x44, 0x01, 3, 0, 0, 0, 1,
    0x26, 0x01, 4,
];
const HEVC_DELTA: &[u8] = &[0, 0, 0, 1, 0x02, 0x01, 5];
// AV1 OBUs with obu_has_size_field = 1:
// TD (type=2, len=0) + SEQ_HDR (type=1, len=2, 8-bit main non-reduced) + KEY_FRAME (type=6, len=2)
const AV1_KEYFRAME: &[u8] = &[0x12, 0x00, 0x0a, 0x02, 0x00, 0x00, 0x32, 0x02, 0x00, 0xaa];
// TD (type=2, len=0) + INTER_FRAME (type=6, len=2, show_existing=0, frame_type=1 -> 0x20)
const AV1_DELTA: &[u8] = &[0x12, 0x00, 0x32, 0x02, 0x20, 0xbb];

#[tokio::test]
async fn quic_delivers_timestamps_recovers_after_gap_and_routes_middle_drag() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let (mut sender, mut receiver) = tokio::try_join!(host.accept(), ViewerSession::connect(address, ticket))?;
        let unit = |sequence, bytes: &'static [u8]| -> Result<AccessUnit> {
            Ok(AccessUnit::from_annex_b(Codec::H264, sequence, bytes.into())?
                .with_rtp_timestamp(1_417 * sequence)
                .with_host_latency_us(1_200 + sequence as u32))
        };
        assert!(sender.send(&unit(0,KEYFRAME)?).await?);
        assert!(sender.send(&unit(1,DELTA)?).await?);
        assert!(!sender.send(&unit(3,DELTA)?).await?);
        assert!(matches!(sender.next_input().await?, Some(HostInput::Keyframe)));
        assert!(sender.send(&unit(4,KEYFRAME)?).await?);
        for sequence in [0,1,4] {
            let frame = receiver.recv().await?.unwrap();
            assert_eq!(frame.rtp_timestamp, 1_417 * sequence);
            assert_eq!(frame.host_latency_us, 1_200 + sequence as u32);
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
    for offset in [2usize, 75usize] {
        timeout(Duration::from_secs(6), async {
            let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, true)?;
            let code = host.pairing_code();
            let ticket = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
                .iter()
                .filter_map(|&replacement| {
                    let mut bytes = code.as_bytes().to_vec();
                    if bytes[offset] == replacement {
                        return None;
                    }
                    bytes[offset] = replacement;
                    let modified = String::from_utf8(bytes).ok()?;
                    Pairing::parse(&modified).ok()
                })
                .next()
                .expect("at least one modified base62 pairing code should parse");
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

#[tokio::test]
async fn quic_dynamic_host_receives_display_preference_and_negotiates_format() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let preference = DisplayPreference::normalized(2560, 1440, 60)?;
        let expected_format = Format {
            codec: Codec::H264,
            width: preference.width,
            height: preference.height,
            frames_per_second: preference.frames_per_second,
            hdr: false,
        };

        let host_task = async {
            let pending = host.accept_pending().await?;
            assert!(pending.peer_address().ip().is_loopback());
            assert_eq!(pending.display_preference(), Some(preference));
            let pref = pending.display_preference().unwrap();
            let format = Format {
                codec: Codec::H264,
                width: pref.width,
                height: pref.height,
                frames_per_second: pref.frames_per_second,
                hdr: false,
            };
            pending.start(format).await
        };

        let (mut sender, mut receiver) = tokio::try_join!(
            host_task,
            ViewerSession::connect_with_preference(address, ticket, Some(preference))
        )?;
        assert_eq!(receiver.format(), expected_format);

        let unit = AccessUnit::from_annex_b(Codec::H264, 0, KEYFRAME.into())?;
        assert!(sender.send(&unit).await?);
        let frame = receiver.recv().await?.unwrap();
        assert_eq!(&*frame.bytes, KEYFRAME);
        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_dynamic_host_receives_none_when_viewer_uses_connect() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, true)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        assert!(host.accept().await.is_err());

        let host_task = async {
            let pending = host.accept_pending().await?;
            assert_eq!(pending.display_preference(), None);
            pending.start(FORMAT).await
        };

        let (_sender, receiver) =
            tokio::try_join!(host_task, ViewerSession::connect(address, ticket))?;
        assert_eq!(receiver.format(), FORMAT);
        assert!(!receiver.can_control());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_rejecting_pending_session_fails_viewer_connect() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let preference = DisplayPreference::new(1920, 1080, 60)?;

        let host_task = async {
            let pending = host.accept_pending().await?;
            assert_eq!(pending.display_preference(), Some(preference));
            pending.reject(b"virtual display unavailable");
            Ok::<_, anyhow::Error>(())
        };

        let (host_res, viewer_res) = tokio::join!(
            host_task,
            ViewerSession::connect_with_preference(address, ticket, Some(preference))
        );
        host_res?;
        assert!(viewer_res.is_err());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_syncs_clipboard_bidirectionally_and_transfers_dropped_files() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let base = std::env::temp_dir().join(format!(
            "remoters-quic-aux-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let src_dir = base.join("src");
        let dst_dir = base.join("dst");
        std::fs::create_dir_all(&src_dir)?;
        let src_file = src_dir.join("hello.txt");
        let payload = b"hello from mac viewer to windows host!\n".repeat(4000);
        std::fs::write(&src_file, &payload)?;

        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let dst_for_host = dst_dir.clone();

        let (mut sender, mut receiver) = tokio::try_join!(
            async {
                let pending = host.accept_pending().await?;
                pending
                    .start_with_receive_dir(FORMAT, Some(dst_for_host))
                    .await
            },
            ViewerSession::connect(address, ticket)
        )?;

        let viewer_aux = receiver.aux_sender();
        let host_aux = sender.aux_sender();

        // 1. Viewer -> Host clipboard text
        let text = ClipboardData::Text("copied on mac".into());
        viewer_aux.send_clipboard(&text).await?;
        assert!(matches!(
            sender.next_input().await?,
            Some(HostInput::Clipboard(received)) if received == text
        ));

        // 2. Host -> Viewer clipboard image
        let image = ClipboardData::Image {
            format: ClipboardImageFormat::Png,
            bytes: vec![137, 80, 78, 71, 13, 10, 26, 10],
        };
        host_aux.send_clipboard(&image).await?;
        assert!(matches!(
            receiver.recv_aux().await,
            Some(SessionAuxiliary::Clipboard(received)) if received == image
        ));

        // 3. Viewer -> Host nested file transfer + duplicate filename deduplication
        let mut progress_steps = Vec::new();
        let sent_bytes = viewer_aux
            .send_file("bundle/hello.txt", &src_file, |sent, total| {
                progress_steps.push((sent, total));
            })
            .await?;
        assert_eq!(sent_bytes, payload.len() as u64);
        assert_eq!(progress_steps.last(), Some(&(sent_bytes, sent_bytes)));

        let first = match sender.next_input().await? {
            Some(HostInput::FileReceived(file)) => file,
            other => panic!("expected FileReceived, got {other:?}"),
        };
        assert_eq!(first.relative_path, "bundle/hello.txt");
        assert_eq!(first.saved_path, dst_dir.join("bundle").join("hello.txt"));
        assert_eq!(std::fs::read(&first.saved_path)?, payload);

        viewer_aux
            .send_file("bundle/hello.txt", &src_file, |_, _| {})
            .await?;
        let second = match sender.next_input().await? {
            Some(HostInput::FileReceived(file)) => file,
            other => panic!("expected duplicate FileReceived, got {other:?}"),
        };
        assert_eq!(
            second.saved_path,
            dst_dir.join("bundle").join("hello (1).txt")
        );
        assert_eq!(std::fs::read(&second.saved_path)?, payload);

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        let _ = std::fs::remove_dir_all(&base);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_view_only_rejects_incoming_clipboard_and_files() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let base = std::env::temp_dir().join(format!(
            "remoters-quic-ro-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&base)?;
        let src_file = base.join("blocked.txt");
        std::fs::write(&src_file, b"blocked")?;

        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, true)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let dst = base.join("dst");

        let (_sender, receiver) = tokio::try_join!(
            async {
                let pending = host.accept_pending().await?;
                pending.start_with_receive_dir(FORMAT, Some(dst)).await
            },
            ViewerSession::connect(address, ticket)
        )?;

        let aux = receiver.aux_sender();
        assert!(
            aux.send_clipboard(&ClipboardData::Text("nope".into()))
                .await
                .is_err()
        );
        assert!(
            aux.send_file("blocked.txt", &src_file, |_, _| {})
                .await
                .is_err()
        );

        let _ = std::fs::remove_dir_all(&base);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_streams_standard_hidden_and_custom_cursor_updates() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;

        let (sender, mut receiver) = tokio::try_join!(
            async {
                let pending = host.accept_pending().await?;
                pending.start(FORMAT).await
            },
            ViewerSession::connect(address, ticket)
        )?;

        assert!(
            receiver
                .aux_sender()
                .send_cursor(CursorUpdate::new(CursorShape::Arrow))
                .is_err()
        );
        assert!(
            sender
                .send_cursor(CursorUpdate {
                    shape: CursorShape::Arrow,
                    custom: Some(CursorImage {
                        width: 0,
                        height: 4,
                        hotspot_x: 0,
                        hotspot_y: 0,
                        rgba: vec![],
                    }),
                })
                .is_err()
        );

        for shape in [
            CursorShape::Arrow,
            CursorShape::IBeam,
            CursorShape::Crosshair,
            CursorShape::ClosedHand,
            CursorShape::OpenHand,
            CursorShape::PointingHand,
            CursorShape::ResizeLeft,
            CursorShape::ResizeRight,
            CursorShape::ResizeLeftRight,
            CursorShape::ResizeUp,
            CursorShape::ResizeDown,
            CursorShape::ResizeUpDown,
            CursorShape::ResizeUpLeftDownRight,
            CursorShape::ResizeUpRightDownLeft,
            CursorShape::ResizeColumn,
            CursorShape::ResizeRow,
            CursorShape::IBeamVertical,
            CursorShape::OperationNotAllowed,
            CursorShape::DragLink,
            CursorShape::DragCopy,
            CursorShape::ContextualMenu,
            CursorShape::Hidden,
        ] {
            let expected = CursorUpdate::new(shape);
            sender.send_cursor(expected.clone())?;
            let received = receiver.recv_aux().await;
            assert!(
                matches!(received, Some(SessionAuxiliary::Cursor(ref update)) if *update == expected),
                "expected cursor {expected:?}, got {received:?}"
            );
        }

        let custom = CursorUpdate::with_custom(
            CursorShape::Arrow,
            CursorImage::new(4, 4, 1, 2, vec![0xab; 4 * 4 * 4])?,
        )?;
        let aux_sender = sender.aux_sender();
        let (_video, _inputs) = sender.split();
        aux_sender.send_cursor(custom.clone())?;
        let received = receiver.recv_aux().await;
        assert!(
            matches!(received, Some(SessionAuxiliary::Cursor(ref update)) if *update == custom),
            "expected custom cursor, got {received:?}"
        );

        receiver.disconnect().await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_coalesces_cursor_burst_without_starving_clipboard() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;

        let (mut sender, mut receiver) = tokio::try_join!(
            async {
                let pending = host.accept_pending().await?;
                pending.start(FORMAT).await
            },
            ViewerSession::connect(address, ticket)
        )?;

        let host_aux = sender.aux_sender();
        let viewer_aux = receiver.aux_sender();

        for i in 0..500u32 {
            let shape = if i % 2 == 0 {
                CursorShape::IBeam
            } else {
                CursorShape::PointingHand
            };
            sender.send_cursor(CursorUpdate::new(shape))?;
        }
        let final_cursor = CursorUpdate::new(CursorShape::Crosshair);
        sender.send_cursor(final_cursor.clone())?;

        let host_clip = ClipboardData::Text("host clipboard during cursor burst".into());
        host_aux.send_clipboard(&host_clip).await?;

        let viewer_clip = ClipboardData::Text("viewer clipboard during cursor burst".into());
        viewer_aux.send_clipboard(&viewer_clip).await?;
        assert!(matches!(
            sender.next_input().await?,
            Some(HostInput::Clipboard(received)) if received == viewer_clip
        ));

        let mut saw_clipboard = false;
        let mut last_cursor = None;
        while !(saw_clipboard && last_cursor.as_ref() == Some(&final_cursor)) {
            match receiver.recv_aux().await {
                Some(SessionAuxiliary::Clipboard(data)) => {
                    assert_eq!(data, host_clip);
                    saw_clipboard = true;
                }
                Some(SessionAuxiliary::Cursor(update)) => {
                    last_cursor = Some(update);
                }
                other => panic!("unexpected aux event: {other:?}"),
            }
        }

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_streams_stereo_48khz_audio_frames_without_aux_wakeup() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;

        let (sender, mut receiver) = tokio::try_join!(
            async {
                let pending = host.accept_pending().await?;
                pending.start(FORMAT).await
            },
            ViewerSession::connect(address, ticket)
        )?;

        let mut audio_rx = receiver
            .take_audio_receiver()
            .expect("audio receiver present");
        assert!(receiver.take_audio_receiver().is_none());

        let sample_pcm: Vec<i16> = (0..1920)
            .map(|i| ((i * 37) % 30_000) as i16 - 15_000)
            .collect();
        let frame_one = AudioFrame::new(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, sample_pcm)?;
        let frame_two = AudioFrame::new(
            AUDIO_SAMPLE_RATE,
            AUDIO_CHANNELS,
            [1234i16, -1234].repeat(480),
        )?;
        let frame_three = AudioFrame::new(44_100, 1, vec![555; 441])?;

        assert!(receiver.aux_sender().send_audio(frame_one.clone()).is_err());
        assert!(
            sender
                .send_audio(AudioFrame {
                    sample_rate: AUDIO_SAMPLE_RATE,
                    channels: AUDIO_CHANNELS,
                    pcm: vec![],
                })
                .is_err()
        );
        assert!(
            sender
                .send_audio(AudioFrame {
                    sample_rate: AUDIO_SAMPLE_RATE,
                    channels: AUDIO_CHANNELS,
                    pcm: vec![0; MAX_AUDIO_SAMPLES + 2],
                })
                .is_err()
        );

        sender.send_audio(frame_one.clone())?;
        let host_aux = sender.aux_sender();
        host_aux.send_audio(frame_two.clone())?;
        let (video, _inputs) = sender.split();
        video.send_audio(frame_three.clone())?;

        assert_eq!(audio_rx.recv().await, Some(frame_one));
        assert_eq!(audio_rx.recv().await, Some(frame_two));
        assert_eq!(audio_rx.recv().await, Some(frame_three));

        // Audio must never wake SessionAuxiliary / recv_aux.
        assert!(
            timeout(Duration::from_millis(50), receiver.recv_aux())
                .await
                .is_err()
        );

        receiver.disconnect().await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_audio_bounded_queues_drop_and_never_stall_video_or_input() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;

        let (mut sender, mut receiver) = tokio::try_join!(
            async {
                let pending = host.accept_pending().await?;
                pending.start(FORMAT).await
            },
            ViewerSession::connect(address, ticket)
        )?;

        let mut audio_rx = receiver
            .take_audio_receiver()
            .expect("audio receiver present");

        // Burst 12 frames synchronously into the host's capacity-4 drop-oldest queue.
        for seq in 0..12i16 {
            let mut pcm = vec![seq; 1920];
            pcm[0] = seq;
            sender.send_audio(AudioFrame::new(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, pcm)?)?;
        }

        // Wait until the final frame arrives on the viewer.
        let mut received_tags = Vec::new();
        while received_tags.last() != Some(&11i16) {
            let frame = audio_rx.recv().await.expect("audio stream open");
            received_tags.push(frame.pcm[0]);
        }
        assert!(
            received_tags.len() <= 4,
            "expected host drop-oldest queue (cap 4) to coalesce burst, got {received_tags:?}"
        );

        // Now flood 20 frames across QUIC while the viewer does NOT read audio_rx (cap 8).
        for seq in 100..120i16 {
            sender.send_audio(AudioFrame::new(
                AUDIO_SAMPLE_RATE,
                AUDIO_CHANNELS,
                vec![seq; 960],
            )?)?;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;

        // Video, auxiliary clipboard, and input must continue without stalling.
        let unit = AccessUnit::from_annex_b(Codec::H264, 0, KEYFRAME.into())?;
        assert!(sender.send(&unit).await?);
        assert!(receiver.recv().await?.is_some());

        let clip = ClipboardData::Text("audio backpressure cannot stall aux".into());
        sender.aux_sender().send_clipboard(&clip).await?;
        assert!(matches!(
            receiver.recv_aux().await,
            Some(SessionAuxiliary::Clipboard(data)) if data == clip
        ));

        let move_event = Input::Move(Point { x: 42, y: 84 });
        receiver.input(move_event).await?;
        assert!(matches!(
            sender.next_input().await?,
            Some(HostInput::Remote(event)) if event == move_event
        ));

        let mut buffered = Vec::new();
        while let Ok(frame) = audio_rx.try_recv() {
            buffered.push(frame.pcm[0]);
        }
        assert!(
            buffered.len() <= 8,
            "viewer audio receiver exceeded capacity 8: {}",
            buffered.len()
        );

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_stream_preference_handshake_and_hevc_format_negotiation() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let display = DisplayPreference::normalized(2560, 1440, 90)?;
        let stream_pref =
            StreamPreference::new(Some(display), CodecPreference::Hevc, Some(90), Some(80))?;
        let expected_format = Format {
            codec: Codec::Hevc,
            width: display.width,
            height: display.height,
            frames_per_second: 90,
            hdr: false,
        };

        let host_task = async {
            let pending = host.accept_pending().await?;
            assert_eq!(pending.display_preference(), Some(display));
            assert_eq!(pending.stream_preference(), stream_pref);
            let pref = pending.stream_preference();
            let disp = pref.display.unwrap();
            pending
                .start(Format {
                    codec: match pref.codec {
                        CodecPreference::Av1 => Codec::Av1,
                        CodecPreference::Hevc => Codec::Hevc,
                        CodecPreference::Auto | CodecPreference::H264 => Codec::H264,
                    },
                    width: disp.width,
                    height: disp.height,
                    frames_per_second: pref.max_fps.unwrap_or(disp.frames_per_second),
                    hdr: pref.hdr,
                })
                .await
        };

        let (mut sender, mut receiver) = tokio::try_join!(
            host_task,
            ViewerSession::connect_with_stream_preference(address, ticket, stream_pref)
        )?;
        assert_eq!(receiver.format(), expected_format);

        let keyframe = AccessUnit::from_annex_b(Codec::Hevc, 0, HEVC_KEYFRAME.into())?;
        let delta = AccessUnit::from_annex_b(Codec::Hevc, 1, HEVC_DELTA.into())?;
        assert!(sender.send(&keyframe).await?);
        assert!(sender.send(&delta).await?);

        let frame0 = receiver.recv().await?.unwrap();
        assert_eq!(frame0.codec, Codec::Hevc);
        assert_eq!(&*frame0.bytes, HEVC_KEYFRAME);

        let frame1 = receiver.recv().await?.unwrap();
        assert_eq!(frame1.codec, Codec::Hevc);
        assert_eq!(&*frame1.bytes, HEVC_DELTA);

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_delivers_live_bitrate_updates_even_when_view_only() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        // 1. View-only host still receives live HostInput::Bitrate updates.
        let ro_host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, true)?;
        let ro_addr = ro_host.address()?;
        let ro_ticket = Pairing::parse(&ro_host.pairing_code())?;
        let (mut ro_sender, ro_receiver) = tokio::try_join!(
            async {
                let pending = ro_host.accept_pending().await?;
                assert_eq!(pending.stream_preference(), StreamPreference::default());
                pending.start(FORMAT).await
            },
            ViewerSession::connect(ro_addr, ro_ticket)
        )?;
        assert!(!ro_receiver.can_control());
        assert!(ro_receiver.set_bitrate_mbps(Some(4)).await.is_err());
        assert!(ro_receiver.set_bitrate_mbps(Some(151)).await.is_err());

        ro_receiver.set_bitrate_mbps(Some(80)).await?;
        assert!(matches!(
            ro_sender.next_input().await?,
            Some(HostInput::Bitrate(Some(80)))
        ));
        ro_receiver.set_bitrate_mbps(None).await?;
        assert!(matches!(
            ro_sender.next_input().await?,
            Some(HostInput::Bitrate(None))
        ));
        ro_receiver.disconnect().await;
        assert!(ro_sender.next_input().await?.is_none());

        // 2. Controllable session interleaves Bitrate with Input::Release (length 1) cleanly.
        let rw_host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let rw_addr = rw_host.address()?;
        let rw_ticket = Pairing::parse(&rw_host.pairing_code())?;
        let (mut rw_sender, mut rw_receiver) = tokio::try_join!(
            async {
                let pending = rw_host.accept_pending().await?;
                pending.start(FORMAT).await
            },
            ViewerSession::connect(rw_addr, rw_ticket)
        )?;

        rw_receiver.set_bitrate_mbps(Some(80)).await?;
        rw_receiver.input(Input::Release).await?;
        rw_receiver.set_bitrate_mbps(None).await?;
        assert!(matches!(
            rw_sender.next_input().await?,
            Some(HostInput::Bitrate(Some(80)))
        ));
        assert!(matches!(
            rw_sender.next_input().await?,
            Some(HostInput::Remote(Input::Release))
        ));
        assert!(matches!(
            rw_sender.next_input().await?,
            Some(HostInput::Bitrate(None))
        ));

        rw_receiver.disconnect().await;
        assert!(rw_sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_delivers_gamepad_reliably_and_drops_when_view_only() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        // 1. Controllable session delivers press, release, and disconnect reliably via INPUT_REQUEST.
        let host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let (mut sender, mut receiver) =
            tokio::try_join!(host.accept(), ViewerSession::connect(address, ticket))?;
        assert!(receiver.can_control());

        let press = GamepadState {
            slot: 0,
            connected: true,
            buttons: GamepadState::A | GamepadState::LEFT_SHOULDER | GamepadState::DPAD_UP,
            left_trigger: 120,
            right_trigger: 255,
            left_x: -16_384,
            left_y: 32_767,
            right_x: 8_192,
            right_y: -32_768,
        };
        let release = GamepadState {
            slot: 0,
            connected: true,
            ..GamepadState::default()
        };
        let disconnect = GamepadState {
            slot: 0,
            connected: false,
            ..GamepadState::default()
        };

        for state in [press, release, disconnect] {
            receiver.input(Input::Gamepad(state)).await?;
        }
        for expected in [press, release, disconnect] {
            assert!(matches!(
                sender.next_input().await?,
                Some(HostInput::Remote(Input::Gamepad(actual))) if actual == expected
            ));
        }

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());

        // 2. View-only session drops Input::Gamepad.
        let ro_host = SessionHost::bind("127.0.0.1:0".parse()?, FORMAT, true)?;
        let ro_address = ro_host.address()?;
        let ro_ticket = Pairing::parse(&ro_host.pairing_code())?;
        let (mut ro_sender, mut ro_receiver) = tokio::try_join!(
            ro_host.accept(),
            ViewerSession::connect(ro_address, ro_ticket)
        )?;
        assert!(!ro_receiver.can_control());

        ro_receiver.input(Input::Gamepad(press)).await?;
        assert!(
            timeout(Duration::from_millis(100), ro_sender.next_input())
                .await
                .is_err()
        );
        ro_receiver.disconnect().await;
        assert!(ro_sender.next_input().await?.is_none());

        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_negotiates_av1_hdr10_and_delivers_obu_frames() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let display = DisplayPreference::normalized(3840, 2160, 120)?;
        let host_caps = CODEC_BIT_H264 | CODEC_BIT_HEVC | CODEC_BIT_AV1;
        let stream_pref =
            StreamPreference::new(Some(display), CodecPreference::Auto, Some(120), Some(100))?
                .with_supported_codecs(host_caps)
                .with_hdr(true);
        let expected_format = Format {
            codec: Codec::Av1,
            width: display.width,
            height: display.height,
            frames_per_second: 120,
            hdr: true,
        };

        let host_task = async {
            let pending = host.accept_pending().await?;
            let pref = pending.stream_preference();
            assert_eq!(pref, stream_pref);
            let codec = negotiate_codec(pref.codec, pref.supported_codecs, host_caps);
            assert_eq!(codec, Codec::Av1);
            let disp = pref.display.unwrap();
            pending
                .start(Format {
                    codec,
                    width: disp.width,
                    height: disp.height,
                    frames_per_second: pref.max_fps.unwrap_or(disp.frames_per_second),
                    hdr: pref.hdr,
                })
                .await
        };

        let (mut sender, mut receiver) = tokio::try_join!(
            host_task,
            ViewerSession::connect_with_stream_preference(address, ticket, stream_pref)
        )?;
        assert_eq!(receiver.format(), expected_format);

        let keyframe = AccessUnit::from_annex_b(Codec::Av1, 0, AV1_KEYFRAME.into())?;
        assert!(keyframe.is_random_access());
        let delta = AccessUnit::from_annex_b(Codec::Av1, 1, AV1_DELTA.into())?;
        assert!(!delta.is_random_access());
        assert!(sender.send(&keyframe).await?);
        assert!(sender.send(&delta).await?);

        let frame0 = receiver.recv().await?.unwrap();
        assert_eq!(frame0.codec, Codec::Av1);
        assert_eq!(&*frame0.bytes, AV1_KEYFRAME);

        let frame1 = receiver.recv().await?.unwrap();
        assert_eq!(frame1.codec, Codec::Av1);
        assert_eq!(&*frame1.bytes, AV1_DELTA);

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
