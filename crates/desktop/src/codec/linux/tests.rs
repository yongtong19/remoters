//! Cross-platform and live-Linux unit tests for the Linux codec stack.

use std::{
    io::Cursor,
    time::{Duration, Instant},
};

use remoters_core::video::Codec;

use super::{
    EncoderArgsConfig, LinuxDecodeBackend, LinuxVideoBackend, build_decoder_args,
    build_decoder_args_for, build_encoder_args, build_encoder_args_with_input,
    ivf::{IvfReader, ivf_file_header, ivf_frame_header},
    low_latency_vbv_kbps,
    pipewire::LatestFrameSlot,
    resolve_backend_from_env, rgba_bytes_to_bgra, rgba_to_bgra,
};

#[test]
fn converts_rgba_pixels_to_bgra_for_gpui_render_image() {
    let mut pixels = vec![10, 20, 30, 255, 1, 2, 3, 128];
    rgba_to_bgra(&mut pixels);
    assert_eq!(pixels, vec![30, 20, 10, 255, 3, 2, 1, 128]);

    let copy = rgba_bytes_to_bgra(&[200, 100, 50, 255]);
    assert_eq!(copy, vec![50, 100, 200, 255]);
}

#[test]
fn low_latency_vbv_caps_burst_to_about_two_frames_with_safe_floor() {
    // 20 Mbps @ 60fps -> ~2 frames = 667k (vs old 40000k = 2 seconds!)
    assert_eq!(low_latency_vbv_kbps(20_000_000, 60), 667);
    // 35 Mbps @ 60fps -> 1167k
    assert_eq!(low_latency_vbv_kbps(35_000_000, 60), 1_167);
    // 70 Mbps @ 120fps -> 1167k
    assert_eq!(low_latency_vbv_kbps(70_000_000, 120), 1_167);
    // Low bitrate / high fps safely clamps to 250k floor
    assert_eq!(low_latency_vbv_kbps(5_000_000, 120), 250);
    assert_eq!(low_latency_vbv_kbps(500_000, 60), 250);
}

#[test]
fn resolves_linux_encoder_backend_overrides_and_fallbacks() {
    // Default auto prefers direct NVENC on SDR when available
    assert_eq!(
        resolve_backend_from_env(None, false, true, true),
        LinuxVideoBackend::NvencDirect
    );
    // Auto falls back to FFmpeg NVENC on HDR or when direct NVENC is unavailable
    assert_eq!(
        resolve_backend_from_env(None, true, true, true),
        LinuxVideoBackend::Nvenc
    );
    assert_eq!(
        resolve_backend_from_env(None, false, false, true),
        LinuxVideoBackend::Nvenc
    );
    assert_eq!(
        resolve_backend_from_env(None, false, false, false),
        LinuxVideoBackend::Software
    );
    // Explicit overrides
    assert_eq!(
        resolve_backend_from_env(Some("nvenc-direct"), false, true, true),
        LinuxVideoBackend::NvencDirect
    );
    assert_eq!(
        resolve_backend_from_env(Some("ffmpeg"), false, true, true),
        LinuxVideoBackend::Nvenc
    );
    assert_eq!(
        resolve_backend_from_env(Some("ffmpeg"), false, true, false),
        LinuxVideoBackend::Software
    );
    assert_eq!(
        resolve_backend_from_env(Some("vaapi"), false, true, true),
        LinuxVideoBackend::Vaapi
    );
    assert_eq!(
        resolve_backend_from_env(Some("software"), false, true, true),
        LinuxVideoBackend::Software
    );
}

