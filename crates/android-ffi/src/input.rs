use anyhow::{Context, Result, bail};
use remoters_core::{
    Button, GamepadState, Input, Point,
    session::{CodecPreference, DisplayPreference, StreamPreference},
    video::{Codec, nal_units, obu},
};

pub fn build_stream_preference(
    width: i32,
    height: i32,
    codec_pref: i32,
    supported_codecs: i32,
    hdr: bool,
    max_fps: i32,
    bitrate_mbps: i32,
) -> Result<StreamPreference> {
    let codec = match codec_pref {
        0 => CodecPreference::Auto,
        1 => CodecPreference::H264,
        2 => CodecPreference::Hevc,
        3 => CodecPreference::Av1,
        other => bail!("Unsupported codec preference: {other}."),
    };

    let supported_mask =
        u8::try_from(supported_codecs).context("Invalid supported codecs bitmask.")?;

    let max_fps_cap = if max_fps <= 0 {
        None
    } else {
        let fps = u16::try_from(max_fps).context("Invalid max FPS preference.")?;
        Some(fps)
    };

    let display = if width > 0 && height > 0 {
        let w = u32::try_from(width).context("Invalid display width.")?;
        let h = u32::try_from(height).context("Invalid display height.")?;
        let fps = max_fps_cap.unwrap_or(60).clamp(1, 120);
        Some(DisplayPreference::normalized(w, h, fps)?)
    } else {
        None
    };

    let target_bitrate_mbps = if bitrate_mbps <= 0 {
        None
    } else {
        let mbps = u16::try_from(bitrate_mbps).context("Invalid bitrate preference.")?;
        Some(mbps)
    };

    let preference = StreamPreference::new(display, codec, max_fps_cap, target_bitrate_mbps)?
        .with_supported_codecs(supported_mask)
        .with_hdr(hdr);
    preference.validate()?;
    Ok(preference)
}

pub fn map_bitrate_mbps(bitrate_mbps: i32) -> Option<u16> {
    if bitrate_mbps <= 0 {
        None
    } else {
        Some((bitrate_mbps.min(u16::MAX as i32) as u16).clamp(5, 150))
    }
}

pub fn map_pointer_button(button: i32) -> Option<Button> {
    match button {
        0 => Some(Button::Left),
        1 => Some(Button::Right),
        2 => Some(Button::Middle),
        _ => None,
    }
}

pub fn clamp_point(x: i32, y: i32) -> Point {
    Point {
        x: x.clamp(0, i32::from(u16::MAX)) as u16,
        y: y.clamp(0, i32::from(u16::MAX)) as u16,
    }
}

pub fn map_scroll_input(x: i32, y: i32, vertical: i32, horizontal: i32) -> Input {
    Input::Scroll {
        at: clamp_point(x, y),
        vertical: vertical.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        horizontal: horizontal.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
    }
}

