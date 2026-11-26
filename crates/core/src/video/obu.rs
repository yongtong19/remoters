//! AV1 Open Bitstream Unit (OBU) inspection and `av1C` configuration generation.

use anyhow::{Context, Result, bail, ensure};

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_FRAME: u8 = 6;

const KEY_FRAME: u8 = 0;
const INTRA_ONLY_FRAME: u8 = 2;

const CP_BT_709: u8 = 1;
const TC_SRGB: u8 = 13;
const MC_IDENTITY: u8 = 0;

/// Parsed slice of a single AV1 OBU with its raw span and payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Obu<'a> {
    obu_type: u8,
    raw: &'a [u8],
    payload: &'a [u8],
}

/// MSB-first bit reader for AV1 sequence and frame headers.
struct BitReader<'a> {
    bytes: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit_pos: 0 }
    }

    fn read_bit(&mut self) -> Result<u8> {
        let byte_idx = self.bit_pos / 8;
        let bit_idx = 7 - (self.bit_pos % 8);
        let byte = *self
            .bytes
            .get(byte_idx)
            .context("Unexpected end of AV1 bitstream.")?;
        self.bit_pos += 1;
        Ok((byte >> bit_idx) & 1)
    }

    fn read_bool(&mut self) -> Result<bool> {
        Ok(self.read_bit()? != 0)
    }

    fn read_bits(&mut self, n: u8) -> Result<u32> {
        ensure!(n <= 32, "AV1 bit read width exceeds 32 bits.");
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }

    fn read_uvlc(&mut self) -> Result<u32> {
        let mut leading_zeros = 0u8;
        while !self.read_bool()? {
            leading_zeros = leading_zeros.saturating_add(1);
            if leading_zeros >= 32 {
                return Ok(u32::MAX);
            }
        }
        let value = self.read_bits(leading_zeros)?;
        Ok(value.saturating_add((1u32 << leading_zeros) - 1))
    }
}

fn read_leb128(bytes: &[u8]) -> Result<(usize, usize)> {
    let mut value = 0u64;
    for i in 0..8 {
        let byte = *bytes
            .get(i)
            .context("Truncated LEB128 size in AV1 OBU header.")?;
        value |= u64::from(byte & 0x7f) << (i * 7);
        if (byte & 0x80) == 0 {
            ensure!(
                value <= u64::from(u32::MAX),
                "AV1 OBU LEB128 size exceeds 32-bit specification limit."
            );
            return Ok((value as usize, i + 1));
        }
    }
    bail!("AV1 OBU LEB128 size exceeds 8 bytes.")
}

fn parse_one_obu(bytes: &[u8]) -> Result<(Obu<'_>, usize)> {
    let header = *bytes.first().context("Empty AV1 OBU buffer.")?;
    ensure!((header & 0x80) == 0, "Invalid AV1 OBU forbidden bit set.");
    let obu_type = (header >> 3) & 0x0f;
    let obu_extension_flag = (header & 0x04) != 0;
    let obu_has_size_field = (header & 0x02) != 0;
    ensure!(
        obu_has_size_field,
        "AV1 OBU missing required obu_has_size_field."
    );

    let mut offset = 1usize;
    if obu_extension_flag {
        ensure!(
            bytes.get(offset).is_some(),
            "Truncated AV1 OBU extension header."
        );
        offset += 1;
    }

    let (payload_size, leb_len) = read_leb128(&bytes[offset..])?;
    offset += leb_len;
    let payload_end = offset
        .checked_add(payload_size)
        .context("AV1 OBU size overflow.")?;
    ensure!(
        payload_end <= bytes.len(),
        "Truncated AV1 OBU payload (expected {payload_size} bytes)."
    );

    Ok((
        Obu {
            obu_type,
            raw: &bytes[..payload_end],
            payload: &bytes[offset..payload_end],
        },
        payload_end,
    ))
}

