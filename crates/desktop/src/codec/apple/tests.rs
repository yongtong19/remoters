//! Unit tests for VideoToolbox rate control, PTS clamping, encoding, and AV1 decode setup.

use core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
use remoters_core::video::{Codec, Format, obu};
use screencapturekit::cv::CVPixelBuffer;

use super::{
    decoder::{DecodeSession, create_av1_format_description, supports_av1_hardware_decoder},
    encoder::HardwareEncoder,
    rate_control::{DATA_RATE_LIMIT_WINDOW_SECS, PtsClamper, data_rate_limits},
};

#[test]
fn data_rate_limits_uses_tight_burst_window_and_scales_with_bitrate() {
    assert_eq!(
        data_rate_limits(5_000_000),
        (187_500, DATA_RATE_LIMIT_WINDOW_SECS)
    );
    assert_eq!(
        data_rate_limits(20_000_000),
        (750_000, DATA_RATE_LIMIT_WINDOW_SECS)
    );
    assert_eq!(
        data_rate_limits(35_000_000),
        (1_312_500, DATA_RATE_LIMIT_WINDOW_SECS)
    );
    assert_eq!(
        data_rate_limits(50_000_000),
        (1_875_000, DATA_RATE_LIMIT_WINDOW_SECS)
    );
    assert_eq!(
        data_rate_limits(150_000_000),
        (5_625_000, DATA_RATE_LIMIT_WINDOW_SECS)
    );

    let (min_bytes, window) = data_rate_limits(0);
    assert_eq!(min_bytes, 1);
    assert!((0.25..=0.5).contains(&window));
}

#[test]
fn pts_clamper_preserves_normal_cadence_and_bounds_static_gaps() {
    let mut clamper = PtsClamper::new(60);
    assert_eq!(clamper.max_step_ticks(), 3_000);

    // Initial frame and steady 60fps cadence pass through untouched.
    assert_eq!(clamper.next(0), 0);
    assert_eq!(clamper.next(1_500), 1_500);
    assert_eq!(clamper.next(3_000), 3_000);

    // A 10-second static screen pause (+900,000 ticks) clamps to 2 * frame_duration (+3,000).
    assert_eq!(clamper.next(903_000), 6_000);

    // The immediately following 60fps frame resumes exact 1x cadence (+1,500), not +3,000.
    assert_eq!(clamper.next(904_500), 7_500);

    // Duplicate or out-of-order raw timestamps still advance monotonically by 1 tick.
    assert_eq!(clamper.next(904_500), 7_501);
    assert_eq!(clamper.next(904_000), 7_502);

    // High-refresh 120fps stream bounds pauses and 200ms keepalives to 1,500 ticks (16.7ms).
    let mut fast = PtsClamper::new(120);
    assert_eq!(fast.max_step_ticks(), 1_500);
    assert_eq!(fast.next(10_000), 10_000);
    assert_eq!(fast.next(28_000), 11_500); // 200ms keepalive (+18,000 ticks)
    assert_eq!(fast.next(28_750), 12_250); // next 120fps frame (+750 ticks)
}

#[test]
fn hardware_encoder_applies_data_rate_limits_and_encodes_with_clamped_pts() {
    let surface = CVPixelBuffer::create(320, 180, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)
        .expect("NV12 pixel buffer");

    for codec in [Codec::H264, Codec::Hevc] {
        let format = Format {
            codec,
            width: 320,
            height: 180,
            frames_per_second: 60,
            hdr: false,
        };
        let mut encoder = HardwareEncoder::new(format, 10_000_000)
            .expect("macOS VideoToolbox hardware encoder should initialize");
        assert!(
            !encoder.data_rate_limits_warned(),
            "VideoToolbox should accept DataRateLimits for {codec:?}"
        );

        let frame0 = encoder
            .encode(&surface, 0, true)
            .expect("encode initial keyframe")
            .expect("initial keyframe output");
        assert!(!frame0.is_empty());

        // Simulate a 5-second static screen jump in raw 90 kHz ticks.
        let frame1 = encoder
            .encode(&surface, 450_000, false)
            .expect("encode post-static frame")
            .expect("post-static frame output");
        assert!(!frame1.is_empty());

        encoder
            .set_bitrate(20_000_000)
            .expect("live bitrate + DataRateLimits update");
        assert!(
            !encoder.data_rate_limits_warned(),
            "VideoToolbox should accept live DataRateLimits update for {codec:?}"
        );

        let frame2 = encoder
            .encode(&surface, 451_500, false)
            .expect("encode after bitrate change")
            .expect("post-bitrate frame output");
        assert!(!frame2.is_empty());
    }
}

