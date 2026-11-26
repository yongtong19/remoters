use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use socket2::Socket;
use tokio::time::timeout;

use super::{
    aux::AUX_AUDIO,
    aux_types::AudioSendQueue,
    protocol::{
        BITRATE_REQUEST, decode_bitrate_mbps, encode_bitrate_mbps, format_packet, parse_format,
    },
    transport::{bind_udp_socket, write_control},
    *,
};
use crate::{
    network::Pairing,
    session::fec,
    video::{AccessUnit, Codec, Format},
};

#[test]
fn display_preference_validation_and_normalization() {
    assert!(DisplayPreference::new(1920, 1080, 60).is_ok());
    assert!(DisplayPreference::new(1921, 1080, 60).is_err());
    assert!(DisplayPreference::new(1920, 1080, 0).is_err());
    assert!(DisplayPreference::new(1920, 1080, 121).is_err());
    assert!(DisplayPreference::new(0, 0, 60).is_err());

    assert!(DisplayPreference::normalized(1, 1080, 60).is_err());
    let scaled_landscape = DisplayPreference::normalized(5120, 2880, 144).unwrap();
    assert_eq!(
        scaled_landscape,
        DisplayPreference {
            width: 3840,
            height: 2160,
            frames_per_second: 120,
        }
    );

    let scaled_portrait = DisplayPreference::normalized(2400, 4800, 0).unwrap();
    assert_eq!(
        scaled_portrait,
        DisplayPreference {
            width: 1920,
            height: 3840,
            frames_per_second: 1,
        }
    );

    let odd = DisplayPreference::normalized(2559, 1439, 60).unwrap();
    assert_eq!(
        odd,
        DisplayPreference {
            width: 2558,
            height: 1438,
            frames_per_second: 60,
        }
    );
}

#[test]
fn display_preference_wire_round_trip() {
    let none_bytes = DisplayPreference::encode(None).unwrap();
    assert_eq!(none_bytes, [0; 6]);
    assert_eq!(DisplayPreference::decode(none_bytes).unwrap(), None);

    let pref = DisplayPreference::new(2560, 1600, 90).unwrap();
    let bytes = DisplayPreference::encode(Some(pref)).unwrap();
    assert_eq!(DisplayPreference::decode(bytes).unwrap(), Some(pref));

    // Partial zeros must fail validation rather than decoding as None.
    assert!(DisplayPreference::decode([0, 0, 4, 56, 0, 60]).is_err());
}

#[test]
fn sanitize_relative_path_validates_and_rejects_unsafe_paths() {
    assert_eq!(
        sanitize_relative_path("report.pdf").unwrap(),
        PathBuf::from("report.pdf")
    );
    assert_eq!(
        sanitize_relative_path("folder/sub\\clip.png").unwrap(),
        PathBuf::from("folder").join("sub").join("clip.png")
    );
    for bad in [
        "",
        "/etc/passwd",
        "\\Windows\\System32",
        "../secret",
        "a/../b",
        "./a",
        "C:\\Users\\a.txt",
        "a//b",
        "bad*name.txt",
        "bad\0.txt",
    ] {
        assert!(
            sanitize_relative_path(bad).is_err(),
            "expected err for {bad:?}"
        );
    }
}