fn parse_reduced_still_picture_header(seq_payload: &[u8]) -> Result<bool> {
    let mut reader = BitReader::new(seq_payload);
    let _seq_profile = reader.read_bits(3)?;
    let _still_picture = reader.read_bool()?;
    reader.read_bool()
}

fn frame_header_is_keyframe(
    frame_payload: &[u8],
    reduced_still_picture_header: bool,
) -> Result<bool> {
    if reduced_still_picture_header {
        ensure!(!frame_payload.is_empty(), "Empty AV1 frame payload.");
        return Ok(true);
    }
    let mut reader = BitReader::new(frame_payload);
    let show_existing_frame = reader.read_bool()?;
    if show_existing_frame {
        return Ok(false);
    }
    let frame_type = reader.read_bits(2)? as u8;
    Ok(frame_type == KEY_FRAME || frame_type == INTRA_ONLY_FRAME)
}

/// Inspects an AV1 OBU-framed access unit (with `obu_has_size_field = 1`).
///
/// Validates all OBUs in the access unit, ensures at least one frame OBU
/// (`OBU_FRAME` or `OBU_FRAME_HEADER`) is present, and returns `Ok(true)` when
/// the access unit is a random-access point containing both an
/// `OBU_SEQUENCE_HEADER` and a keyframe (`KEY_FRAME` or `INTRA_ONLY_FRAME`).
pub fn inspect_av1(bytes: &[u8]) -> Result<bool> {
    ensure!(!bytes.is_empty(), "Empty AV1 access unit.");
    let mut offset = 0usize;
    let mut has_sequence_header = false;
    let mut reduced_still_picture_header = false;
    let mut has_frame = false;
    let mut is_keyframe = false;

    while offset < bytes.len() {
        let (obu, consumed) = parse_one_obu(&bytes[offset..])?;
        offset += consumed;
        match obu.obu_type {
            OBU_SEQUENCE_HEADER => {
                has_sequence_header = true;
                reduced_still_picture_header = parse_reduced_still_picture_header(obu.payload)?;
            }
            OBU_FRAME_HEADER | OBU_FRAME => {
                has_frame = true;
                if frame_header_is_keyframe(obu.payload, reduced_still_picture_header)? {
                    is_keyframe = true;
                }
            }
            _ => {}
        }
    }

    ensure!(has_frame, "The AV1 access unit contains no video frame.");
    Ok(has_sequence_header && is_keyframe)
}

/// Returns the complete raw `OBU_SEQUENCE_HEADER` OBU (including OBU header,
/// LEB128 size, and payload) for the first `OBU_SEQUENCE_HEADER` in `bytes`.
pub fn extract_sequence_header(bytes: &[u8]) -> Option<&[u8]> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let (obu, consumed) = parse_one_obu(&bytes[offset..]).ok()?;
        if obu.obu_type == OBU_SEQUENCE_HEADER {
            return Some(obu.raw);
        }
        offset += consumed;
    }
    None
}