#[test]
fn av1_hardware_decode_probe_and_av1c_format_work() {
    let supported = supports_av1_hardware_decoder();
    let build_seq_obu = |high_bitdepth: bool| -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        let push_bits = |bits: &mut Vec<bool>, val: u32, n: u8| {
            for shift in (0..n).rev() {
                bits.push(((val >> shift) & 1) != 0);
            }
        };
        push_bits(&mut bits, 0, 3); // seq_profile = 0 (Main)
        bits.push(false); // still_picture
        bits.push(false); // reduced_still_picture_header
        bits.push(false); // timing_info_present_flag
        bits.push(false); // initial_display_delay_present_flag
        push_bits(&mut bits, 0, 5); // operating_points_cnt_minus_1 = 0
        push_bits(&mut bits, 0, 12); // operating_point_idc[0]
        push_bits(&mut bits, 12, 5); // seq_level_idx[0] = 5.0
        bits.push(false); // seq_tier[0]
        push_bits(&mut bits, 10, 4); // frame_width_bits_minus_1 = 10 (11 bits)
        push_bits(&mut bits, 10, 4); // frame_height_bits_minus_1 = 10 (11 bits)
        push_bits(&mut bits, 1919, 11); // max_frame_width_minus_1
        push_bits(&mut bits, 1079, 11); // max_frame_height_minus_1
        bits.push(false); // frame_id_numbers_present_flag
        bits.push(true); // use_128x128_superblock
        bits.push(false); // enable_filter_intra
        bits.push(false); // enable_intra_edge_filter
        bits.push(false); // enable_interintra_compound
        bits.push(false); // enable_masked_compound
        bits.push(false); // enable_warped_motion
        bits.push(false); // enable_dual_filter
        bits.push(true); // enable_order_hint
        bits.push(false); // enable_jnt_comp
        bits.push(false); // enable_ref_frame_mvs
        bits.push(true); // seq_choose_screen_content_tools
        bits.push(true); // seq_choose_integer_mv
        push_bits(&mut bits, 6, 3); // order_hint_bits_minus_1
        bits.push(false); // enable_superres
        bits.push(true); // enable_cdef
        bits.push(true); // enable_restoration
        bits.push(high_bitdepth); // high_bitdepth (8-bit or 10-bit)
        bits.push(false); // mono_chrome = false
        bits.push(true); // color_description_present_flag
        if high_bitdepth {
            push_bits(&mut bits, 9, 8); // BT.2020 primaries
            push_bits(&mut bits, 16, 8); // SMPTE ST 2084 PQ
            push_bits(&mut bits, 9, 8); // BT.2020 NCL
        } else {
            push_bits(&mut bits, 1, 8); // BT.709
            push_bits(&mut bits, 1, 8);
            push_bits(&mut bits, 1, 8);
        }
        bits.push(false); // color_range (studio/limited)
        push_bits(&mut bits, 0, 2); // chroma_sample_position
        bits.push(false); // separate_uv_delta_q
        bits.push(false); // film_grain_params_present
        bits.push(true); // trailing_one_bit

        let mut payload = Vec::new();
        for chunk in bits.chunks(8) {
            let mut byte = 0u8;
            for (i, &bit) in chunk.iter().enumerate() {
                if bit {
                    byte |= 1 << (7 - i);
                }
            }
            payload.push(byte);
        }
        let mut obu = vec![0x0a, payload.len() as u8]; // OBU_SEQUENCE_HEADER (1) + obu_has_size_field (1)
        obu.extend_from_slice(&payload);
        obu
    };
    let av1c_sdr = obu::build_av1c(&build_seq_obu(false)).expect("valid 8-bit av1C");
    let av1c_hdr = obu::build_av1c(&build_seq_obu(true)).expect("valid 10-bit av1C");
    create_av1_format_description(&av1c_sdr, 1920, 1080, false).expect("SDR av01 format desc");
    create_av1_format_description(&av1c_hdr, 1920, 1080, true).expect("HDR av01 format desc");
    eprintln!(
        "supports_av1_hardware_decoder() = {supported}, av1C sdr={}B hdr={}B",
        av1c_sdr.len(),
        av1c_hdr.len()
    );
    if supported {
        DecodeSession::new_av1(&av1c_sdr, 1920, 1080, false)
            .expect("M3/M4 hardware AV1 SDR DecodeSession should open cleanly");
        DecodeSession::new_av1(&av1c_hdr, 1920, 1080, true)
            .expect("M3/M4 hardware AV1 HDR10 DecodeSession should open cleanly");
    }
}