#[test]
fn unique_destination_deduplicates_without_overwriting() {
    let base = std::env::temp_dir().join(format!(
        "remoters-unique-dest-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let rel = Path::new("note.txt");
    let first = unique_destination(&base, rel).unwrap();
    assert_eq!(first, base.join("note.txt"));
    std::fs::write(&first, b"1").unwrap();

    let second = unique_destination(&base, rel).unwrap();
    assert_eq!(second, base.join("note (1).txt"));
    std::fs::write(&second, b"2").unwrap();

    let third = unique_destination(&base, rel).unwrap();
    assert_eq!(third, base.join("note (2).txt"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn cursor_shape_and_image_validation() {
    for byte in 0..=21u8 {
        let shape = CursorShape::from_byte(byte).unwrap();
        assert_eq!(u8::from(shape), byte);
    }
    assert!(CursorShape::from_byte(22).is_none());
    assert!(CursorShape::try_from(255).is_err());

    let standard = CursorUpdate::new(CursorShape::IBeam);
    assert_eq!(
        standard.encode().unwrap(),
        vec![CursorShape::IBeam as u8, 0]
    );

    assert!(CursorImage::new(0, 16, 0, 0, vec![]).is_err());
    assert!(CursorImage::new(16, 0, 0, 0, vec![]).is_err());
    assert!(CursorImage::new(129, 1, 0, 0, vec![0; 129 * 4]).is_err());
    assert!(CursorImage::new(1, 129, 0, 0, vec![0; 129 * 4]).is_err());
    assert!(CursorImage::new(4, 4, 0, 0, vec![0; 15]).is_err());

    let valid = CursorImage::new(128, 128, 64, 64, vec![0xff; 128 * 128 * 4]).unwrap();
    let encoded = CursorUpdate::with_custom(CursorShape::Crosshair, valid)
        .unwrap()
        .encode()
        .unwrap();
    assert_eq!(encoded[0], CursorShape::Crosshair as u8);
    assert_eq!(encoded[1], 1);
    assert_eq!(encoded.len(), 10 + 128 * 128 * 4);
}

#[test]
fn audio_frame_validation_and_wire_encoding() {
    let valid_stereo = AudioFrame::new(
        AUDIO_SAMPLE_RATE,
        AUDIO_CHANNELS,
        vec![i16::MIN, -1, 0, i16::MAX],
    )
    .unwrap();
    let encoded = valid_stereo.encode().unwrap();
    assert_eq!(&encoded[0..4], &48_000u32.to_be_bytes());
    assert_eq!(encoded[4], 2);
    assert_eq!(encoded[5], 0);
    assert_eq!(&encoded[6..8], &4u16.to_be_bytes());
    assert_eq!(&encoded[8..10], &i16::MIN.to_le_bytes());
    assert_eq!(&encoded[10..12], &(-1i16).to_le_bytes());
    assert_eq!(&encoded[12..14], &0i16.to_le_bytes());
    assert_eq!(&encoded[14..16], &i16::MAX.to_le_bytes());

    assert!(AudioFrame::new(8_000, 1, vec![42; 80]).is_ok());
    assert!(AudioFrame::new(96_000, 2, vec![1; MAX_AUDIO_SAMPLES]).is_ok());

    assert!(AudioFrame::new(7_999, 2, vec![0, 0]).is_err());
    assert!(AudioFrame::new(96_001, 2, vec![0, 0]).is_err());
    assert!(AudioFrame::new(48_000, 0, vec![0, 0]).is_err());
    assert!(AudioFrame::new(48_000, 3, vec![0, 0, 0]).is_err());
    assert!(AudioFrame::new(48_000, 2, vec![]).is_err());
    assert!(AudioFrame::new(48_000, 2, vec![1, 2, 3]).is_err());
    assert!(AudioFrame::new(48_000, 2, vec![0; MAX_AUDIO_SAMPLES + 2]).is_err());
}

#[test]
fn audio_send_queue_drops_oldest_when_full() {
    let queue = AudioSendQueue::new();
    for seq in 0..6i16 {
        queue
            .push(AudioFrame::new(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, vec![seq, -seq]).unwrap())
            .unwrap();
    }
    let mut drained = Vec::new();
    while let Some(frame) = queue.pop() {
        drained.push(frame.pcm[0]);
    }
    assert_eq!(drained, vec![2, 3, 4, 5]);

    queue.close();
    assert!(
        queue
            .push(AudioFrame::new(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, vec![9, 9]).unwrap())
            .is_err()
    );
}

#[tokio::test]
async fn host_rejects_incoming_aux_audio_stream_from_viewer() -> Result<()> {
    let format = Format {
        codec: Codec::H264,
        width: 1920,
        height: 1080,
        frames_per_second: 60,
        hdr: false,
    };
    let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
    let address = host.address()?;
    let ticket = Pairing::parse(&host.pairing_code())?;
    let (mut _sender, receiver) = tokio::try_join!(
        async {
            let pending = host.accept_pending().await?;
            pending.start(format).await
        },
        ViewerSession::connect(address, ticket)
    )?;

    let (mut send, mut recv) = receiver.connection.open_bi().await?;
    send.write_all(&[AUX_AUDIO]).await?;
    let encoded = AudioFrame::new(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, vec![10, -10])?.encode()?;
    send.write_all(&encoded).await?;
    send.finish()?;

    let mut ack = [0u8; 1];
    recv.read_exact(&mut ack).await?;
    assert_eq!(ack[0], 1);

    receiver.disconnect().await;
    Ok(())
}

#[test]
fn codec_preference_and_stream_preference_validation_and_wire_round_trip() {
    assert_eq!(CodecPreference::default(), CodecPreference::Auto);
    for (byte, expected) in [
        (0u8, CodecPreference::Auto),
        (1u8, CodecPreference::H264),
        (2u8, CodecPreference::Hevc),
        (3u8, CodecPreference::Av1),
    ] {
        assert_eq!(CodecPreference::from_byte(byte), Some(expected));
        assert_eq!(CodecPreference::try_from(byte).unwrap(), expected);
        assert_eq!(u8::from(expected), byte);
    }
    assert!(CodecPreference::from_byte(4).is_none());
    assert!(CodecPreference::try_from(255).is_err());

    let default_pref = StreamPreference::default();
    assert_eq!(
        default_pref,
        StreamPreference {
            display: None,
            codec: CodecPreference::Auto,
            max_fps: None,
            target_bitrate_mbps: None,
            supported_codecs: 0,
            hdr: false,
        }
    );
    let default_bytes = default_pref.encode().unwrap();
    assert_eq!(default_bytes, [0; 13]);
    assert_eq!(
        StreamPreference::decode(default_bytes).unwrap(),
        default_pref
    );

    let disp = DisplayPreference::new(2560, 1440, 90).unwrap();
    let hevc =
        StreamPreference::new(Some(disp), CodecPreference::Hevc, Some(90), Some(80)).unwrap();
    let encoded = hevc.encode().unwrap();
    assert_eq!(encoded[6], 2);
    assert_eq!(u16::from_be_bytes([encoded[7], encoded[8]]), 90);
    assert_eq!(u16::from_be_bytes([encoded[9], encoded[10]]), 80);
    assert_eq!(encoded[11], 0);
    assert_eq!(encoded[12], 0);
    assert_eq!(StreamPreference::decode(encoded).unwrap(), hevc);

    let av1_hdr = StreamPreference::new(Some(disp), CodecPreference::Av1, Some(120), Some(100))
        .unwrap()
        .with_supported_codecs(CODEC_BIT_H264 | CODEC_BIT_HEVC | CODEC_BIT_AV1)
        .with_hdr(true);
    let av1_encoded = av1_hdr.encode().unwrap();
    assert_eq!(av1_encoded[6], 3);
    assert_eq!(av1_encoded[11], 7);
    assert_eq!(av1_encoded[12], 1);
    assert_eq!(StreamPreference::decode(av1_encoded).unwrap(), av1_hdr);

    let bounds_min = StreamPreference::new(None, CodecPreference::H264, Some(30), Some(5)).unwrap();
    assert_eq!(
        StreamPreference::decode(bounds_min.encode().unwrap()).unwrap(),
        bounds_min
    );
    let bounds_max =
        StreamPreference::new(None, CodecPreference::Auto, Some(120), Some(150)).unwrap();
    assert_eq!(
        StreamPreference::decode(bounds_max.encode().unwrap()).unwrap(),
        bounds_max
    );

    for invalid_fps in [0, 29, 121] {
        assert!(
            StreamPreference::new(None, CodecPreference::Auto, Some(invalid_fps), None).is_err()
        );
    }
    for invalid_bitrate in [0, 4, 151] {
        assert!(
            StreamPreference::new(None, CodecPreference::Auto, None, Some(invalid_bitrate))
                .is_err()
        );
    }
    assert!(
        StreamPreference {
            display: Some(DisplayPreference {
                width: 1921,
                height: 1080,
                frames_per_second: 60,
            }),
            codec: CodecPreference::Auto,
            max_fps: None,
            target_bitrate_mbps: None,
            supported_codecs: 0,
            hdr: false,
        }
        .validate()
        .is_err()
    );
    assert!(
        StreamPreference::default()
            .with_supported_codecs(8)
            .validate()
            .is_err()
    );

    // Invalid wire bytes must fail decode.
    let mut bad_codec = [0u8; 13];
    bad_codec[6] = 4;
    assert!(StreamPreference::decode(bad_codec).is_err());

    let mut bad_fps = [0u8; 13];
    bad_fps[7..9].copy_from_slice(&29u16.to_be_bytes());
    assert!(StreamPreference::decode(bad_fps).is_err());

    let mut bad_bitrate = [0u8; 13];
    bad_bitrate[9..11].copy_from_slice(&151u16.to_be_bytes());
    assert!(StreamPreference::decode(bad_bitrate).is_err());

    let mut bad_mask = [0u8; 13];
    bad_mask[11] = 0b1000;
    assert!(StreamPreference::decode(bad_mask).is_err());

    let mut bad_hdr = [0u8; 13];
    bad_hdr[12] = 2;
    assert!(StreamPreference::decode(bad_hdr).is_err());
}

#[test]
fn negotiate_codec_enforces_av1_gt_hevc_gt_h264_priority() {
    let all = CODEC_BIT_H264 | CODEC_BIT_HEVC | CODEC_BIT_AV1;
    let hevc_h264 = CODEC_BIT_H264 | CODEC_BIT_HEVC;

    // Auto selects AV1 > HEVC > H264 over common capabilities.
    assert_eq!(negotiate_codec(CodecPreference::Auto, all, all), Codec::Av1);
    assert_eq!(
        negotiate_codec(CodecPreference::Auto, all, hevc_h264),
        Codec::Hevc
    );
    assert_eq!(
        negotiate_codec(CodecPreference::Auto, hevc_h264, all),
        Codec::Hevc
    );
    assert_eq!(
        negotiate_codec(CodecPreference::Auto, CODEC_BIT_H264, all),
        Codec::H264
    );

    // Legacy viewer mask 0 allows full host priority.
    assert_eq!(negotiate_codec(CodecPreference::Auto, 0, all), Codec::Av1);
    assert_eq!(
        negotiate_codec(CodecPreference::Auto, 0, hevc_h264),
        Codec::Hevc
    );
    assert_eq!(negotiate_codec(CodecPreference::Auto, 0, 0), Codec::H264);

    // Explicit preference honored if common, otherwise falls back by AV1 > HEVC > H264.
    assert_eq!(
        negotiate_codec(CodecPreference::Hevc, all, all),
        Codec::Hevc
    );
    assert_eq!(
        negotiate_codec(CodecPreference::H264, all, all),
        Codec::H264
    );
    assert_eq!(
        negotiate_codec(CodecPreference::Av1, all, hevc_h264),
        Codec::Hevc
    );
    assert_eq!(
        negotiate_codec(CodecPreference::Av1, all, CODEC_BIT_H264),
        Codec::H264
    );
    assert_eq!(
        negotiate_codec(CodecPreference::Hevc, CODEC_BIT_AV1, all),
        Codec::Av1
    );
}

#[test]
fn bitrate_mbps_validation() {
    assert_eq!(encode_bitrate_mbps(None).unwrap(), 0);
    assert_eq!(decode_bitrate_mbps(0).unwrap(), None);
    for mbps in [5, 25, 80, 150] {
        assert_eq!(encode_bitrate_mbps(Some(mbps)).unwrap(), mbps);
        assert_eq!(decode_bitrate_mbps(mbps).unwrap(), Some(mbps));
    }
    for bad in [0, 4, 151] {
        assert!(encode_bitrate_mbps(Some(bad)).is_err());
    }
    for bad in [1, 4, 151] {
        assert!(decode_bitrate_mbps(bad).is_err());
    }
}

#[test]
fn format_packet_round_trips_av1_and_hdr() {
    let fmt = Format {
        codec: Codec::Av1,
        width: 3840,
        height: 2160,
        frames_per_second: 120,
        hdr: true,
    };
    let packet = format_packet(fmt, true);
    let (parsed, can_control) = parse_format(packet).unwrap();
    assert_eq!(parsed, fmt);
    assert!(can_control);

    let mut bad_hdr = packet;
    bad_hdr[8] = 2;
    assert!(parse_format(bad_hdr).is_err());
}

#[tokio::test]
async fn host_rejects_invalid_bitrate_control_wire_packet() -> Result<()> {
    let format = Format {
        codec: Codec::H264,
        width: 1920,
        height: 1080,
        frames_per_second: 60,
        hdr: false,
    };
    let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, true)?;
    let address = host.address()?;
    let ticket = Pairing::parse(&host.pairing_code())?;
    let (mut sender, receiver) = tokio::try_join!(
        async {
            let pending = host.accept_pending().await?;
            pending.start(format).await
        },
        ViewerSession::connect(address, ticket)
    )?;

    let [hi, lo] = 4u16.to_be_bytes();
    write_control(&receiver.control, &[BITRATE_REQUEST, hi, lo]).await?;
    assert!(sender.next_input().await.is_err());
    receiver.disconnect().await;
    Ok(())
}

fn synthetic_h264_frame(keyframe: bool, total_len: usize, seed: u8) -> Vec<u8> {
    let prefix: &[u8] = if keyframe {
        &[
            0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3,
        ]
    } else {
        &[0, 0, 0, 1, 0x41, 4]
    };
    let mut out = Vec::with_capacity(total_len.max(prefix.len()));
    out.extend_from_slice(prefix);
    while out.len() < total_len {
        // Avoid accidental 0x000001 start codes inside synthetic slice body.
        let b = (((out.len() as u32).wrapping_mul(31) + u32::from(seed) * 17) & 0x7f) as u8 | 0x80;
        out.push(b);
    }
    out
}

#[test]
fn bind_udp_socket_configures_buffers_and_binds_loopback() {
    let std_sock = bind_udp_socket("127.0.0.1:0".parse().unwrap()).unwrap();
    assert!(std_sock.local_addr().unwrap().port() != 0);
    let sock = Socket::from(std_sock);
    assert!(sock.send_buffer_size().unwrap() > 0);
    assert!(sock.recv_buffer_size().unwrap() > 0);
}

#[tokio::test]
async fn quic_delivers_normal_frames_over_fec_datagrams() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let format = Format {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            frames_per_second: 60,
            hdr: false,
        };
        let host = SessionHost::bind("127.0.0.1:0".parse()?, format, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let (mut sender, mut receiver) =
            tokio::try_join!(host.accept(), ViewerSession::connect(address, ticket))?;

        assert!(sender.video.connection.max_datagram_size().is_some());
        let dgrams_before = sender.video.connection.stats().frame_tx.datagram;

        let frames: Vec<(u64, u64, u32, Vec<u8>)> = vec![
            (0, 1_500, 1_100, synthetic_h264_frame(true, 42_000, 1)),
            (1, 3_000, 1_200, synthetic_h264_frame(false, 18_500, 2)),
            (2, 4_500, 1_300, synthetic_h264_frame(false, 9_123, 3)),
        ];
        for (seq, ts, lat, bytes) in &frames {
            let unit = AccessUnit::from_annex_b(Codec::H264, *seq, bytes.clone().into())?
                .with_rtp_timestamp(*ts)
                .with_host_latency_us(*lat);
            assert!(sender.send(&unit).await?);
            let got = receiver.recv().await?.expect("expected frame");
            assert_eq!(got.rtp_timestamp, *ts);
            assert_eq!(got.host_latency_us, *lat);
            assert_eq!(&*got.bytes, bytes.as_slice());
        }

        let dgrams_after = sender.video.connection.stats().frame_tx.datagram;
        assert!(
            dgrams_after > dgrams_before + 50,
            "expected multi-shard video frames to transmit over QUIC datagrams (before={dgrams_before}, after={dgrams_after})"
        );

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_falls_back_to_uni_stream_for_oversized_frame() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let format = Format {
            codec: Codec::H264,
            width: 3840,
            height: 2160,
            frames_per_second: 60,
            hdr: false,
        };
        let host = SessionHost::bind("127.0.0.1:0".parse()?, format, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let (mut sender, mut receiver) =
            tokio::try_join!(host.accept(), ViewerSession::connect(address, ticket))?;

        // 320 KiB keyframe exceeds MAX_DATA_SHARDS (~180 * ~1150 B) and must fall back
        // seamlessly to a unidirectional QUIC stream.
        let giant_idr = synthetic_h264_frame(true, 320 * 1024, 11);
        let unit0 = AccessUnit::from_annex_b(Codec::H264, 0, giant_idr.clone().into())?
            .with_rtp_timestamp(90_000)
            .with_host_latency_us(2_500);
        let dgrams_before = sender.video.connection.stats().frame_tx.datagram;
        assert!(sender.send(&unit0).await?);
        let got0 = receiver.recv().await?.expect("oversized keyframe");
        assert_eq!(got0.rtp_timestamp, 90_000);
        assert_eq!(got0.host_latency_us, 2_500);
        assert_eq!(&*got0.bytes, giant_idr.as_slice());
        assert_eq!(
            sender.video.connection.stats().frame_tx.datagram,
            dgrams_before,
            "oversized frame should use uni-stream without sending video datagrams"
        );

        // Follow immediately with a normal delta frame over FEC datagrams to confirm
        // hybrid uni-stream + datagram continuity in the viewer reorderer.
        let delta = synthetic_h264_frame(false, 12_000, 12);
        let unit1 = AccessUnit::from_annex_b(Codec::H264, 1, delta.clone().into())?
            .with_rtp_timestamp(91_500)
            .with_host_latency_us(900);
        assert!(sender.send(&unit1).await?);
        let got1 = receiver.recv().await?.expect("delta frame after giant IDR");
        assert_eq!(got1.rtp_timestamp, 91_500);
        assert_eq!(&*got1.bytes, delta.as_slice());
        assert!(sender.video.connection.stats().frame_tx.datagram > dgrams_before);

        receiver.disconnect().await;
        assert!(sender.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_recovers_simulated_fec_shard_loss_in_0rtt_without_keyframe_request() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let format = Format {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            frames_per_second: 60,
            hdr: false,
        };
        let host = SessionHost::bind("127.0.0.1:0".parse()?, format, false)?;
        let address = host.address()?;
        let ticket = Pairing::parse(&host.pairing_code())?;
        let (mut sender, mut receiver) =
            tokio::try_join!(host.accept(), ViewerSession::connect(address, ticket))?;

        let max_dgram = sender
            .video
            .connection
            .max_datagram_size()
            .expect("datagrams enabled");

        // Frame 0 (keyframe, ~30 KiB): drop ALL m first data shards; RS recovers in 0-RTT!
        let frame0 = synthetic_h264_frame(true, 30_000, 21);
        let pkts0 = fec::encode_frame_datagrams(0, 10_000, 800, &frame0, max_dgram)?
            .expect("frame0 fits in FEC datagrams");
        let k0 = pkts0[0][25] as usize;
        let m0 = pkts0[0][26] as usize;
        assert!(k0 >= 20 && m0 >= 4);
        for pkt in pkts0.into_iter().skip(m0) {
            sender.video.connection.send_datagram(pkt)?;
        }
        let got0 = receiver.recv().await?.expect("recovered keyframe");
        assert_eq!(got0.rtp_timestamp, 10_000);
        assert_eq!(got0.host_latency_us, 800);
        assert_eq!(&*got0.bytes, frame0.as_slice());

        // Frame 1 (delta, ~30 KiB): drop m scattered data shards and reverse shard order.
        let frame1 = synthetic_h264_frame(false, 30_000, 22);
        let pkts1 = fec::encode_frame_datagrams(1, 11_500, 900, &frame1, max_dgram)?
            .expect("frame1 fits in FEC datagrams");
        let m1 = pkts1[0][26] as usize;
        let mut kept1: Vec<_> = pkts1
            .into_iter()
            .enumerate()
            .filter(|(idx, _)| !(*idx < m1 * 2 && idx % 2 == 0))
            .map(|(_, p)| p)
            .collect();
        kept1.reverse();
        for pkt in kept1 {
            sender.video.connection.send_datagram(pkt)?;
        }
        let got1 = receiver.recv().await?.expect("recovered delta frame");
        assert_eq!(got1.rtp_timestamp, 11_500);
        assert_eq!(got1.host_latency_us, 900);
        assert_eq!(&*got1.bytes, frame1.as_slice());

        // Neither recoverable loss should have triggered a KEYFRAME_REQUEST.
        assert!(
            timeout(Duration::from_millis(140), sender.next_input())
                .await
                .is_err(),
            "0-RTT RS FEC recovery must not trigger a keyframe request"
        );

        // Frame 2: drop m + 1 shards (genuinely unrecoverable) -> MUST trigger keyframe request
        // once the FEC reassembler expires the incomplete frame.
        let frame2 = synthetic_h264_frame(false, 20_000, 23);
        let pkts2 = fec::encode_frame_datagrams(2, 13_000, 950, &frame2, max_dgram)?
            .expect("frame2 fits in FEC datagrams");
        let k2 = pkts2[0][25] as usize;
        for pkt in pkts2.into_iter().take(k2 - 1) {
            sender.video.connection.send_datagram(pkt)?;
        }
        assert!(
            matches!(
                timeout(Duration::from_millis(400), sender.next_input()).await?,
                Ok(Some(HostInput::Keyframe))
            ),
            "unrecoverable FEC frame must trigger a keyframe request"
        );

        receiver.disconnect().await;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test]
async fn quic_host_supports_concurrent_viewers_with_per_viewer_control_override() -> Result<()> {
    timeout(Duration::from_secs(6), async {
        let format = Format {
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            frames_per_second: 60,
            hdr: false,
        };
        let host = SessionHost::bind_dynamic("127.0.0.1:0".parse()?, false)?;
        let address = host.address()?;
        let ticket1 = Pairing::parse(&host.pairing_code())?;
        let ticket2 = Pairing::parse(&host.pairing_code())?;

        let (mut host1, mut viewer1) = tokio::try_join!(
            async {
                host.accept_pending()
                    .await?
                    .start_with_options(format, Some(false), None)
                    .await
            },
            ViewerSession::connect(address, ticket1)
        )?;
        assert!(viewer1.can_control());

        let (mut host2, mut viewer2) = tokio::try_join!(
            async {
                host.accept_pending()
                    .await?
                    .start_with_options(format, Some(true), None)
                    .await
            },
            ViewerSession::connect(address, ticket2)
        )?;
        assert!(!viewer2.can_control());

        let keyframe = synthetic_h264_frame(true, 16_000, 31);
        let unit = AccessUnit::from_annex_b(Codec::H264, 0, keyframe.clone().into())?
            .with_rtp_timestamp(90_000)
            .with_host_latency_us(500);
        assert!(host1.video.send(&unit).await?);
        assert!(host2.video.send(&unit).await?);

        let (got1, got2) = tokio::try_join!(viewer1.recv(), viewer2.recv())?;
        assert_eq!(&*got1.expect("viewer1 frame").bytes, keyframe.as_slice());
        assert_eq!(&*got2.expect("viewer2 frame").bytes, keyframe.as_slice());

        viewer1.disconnect().await;
        viewer2.disconnect().await;
        assert!(host1.next_input().await?.is_none());
        assert!(host2.next_input().await?.is_none());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