#[test]
fn latest_frame_slot_drops_stale_frames_and_preserves_capture_time() {
    let slot = LatestFrameSlot::new();
    let t1 = Instant::now();
    let mut f1 = vec![1u8; 16];
    slot.publish_swap(&mut f1, 2, 2, t1);

    let t2 = Instant::now();
    let mut f2 = vec![2u8; 16];
    slot.publish_swap(&mut f2, 2, 2, t2);

    let mut last_seq = 0u64;
    let mut dst = Vec::new();
    let meta = slot
        .take_latest(&mut dst, &mut last_seq, Duration::from_millis(50))
        .expect("expected latest frame");
    assert_eq!(meta.seq, 2);
    assert_eq!(meta.width, 2);
    assert_eq!(meta.height, 2);
    assert_eq!(meta.captured_at, t2);
    assert_eq!(dst, vec![2u8; 16]);
    assert_eq!(last_seq, 2);

    // No newer frame published -> returns None on close.
    slot.close();
    assert!(
        slot.take_latest(&mut dst, &mut last_seq, Duration::from_millis(50))
            .is_none()
    );
}

#[test]
fn builds_ffmpeg_encoder_args_for_software_vaapi_av1_and_hdr() {
    let h264 = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::H264,
        input_width: 1920,
        input_height: 1080,
        output_width: 1920,
        output_height: 1080,
        fps: 60,
        bitrate_bps: 20_000_000,
        hdr: false,
        backend: LinuxVideoBackend::Software,
        input_pix_fmt: "rgba",
    });
    assert!(h264.windows(2).any(|w| w == ["-c:v", "libx264"]));
    assert!(h264.windows(2).any(|w| w == ["-preset", "ultrafast"]));
    assert!(h264.windows(2).any(|w| w == ["-tune", "zerolatency"]));
    assert!(h264.windows(2).any(|w| w == ["-b:v", "20000k"]));
    assert!(h264.windows(2).any(|w| w == ["-maxrate", "20000k"]));
    assert!(h264.windows(2).any(|w| w == ["-bufsize", "667k"]));
    assert!(h264.windows(2).any(|w| w == ["-flush_packets", "1"]));
    assert!(h264.windows(2).any(|w| w == ["-f", "h264"]));
    assert!(!h264.iter().any(|arg| arg == "-vf"));
    assert!(!h264.iter().any(|arg| arg == "-intra-refresh"));

    let hevc_scaled = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::Hevc,
        input_width: 5120,
        input_height: 2880,
        output_width: 3840,
        output_height: 2160,
        fps: 120,
        bitrate_bps: 80_000_000,
        hdr: false,
        backend: LinuxVideoBackend::Software,
        input_pix_fmt: "rgba",
    });
    assert!(hevc_scaled.windows(2).any(|w| w == ["-c:v", "libx265"]));
    assert!(
        hevc_scaled
            .windows(2)
            .any(|w| w == ["-vf", "scale=3840:2160"])
    );
    assert!(hevc_scaled.windows(2).any(|w| w == ["-r", "120"]));
    assert!(hevc_scaled.windows(2).any(|w| w == ["-b:v", "80000k"]));
    assert!(hevc_scaled.windows(2).any(|w| w == ["-bufsize", "1334k"]));
    assert!(hevc_scaled.windows(2).any(|w| w == ["-f", "hevc"]));

    let av1_sw_hdr = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::Av1,
        input_width: 3840,
        input_height: 2160,
        output_width: 3840,
        output_height: 2160,
        fps: 60,
        bitrate_bps: 45_000_000,
        hdr: true,
        backend: LinuxVideoBackend::Software,
        input_pix_fmt: "rgba",
    });
    assert!(av1_sw_hdr.windows(2).any(|w| w == ["-c:v", "libsvtav1"]));
    assert!(
        av1_sw_hdr
            .windows(2)
            .any(|w| w == ["-pix_fmt", "yuv420p10le"])
    );
    assert!(
        av1_sw_hdr
            .windows(2)
            .any(|w| w == ["-color_primaries", "bt2020"])
    );
    assert!(
        av1_sw_hdr
            .windows(2)
            .any(|w| w == ["-color_trc", "smpte2084"])
    );
    assert!(
        av1_sw_hdr
            .windows(2)
            .any(|w| w == ["-colorspace", "bt2020nc"])
    );
    assert!(av1_sw_hdr.windows(2).any(|w| w == ["-f", "ivf"]));

    let vaapi_hdr = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::Hevc,
        input_width: 2560,
        input_height: 1440,
        output_width: 2560,
        output_height: 1440,
        fps: 60,
        bitrate_bps: 35_000_000,
        hdr: true,
        backend: LinuxVideoBackend::Vaapi,
        input_pix_fmt: "rgba",
    });
    assert!(
        vaapi_hdr
            .windows(2)
            .any(|w| w == ["-vaapi_device", "/dev/dri/renderD128"])
    );
    assert!(vaapi_hdr.windows(2).any(|w| w == ["-c:v", "hevc_vaapi"]));
    assert!(vaapi_hdr.windows(2).any(|w| w == ["-profile:v", "main10"]));
    assert!(
        vaapi_hdr
            .windows(2)
            .any(|w| w == ["-vf", "format=p010le,hwupload"])
    );
    assert!(
        vaapi_hdr
            .windows(2)
            .any(|w| w == ["-bsf:v", "hevc_metadata=aud=insert"])
    );
    assert!(vaapi_hdr.windows(2).any(|w| w == ["-bufsize", "1167k"]));

    let av1_vaapi = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::Av1,
        input_width: 1920,
        input_height: 1080,
        output_width: 1920,
        output_height: 1080,
        fps: 60,
        bitrate_bps: 20_000_000,
        hdr: false,
        backend: LinuxVideoBackend::Vaapi,
        input_pix_fmt: "rgba",
    });
    assert!(av1_vaapi.windows(2).any(|w| w == ["-c:v", "av1_vaapi"]));
    assert!(
        av1_vaapi
            .windows(2)
            .any(|w| w == ["-vf", "format=nv12,hwupload"])
    );
    assert!(av1_vaapi.windows(2).any(|w| w == ["-f", "ivf"]));

    let av1_nvenc_hdr = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::Av1,
        input_width: 3840,
        input_height: 2160,
        output_width: 3840,
        output_height: 2160,
        fps: 120,
        bitrate_bps: 70_000_000,
        hdr: true,
        backend: LinuxVideoBackend::Nvenc,
        input_pix_fmt: "rgba",
    });
    assert!(av1_nvenc_hdr.windows(2).any(|w| w == ["-c:v", "av1_nvenc"]));
    assert!(
        av1_nvenc_hdr
            .windows(2)
            .any(|w| w == ["-pix_fmt", "p010le"])
    );
    assert!(av1_nvenc_hdr.windows(2).any(|w| w == ["-tune", "ull"]));
    assert!(av1_nvenc_hdr.windows(2).any(|w| w == ["-rc", "cbr"]));
    assert!(av1_nvenc_hdr.windows(2).any(|w| w == ["-bufsize", "1167k"]));
    assert!(av1_nvenc_hdr.windows(2).any(|w| w == ["-f", "ivf"]));
    assert!(!av1_nvenc_hdr.iter().any(|arg| arg == "-intra-refresh"));

    let hevc_nvenc_hdr = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::Hevc,
        input_width: 2560,
        input_height: 1440,
        output_width: 2560,
        output_height: 1440,
        fps: 60,
        bitrate_bps: 35_000_000,
        hdr: true,
        backend: LinuxVideoBackend::Nvenc,
        input_pix_fmt: "rgba",
    });
    assert!(
        hevc_nvenc_hdr
            .windows(2)
            .any(|w| w == ["-c:v", "hevc_nvenc"])
    );
    assert!(
        hevc_nvenc_hdr
            .windows(2)
            .any(|w| w == ["-profile:v", "main10"])
    );
    assert!(hevc_nvenc_hdr.windows(2).any(|w| w == ["-aud", "1"]));
    assert!(
        hevc_nvenc_hdr
            .windows(2)
            .any(|w| w == ["-intra-refresh", "1"])
    );
    assert!(hevc_nvenc_hdr.windows(2).any(|w| w == ["-forced-idr", "1"]));
    assert!(hevc_nvenc_hdr.windows(2).any(|w| w == ["-rc", "cbr"]));
    assert!(
        hevc_nvenc_hdr
            .windows(2)
            .any(|w| w == ["-bufsize", "1167k"])
    );
    assert!(hevc_nvenc_hdr.windows(2).any(|w| w == ["-f", "hevc"]));

    let h264_nvenc = build_encoder_args(&EncoderArgsConfig {
        codec: Codec::H264,
        input_width: 1920,
        input_height: 1080,
        output_width: 1920,
        output_height: 1080,
        fps: 60,
        bitrate_bps: 20_000_000,
        hdr: false,
        backend: LinuxVideoBackend::Nvenc,
        input_pix_fmt: "bgr0",
    });
    assert!(h264_nvenc.windows(2).any(|w| w == ["-c:v", "h264_nvenc"]));
    assert!(h264_nvenc.windows(2).any(|w| w == ["-intra-refresh", "1"]));
    assert!(h264_nvenc.windows(2).any(|w| w == ["-forced-idr", "1"]));
    assert!(h264_nvenc.windows(2).any(|w| w == ["-bufsize", "667k"]));

    let hevc_nvenc_x11grab = build_encoder_args_with_input(
        &EncoderArgsConfig {
            codec: Codec::Hevc,
            input_width: 2560,
            input_height: 1440,
            output_width: 2560,
            output_height: 1440,
            fps: 60,
            bitrate_bps: 35_000_000,
            hdr: false,
            backend: LinuxVideoBackend::Nvenc,
            input_pix_fmt: "bgr0",
        },
        Some(":0+0,0"),
    );
    assert!(
        hevc_nvenc_x11grab
            .windows(2)
            .any(|w| w == ["-f", "x11grab"])
    );
    assert!(hevc_nvenc_x11grab.windows(2).any(|w| w == ["-i", ":0+0,0"]));
    assert!(
        hevc_nvenc_x11grab
            .windows(2)
            .any(|w| w == ["-pix_fmt", "bgr0"])
    );
    assert!(
        hevc_nvenc_x11grab
            .windows(2)
            .any(|w| w == ["-flush_packets", "1"])
    );

    let av1_cuvid = build_decoder_args_for(Codec::Av1, LinuxDecodeBackend::Cuvid);
    assert!(av1_cuvid.windows(2).any(|w| w == ["-c:v", "av1_cuvid"]));
    assert!(av1_cuvid.windows(2).any(|w| w == ["-f", "ivf"]));
}