pub fn map_key_input(vk_code: i32, down: bool) -> Option<Input> {
    if !(1..=254).contains(&vk_code) {
        return None;
    }
    Some(Input::Key {
        code: vk_code as u16,
        down,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn map_gamepad_input(
    slot: i32,
    connected: bool,
    buttons: i32,
    lt: i32,
    rt: i32,
    lx: i32,
    ly: i32,
    rx: i32,
    ry: i32,
) -> Option<Input> {
    if !(0..=3).contains(&slot) {
        return None;
    }
    Some(Input::Gamepad(GamepadState {
        slot: slot as u8,
        connected,
        buttons: (buttons as u32 & 0xFFFF) as u16,
        left_trigger: lt.clamp(0, 255) as u8,
        right_trigger: rt.clamp(0, 255) as u8,
        left_x: lx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        left_y: ly.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        right_x: rx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        right_y: ry.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
    }))
}

pub fn codec_to_int(codec: Codec) -> i32 {
    match codec {
        Codec::H264 => 0,
        Codec::Hevc => 1,
        Codec::Av1 => 2,
    }
}

pub fn codec_label(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "H.264",
        Codec::Hevc => "HEVC",
        Codec::Av1 => "AV1",
    }
}

pub fn is_keyframe(codec: Codec, bytes: &[u8]) -> bool {
    if codec == Codec::Av1 {
        return obu::inspect_av1(bytes).unwrap_or(false);
    }
    let mut has_idr = false;
    let mut params = 0u8;
    for nal in nal_units(bytes) {
        let Some(&header) = nal.first() else {
            continue;
        };
        match codec {
            Codec::H264 => match header & 0x1f {
                7 => params |= 1,
                8 => params |= 2,
                5 => has_idr = true,
                _ => {}
            },
            Codec::Hevc => match (header >> 1) & 0x3f {
                32 => params |= 1,
                33 => params |= 2,
                34 => params |= 4,
                19 | 20 => has_idr = true,
                _ => {}
            },
            Codec::Av1 => unreachable!(),
        }
    }
    has_idr
        && params
            == match codec {
                Codec::H264 => 3,
                Codec::Hevc => 7,
                Codec::Av1 => unreachable!(),
            }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_preference_and_bitrate_mapping() {
        let pref = build_stream_preference(0, 0, 0, 0, false, 0, 0).unwrap();
        assert_eq!(pref, StreamPreference::default());

        let pref_hevc = build_stream_preference(2400, 1080, 2, 0b011, false, 90, 40).unwrap();
        assert_eq!(pref_hevc.codec, CodecPreference::Hevc);
        assert_eq!(pref_hevc.supported_codecs, 0b011);
        assert!(!pref_hevc.hdr);
        assert_eq!(pref_hevc.max_fps, Some(90));
        assert_eq!(pref_hevc.target_bitrate_mbps, Some(40));
        assert_eq!(
            pref_hevc.display,
            Some(DisplayPreference::new(2400, 1080, 90).unwrap())
        );

        let pref_av1_hdr = build_stream_preference(2400, 1080, 3, 0b111, true, 120, 80).unwrap();
        assert_eq!(pref_av1_hdr.codec, CodecPreference::Av1);
        assert_eq!(pref_av1_hdr.supported_codecs, 0b111);
        assert!(pref_av1_hdr.hdr);
        assert_eq!(pref_av1_hdr.max_fps, Some(120));
        assert_eq!(pref_av1_hdr.target_bitrate_mbps, Some(80));

        assert!(build_stream_preference(1920, 1080, 99, 0, false, 60, 25).is_err());
        assert!(build_stream_preference(1920, 1080, 3, 0x80, false, 60, 25).is_err());
        assert!(build_stream_preference(1920, 1080, 3, -1, false, 60, 25).is_err());
        assert!(build_stream_preference(1920, 1080, 1, 0, false, 15, 25).is_err());
        assert!(build_stream_preference(1920, 1080, 1, 0, false, 60, 200).is_err());

        assert_eq!(map_bitrate_mbps(0), None);
        assert_eq!(map_bitrate_mbps(-5), None);
        assert_eq!(map_bitrate_mbps(2), Some(5));
        assert_eq!(map_bitrate_mbps(50), Some(50));
        assert_eq!(map_bitrate_mbps(500), Some(150));
    }

    #[test]
    fn stream_preference_preserves_non_16_by_9_native_phone_resolutions() {
        // 22:10 phone (2640x1200) with Auto FPS -> preserves exact aspect ratio at 60 FPS default
        let pref_2640 = build_stream_preference(2640, 1200, 0, 0b111, false, 0, 0).unwrap();
        assert_eq!(
            pref_2640.display,
            Some(DisplayPreference::new(2640, 1200, 60).unwrap())
        );

        // Xiaomi 17 native landscape (2656x1220) at 120 FPS
        let pref_2656 = build_stream_preference(2656, 1220, 3, 0b111, true, 120, 80).unwrap();
        assert_eq!(
            pref_2656.display,
            Some(DisplayPreference::new(2656, 1220, 120).unwrap())
        );

        // 19.5:9 phone (2340x1080) and 20:9 QHD+ (3200x1440)
        let pref_2340 = build_stream_preference(2340, 1080, 2, 0b011, false, 90, 50).unwrap();
        assert_eq!(
            pref_2340.display,
            Some(DisplayPreference::new(2340, 1080, 90).unwrap())
        );
        let pref_3200 = build_stream_preference(3200, 1440, 2, 0b011, false, 120, 80).unwrap();
        assert_eq!(
            pref_3200.display,
            Some(DisplayPreference::new(3200, 1440, 120).unwrap())
        );

        // Odd dimensions align down to even numbers cleanly
        let pref_odd = build_stream_preference(2655, 1221, 0, 0, false, 60, 0).unwrap();
        assert_eq!(
            pref_odd.display,
            Some(DisplayPreference::new(2654, 1220, 60).unwrap())
        );

        // Oversized non-16:9 display (4400x2000, 2.2:1) scales down within 3840x2160 preserving aspect ratio
        let pref_oversized = build_stream_preference(4400, 2000, 0, 0, false, 60, 0).unwrap();
        let display = pref_oversized.display.unwrap();
        assert_eq!(display.width, 3840);
        assert_eq!(display.height, 1744);
        assert_eq!(display.width % 2, 0);
        assert_eq!(display.height % 2, 0);
    }

    #[test]
    fn pointer_key_scroll_and_gamepad_mapping() {
        assert_eq!(clamp_point(-10, 70000), Point { x: 0, y: 65535 });
        assert_eq!(map_pointer_button(0), Some(Button::Left));
        assert_eq!(map_pointer_button(1), Some(Button::Right));
        assert_eq!(map_pointer_button(2), Some(Button::Middle));
        assert_eq!(map_pointer_button(3), None);

        assert_eq!(map_key_input(0, true), None);
        assert_eq!(map_key_input(255, false), None);
        assert_eq!(
            map_key_input(65, true),
            Some(Input::Key {
                code: 65,
                down: true
            })
        );

        assert_eq!(
            map_scroll_input(100, 200, 120, -120),
            Input::Scroll {
                at: Point { x: 100, y: 200 },
                vertical: 120,
                horizontal: -120
            }
        );

        assert_eq!(map_gamepad_input(-1, true, 0, 0, 0, 0, 0, 0, 0), None);
        assert_eq!(map_gamepad_input(4, true, 0, 0, 0, 0, 0, 0, 0), None);

        let gp = map_gamepad_input(1, true, 0x1001, 300, -5, -40000, 40000, -1234, 5678).unwrap();
        assert_eq!(
            gp,
            Input::Gamepad(GamepadState {
                slot: 1,
                connected: true,
                buttons: 0x1001,
                left_trigger: 255,
                right_trigger: 0,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: -1234,
                right_y: 5678,
            })
        );
    }

    #[test]
    fn keyframe_and_codec_detection() {
        assert_eq!(codec_to_int(Codec::H264), 0);
        assert_eq!(codec_to_int(Codec::Hevc), 1);
        assert_eq!(codec_to_int(Codec::Av1), 2);
        assert_eq!(codec_label(Codec::H264), "H.264");
        assert_eq!(codec_label(Codec::Hevc), "HEVC");
        assert_eq!(codec_label(Codec::Av1), "AV1");

        let h264_idr = [
            0, 0, 0, 1, 0x67, 0x42, // SPS
            0, 0, 0, 1, 0x68, 0xce, // PPS
            0, 0, 0, 1, 0x65, 0x88, // IDR
        ];
        assert!(is_keyframe(Codec::H264, &h264_idr));

        let h264_p = [0, 0, 0, 1, 0x41, 0x9a];
        assert!(!is_keyframe(Codec::H264, &h264_p));

        let hevc_idr = [
            0, 0, 0, 1, 0x40, 0x01, // VPS (32)
            0, 0, 0, 1, 0x42, 0x01, // SPS (33)
            0, 0, 0, 1, 0x44, 0x01, // PPS (34)
            0, 0, 0, 1, 0x26, 0x01, // IDR_W_RADL (19)
        ];
        assert!(is_keyframe(Codec::Hevc, &hevc_idr));

        let av1_key = [
            0x12, 0x00, // OBU_TEMPORAL_DELIMITER (type=2, has_size=1, size=0)
            0x0a, 0x01, 0x00, // OBU_SEQUENCE_HEADER (type=1, has_size=1, size=1, reduced=0)
            0x32, 0x02, 0x00, 0xaa, // OBU_FRAME (type=6, has_size=1, size=2, KEY_FRAME)
        ];
        assert!(is_keyframe(Codec::Av1, &av1_key));

        let av1_inter = [
            0x12, 0x00, // OBU_TEMPORAL_DELIMITER
            0x32, 0x02, 0x20, 0x55, // OBU_FRAME (INTER_FRAME)
        ];
        assert!(!is_keyframe(Codec::Av1, &av1_inter));
    }
}