/// Builds an ISO BMFF `av1C` (`AV1CodecConfigurationBox` payload) from a single
/// raw `OBU_SEQUENCE_HEADER` OBU.
pub fn build_av1c(seq_header_obu: &[u8]) -> Result<Vec<u8>> {
    let (obu, consumed) = parse_one_obu(seq_header_obu)?;
    ensure!(
        consumed == seq_header_obu.len(),
        "Expected a single OBU_SEQUENCE_HEADER OBU without trailing bytes."
    );
    ensure!(
        obu.obu_type == OBU_SEQUENCE_HEADER,
        "Expected OBU_SEQUENCE_HEADER (type 1), got type {}.",
        obu.obu_type
    );

    let mut reader = BitReader::new(obu.payload);
    let seq_profile = reader.read_bits(3)? as u8;
    let _still_picture = reader.read_bool()?;
    let reduced_still_picture_header = reader.read_bool()?;

    let seq_level_idx_0: u8;
    let seq_tier_0: bool;
    if reduced_still_picture_header {
        seq_level_idx_0 = reader.read_bits(5)? as u8;
        seq_tier_0 = false;
    } else {
        let timing_info_present_flag = reader.read_bool()?;
        let decoder_model_info_present_flag;
        let mut buffer_delay_length_minus_1 = 0u8;
        if timing_info_present_flag {
            let _num_units_in_display_tick = reader.read_bits(32)?;
            let _time_scale = reader.read_bits(32)?;
            let equal_picture_interval = reader.read_bool()?;
            if equal_picture_interval {
                let _num_ticks_per_picture_minus_1 = reader.read_uvlc()?;
            }
            decoder_model_info_present_flag = reader.read_bool()?;
            if decoder_model_info_present_flag {
                buffer_delay_length_minus_1 = reader.read_bits(5)? as u8;
                let _num_units_in_decoding_tick = reader.read_bits(32)?;
                let _buffer_removal_time_length_minus_1 = reader.read_bits(5)?;
                let _frame_presentation_time_length_minus_1 = reader.read_bits(5)?;
            }
        } else {
            decoder_model_info_present_flag = false;
        }

        let initial_display_delay_present_flag = reader.read_bool()?;
        let operating_points_cnt_minus_1 = reader.read_bits(5)? as usize;
        let mut level0 = 0u8;
        let mut tier0 = false;
        for i in 0..=operating_points_cnt_minus_1 {
            let _operating_point_idc = reader.read_bits(12)?;
            let level = reader.read_bits(5)? as u8;
            let tier = if level > 7 {
                reader.read_bool()?
            } else {
                false
            };
            if i == 0 {
                level0 = level;
                tier0 = tier;
            }
            if decoder_model_info_present_flag {
                let decoder_model_present_for_this_op = reader.read_bool()?;
                if decoder_model_present_for_this_op {
                    let n = buffer_delay_length_minus_1 + 1;
                    let _decoder_buffer_delay = reader.read_bits(n)?;
                    let _encoder_buffer_delay = reader.read_bits(n)?;
                    let _low_delay_mode_flag = reader.read_bool()?;
                }
            }
            if initial_display_delay_present_flag {
                let initial_display_delay_present_for_this_op = reader.read_bool()?;
                if initial_display_delay_present_for_this_op {
                    let _initial_display_delay_minus_1 = reader.read_bits(4)?;
                }
            }
        }
        seq_level_idx_0 = level0;
        seq_tier_0 = tier0;
    }

    let frame_width_bits_minus_1 = reader.read_bits(4)? as u8;
    let frame_height_bits_minus_1 = reader.read_bits(4)? as u8;
    let _max_frame_width_minus_1 = reader.read_bits(frame_width_bits_minus_1 + 1)?;
    let _max_frame_height_minus_1 = reader.read_bits(frame_height_bits_minus_1 + 1)?;

    let frame_id_numbers_present_flag = if reduced_still_picture_header {
        false
    } else {
        reader.read_bool()?
    };
    if frame_id_numbers_present_flag {
        let _delta_frame_id_length_minus_2 = reader.read_bits(4)?;
        let _additional_frame_id_length_minus_1 = reader.read_bits(3)?;
    }

    let _use_128x128_superblock = reader.read_bool()?;
    let _enable_filter_intra = reader.read_bool()?;
    let _enable_intra_edge_filter = reader.read_bool()?;

    if !reduced_still_picture_header {
        let _enable_interintra_compound = reader.read_bool()?;
        let _enable_masked_compound = reader.read_bool()?;
        let _enable_warped_motion = reader.read_bool()?;
        let _enable_dual_filter = reader.read_bool()?;
        let enable_order_hint = reader.read_bool()?;
        if enable_order_hint {
            let _enable_jnt_comp = reader.read_bool()?;
            let _enable_ref_frame_mvs = reader.read_bool()?;
        }
        let seq_choose_screen_content_tools = reader.read_bool()?;
        let seq_force_screen_content_tools = if seq_choose_screen_content_tools {
            2u8
        } else {
            reader.read_bit()?
        };
        if seq_force_screen_content_tools > 0 {
            let seq_choose_integer_mv = reader.read_bool()?;
            if !seq_choose_integer_mv {
                let _seq_force_integer_mv = reader.read_bit()?;
            }
        }
        if enable_order_hint {
            let _order_hint_bits_minus_1 = reader.read_bits(3)?;
        }
    }

    let _enable_superres = reader.read_bool()?;
    let _enable_cdef = reader.read_bool()?;
    let _enable_restoration = reader.read_bool()?;

    // color_config()
    let high_bitdepth = reader.read_bool()?;
    let (twelve_bit, bit_depth) = if seq_profile == 2 && high_bitdepth {
        let twelve = reader.read_bool()?;
        (twelve, if twelve { 12u8 } else { 10u8 })
    } else {
        (false, if high_bitdepth { 10u8 } else { 8u8 })
    };

    let monochrome = if seq_profile == 1 {
        false
    } else {
        reader.read_bool()?
    };

    let color_description_present_flag = reader.read_bool()?;
    let (color_primaries, transfer_characteristics, matrix_coefficients) =
        if color_description_present_flag {
            (
                reader.read_bits(8)? as u8,
                reader.read_bits(8)? as u8,
                reader.read_bits(8)? as u8,
            )
        } else {
            (2, 2, 2)
        };

    let (subsampling_x, subsampling_y, chroma_sample_position) = if monochrome {
        let _color_range = reader.read_bool()?;
        (true, true, 0u8)
    } else if color_primaries == CP_BT_709
        && transfer_characteristics == TC_SRGB
        && matrix_coefficients == MC_IDENTITY
    {
        (false, false, 0u8)
    } else {
        let _color_range = reader.read_bool()?;
        let (sx, sy) = if seq_profile == 0 {
            (true, true)
        } else if seq_profile == 1 {
            (false, false)
        } else if bit_depth == 12 {
            let sx = reader.read_bool()?;
            let sy = if sx { reader.read_bool()? } else { false };
            (sx, sy)
        } else {
            (true, false)
        };
        let csp = if sx && sy {
            reader.read_bits(2)? as u8
        } else {
            0u8
        };
        (sx, sy, csp)
    };

    let mut out = Vec::with_capacity(4 + seq_header_obu.len());
    out.push(0x81);
    out.push(((seq_profile & 0x07) << 5) | (seq_level_idx_0 & 0x1f));
    out.push(
        (u8::from(seq_tier_0) << 7)
            | (u8::from(high_bitdepth) << 6)
            | (u8::from(twelve_bit) << 5)
            | (u8::from(monochrome) << 4)
            | (u8::from(subsampling_x) << 3)
            | (u8::from(subsampling_y) << 2)
            | (chroma_sample_position & 0x03),
    );
    out.push(0x00);
    out.extend_from_slice(seq_header_obu);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BitWriter {
        bytes: Vec<u8>,
        cur: u8,
        bits_in_cur: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                cur: 0,
                bits_in_cur: 0,
            }
        }

        fn push_bit(&mut self, bit: bool) {
            self.cur = (self.cur << 1) | u8::from(bit);
            self.bits_in_cur += 1;
            if self.bits_in_cur == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.bits_in_cur = 0;
            }
        }

        fn push_bits(&mut self, value: u32, n: u8) {
            for shift in (0..n).rev() {
                self.push_bit(((value >> shift) & 1) != 0);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.bits_in_cur > 0 {
                // AV1 trailing bits / zero-pad remaining bits in final byte
                self.cur <<= 8 - self.bits_in_cur;
                self.bytes.push(self.cur);
            }
            self.bytes
        }
    }

    fn wrap_obu(obu_type: u8, extension: Option<u8>, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut header = (obu_type & 0x0f) << 3 | 0x02; // obu_has_size_field = 1
        if extension.is_some() {
            header |= 0x04;
        }
        out.push(header);
        if let Some(ext) = extension {
            out.push(ext);
        }
        let mut size = payload.len();
        loop {
            let mut byte = (size & 0x7f) as u8;
            size >>= 7;
            if size > 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if size == 0 {
                break;
            }
        }
        out.extend_from_slice(payload);
        out
    }

    fn build_seq_payload(
        seq_profile: u8,
        seq_level_idx: u8,
        seq_tier: bool,
        high_bitdepth: bool,
        twelve_bit: bool,
        hdr_bt2020_pq: bool,
        chroma_sample_position: u8,
    ) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.push_bits(u32::from(seq_profile), 3);
        w.push_bit(false); // still_picture
        w.push_bit(false); // reduced_still_picture_header
        w.push_bit(false); // timing_info_present_flag
        w.push_bit(false); // initial_display_delay_present_flag
        w.push_bits(0, 5); // operating_points_cnt_minus_1 = 0
        w.push_bits(0, 12); // operating_point_idc[0]
        w.push_bits(u32::from(seq_level_idx), 5);
        if seq_level_idx > 7 {
            w.push_bit(seq_tier);
        }
        w.push_bits(11, 4); // frame_width_bits_minus_1 = 11 (12 bits)
        w.push_bits(11, 4); // frame_height_bits_minus_1 = 11 (12 bits)
        w.push_bits(3839, 12); // max_frame_width_minus_1
        w.push_bits(2159, 12); // max_frame_height_minus_1
        w.push_bit(false); // frame_id_numbers_present_flag
        w.push_bit(true); // use_128x128_superblock
        w.push_bit(false); // enable_filter_intra
        w.push_bit(false); // enable_intra_edge_filter
        w.push_bit(false); // enable_interintra_compound
        w.push_bit(false); // enable_masked_compound
        w.push_bit(false); // enable_warped_motion
        w.push_bit(false); // enable_dual_filter
        w.push_bit(true); // enable_order_hint
        w.push_bit(false); // enable_jnt_comp
        w.push_bit(false); // enable_ref_frame_mvs
        w.push_bit(true); // seq_choose_screen_content_tools
        w.push_bit(true); // seq_choose_integer_mv
        w.push_bits(6, 3); // order_hint_bits_minus_1
        w.push_bit(false); // enable_superres
        w.push_bit(true); // enable_cdef
        w.push_bit(true); // enable_restoration
        // color_config()
        w.push_bit(high_bitdepth);
        if seq_profile == 2 && high_bitdepth {
            w.push_bit(twelve_bit);
        }
        if seq_profile != 1 {
            w.push_bit(false); // mono_chrome = 0
        }
        w.push_bit(true); // color_description_present_flag
        if hdr_bt2020_pq {
            w.push_bits(9, 8); // BT.2020 primaries
            w.push_bits(16, 8); // SMPTE ST 2084 PQ
            w.push_bits(9, 8); // BT.2020 NCL
        } else {
            w.push_bits(1, 8); // BT.709
            w.push_bits(1, 8); // BT.709
            w.push_bits(1, 8); // BT.709
        }
        w.push_bit(false); // color_range = studio/limited
        if seq_profile == 0 {
            w.push_bits(u32::from(chroma_sample_position), 2);
        }
        w.push_bit(false); // separate_uv_delta_q
        w.finish()
    }

    #[test]
    fn inspect_and_extract_av1_access_units() {
        let td = wrap_obu(2, None, &[]);
        let seq = wrap_obu(
            OBU_SEQUENCE_HEADER,
            None,
            &build_seq_payload(0, 12, false, false, false, false, 0),
        );
        // show_existing_frame=0 (bit 7 = 0), frame_type=0 KEY_FRAME (bits 6..5 = 00) -> 0x00
        let key_frame = wrap_obu(OBU_FRAME, None, &[0x00, 0xaa, 0xbb]);
        // show_existing_frame=0 (bit 7 = 0), frame_type=2 INTRA_ONLY_FRAME (bits 6..5 = 10) -> 0x40
        let intra_frame = wrap_obu(OBU_FRAME_HEADER, Some(0x20), &[0x40, 0x11]);
        // show_existing_frame=0 (bit 7 = 0), frame_type=1 INTER_FRAME (bits 6..5 = 01) -> 0x20
        let inter_frame = wrap_obu(OBU_FRAME, None, &[0x20, 0x55]);
        // show_existing_frame=1 (bit 7 = 1) -> 0x80
        let show_existing = wrap_obu(OBU_FRAME_HEADER, None, &[0x80]);

        let key_au = [td.as_slice(), seq.as_slice(), key_frame.as_slice()].concat();
        assert!(inspect_av1(&key_au).unwrap());
        assert_eq!(extract_sequence_header(&key_au), Some(seq.as_slice()));

        let intra_au = [seq.as_slice(), intra_frame.as_slice()].concat();
        assert!(inspect_av1(&intra_au).unwrap());

        let key_without_seq = [td.as_slice(), key_frame.as_slice()].concat();
        assert!(!inspect_av1(&key_without_seq).unwrap());
        assert_eq!(extract_sequence_header(&key_without_seq), None);

        let delta_au = [td.as_slice(), inter_frame.as_slice()].concat();
        assert!(!inspect_av1(&delta_au).unwrap());

        let show_existing_au = [seq.as_slice(), show_existing.as_slice()].concat();
        assert!(!inspect_av1(&show_existing_au).unwrap());

        // Missing obu_has_size_field must fail cleanly.
        assert!(inspect_av1(&[0x08, 0x00]).is_err());
        // Sequence header without any frame must fail cleanly.
        assert!(inspect_av1(&seq).is_err());
        // Empty buffer must fail cleanly.
        assert!(inspect_av1(&[]).is_err());
    }

    #[test]
    fn build_av1c_8bit_main_and_10bit_hdr10() {
        // 1. 8-bit Main profile (seq_profile=0, level=12 (5.0), tier=0, high_bitdepth=0, 4:2:0, csp=0)
        let seq_8bit = wrap_obu(
            OBU_SEQUENCE_HEADER,
            None,
            &build_seq_payload(0, 12, false, false, false, false, 0),
        );
        let av1c_8bit = build_av1c(&seq_8bit).unwrap();
        assert_eq!(av1c_8bit[0], 0x81);
        assert_eq!(av1c_8bit[1], 12); // (0 << 5) | 12
        assert_eq!(av1c_8bit[2], 0b0_0_0_0_1_1_00); // subsampling_x=1, subsampling_y=1, csp=0
        assert_eq!(av1c_8bit[3], 0x00);
        assert_eq!(&av1c_8bit[4..], seq_8bit.as_slice());

        // 2. 10-bit HDR10 Main profile (seq_profile=0, level=13 (5.1), tier=1, high_bitdepth=1, 4:2:0, csp=2 colocated)
        let seq_10bit = wrap_obu(
            OBU_SEQUENCE_HEADER,
            None,
            &build_seq_payload(0, 13, true, true, false, true, 2),
        );
        let av1c_10bit = build_av1c(&seq_10bit).unwrap();
        assert_eq!(av1c_10bit[0], 0x81);
        assert_eq!(av1c_10bit[1], 13); // (0 << 5) | 13
        assert_eq!(av1c_10bit[2], 0b1_1_0_0_1_1_10); // tier=1, high_bitdepth=1, sx=1, sy=1, csp=2
        assert_eq!(av1c_10bit[3], 0x00);
        assert_eq!(&av1c_10bit[4..], seq_10bit.as_slice());

        // Reject non-sequence-header OBU or trailing bytes.
        let frame_obu = wrap_obu(OBU_FRAME, None, &[0x00]);
        assert!(build_av1c(&frame_obu).is_err());
        let trailing = [seq_8bit.as_slice(), &[0x00]].concat();
        assert!(build_av1c(&trailing).is_err());
    }
}