#[test]
fn builds_ffmpeg_decoder_args_for_h264_hevc_and_av1() {
    let h264 = build_decoder_args(Codec::H264);
    assert!(h264.windows(2).any(|w| w == ["-flags", "low_delay"]));
    assert!(h264.windows(2).any(|w| w == ["-f", "h264"]));
    assert!(h264.windows(2).any(|w| w == ["-pix_fmt", "bgra"]));
    assert!(h264.windows(2).any(|w| w == ["-f", "rawvideo"]));

    let hevc = build_decoder_args(Codec::Hevc);
    assert!(hevc.windows(2).any(|w| w == ["-f", "hevc"]));
    assert!(hevc.windows(2).any(|w| w == ["-pix_fmt", "bgra"]));

    let av1 = build_decoder_args(Codec::Av1);
    assert!(av1.windows(2).any(|w| w == ["-f", "ivf"]));
    assert!(av1.windows(2).any(|w| w == ["-pix_fmt", "bgra"]));
}

#[test]
fn reads_av1_access_units_from_ivf_stream() {
    // OBU_SEQUENCE_HEADER (type 1, has_size=1, size=2) + OBU_FRAME (type 6, keyframe)
    let seq_obu = [0x0a, 0x02, 0x00, 0x00];
    let key_obu = [0x32, 0x02, 0x00, 0xaa];
    let key_au = [seq_obu.as_slice(), key_obu.as_slice()].concat();
    let delta_au = [0x32, 0x02, 0x20, 0x55];

    let mut stream = Vec::new();
    stream.extend_from_slice(&ivf_file_header(1920, 1080, 60));
    stream.extend_from_slice(&ivf_frame_header(key_au.len() as u32, 0));
    stream.extend_from_slice(&key_au);
    stream.extend_from_slice(&ivf_frame_header(delta_au.len() as u32, 1));
    stream.extend_from_slice(&delta_au);

    let mut reader = IvfReader::new(Cursor::new(stream));
    let u0 = reader.next_unit().unwrap().unwrap();
    assert_eq!(u0.sequence, 0);
    assert!(u0.is_random_access());
    assert_eq!(u0.bytes().as_ref(), key_au.as_slice());

    let u1 = reader.next_unit().unwrap().unwrap();
    assert_eq!(u1.sequence, 1);
    assert!(!u1.is_random_access());
    assert_eq!(u1.bytes().as_ref(), delta_au.as_slice());

    assert!(reader.next_unit().unwrap().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_nvidia_hardware_codec_probes_report_cleanly() {
    let av1_enc = super::probe_av1_encoder();
    let hevc_enc = super::probe_hevc_encoder();
    let hdr_enc = super::probe_hdr_encoder();
    let av1_dec = super::probe_av1_decoder();
    let hevc_dec = super::probe_hevc_decoder();
    let direct_h264 = super::nvenc::probe_direct_nvenc(Codec::H264);
    let direct_hevc = super::nvenc::probe_direct_nvenc(Codec::Hevc);
    let direct_av1 = super::nvenc::probe_direct_nvenc(Codec::Av1);
    eprintln!(
        "Linux codec probe: av1_enc={av1_enc} hevc_enc={hevc_enc} hdr_enc={hdr_enc} \
         av1_dec={av1_dec} hevc_dec={hevc_dec} direct_nvenc=(h264={direct_h264}, hevc={direct_hevc}, av1={direct_av1})"
    );
    if super::ffmpeg_args::probe_cuvid_codec(Codec::Av1) {
        assert!(super::ffmpeg_args::probe_nvenc_codec(Codec::H264, false));
        assert!(super::ffmpeg_args::probe_nvenc_codec(Codec::Hevc, true));
        assert!(super::ffmpeg_args::probe_nvenc_codec(Codec::Av1, true));
        assert!(direct_h264 && direct_hevc && direct_av1);
        assert!(av1_enc && hevc_enc && hdr_enc && av1_dec && hevc_dec);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_direct_nvenc_encodes_live_frames_when_gpu_available() {
    use remoters_core::video::AccessUnit;

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        if !super::nvenc::probe_direct_nvenc(codec) {
            continue;
        }
        let mut enc =
            super::nvenc::DirectNvencEncoder::new(codec, 640, 360, 60, 10_000_000, "bgr0")
                .expect("direct NVENC init should succeed");
        let frame = vec![0x40u8; 640 * 360 * 4];
        let au0 = enc
            .encode_frame(&frame, false)
            .expect("initial NVENC frame should encode");
        let parsed0 = AccessUnit::from_annex_b(codec, 0, au0.into())
            .expect("initial direct NVENC frame should parse as AccessUnit");
        assert!(parsed0.is_random_access(), "{codec:?} frame 0 must be IDR");

        let au1 = enc
            .encode_frame(&frame, false)
            .expect("delta NVENC frame should encode");
        let _parsed1 = AccessUnit::from_annex_b(codec, 1, au1.into())
            .expect("delta direct NVENC frame should parse as AccessUnit");

        enc.set_bitrate(20_000_000)
            .expect("live NVENC bitrate reconfigure should succeed");

        let au2 = enc
            .encode_frame(&frame, true)
            .expect("forced-IDR NVENC frame should encode");
        let parsed2 = AccessUnit::from_annex_b(codec, 2, au2.into())
            .expect("forced-IDR direct NVENC frame should parse as AccessUnit");
        assert!(
            parsed2.is_random_access(),
            "{codec:?} forced keyframe must be random-access"
        );
    }
}
