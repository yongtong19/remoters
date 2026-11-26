//! Official NVENC 12.0 & CUDA Driver API C ABI definitions, GUIDs, version macros,
//! and Sunshine-grade ultra-low-latency configuration helpers.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use core::ffi::c_void;

use remoters_core::video::Codec;

pub const NVENC_TARGET_MAJOR: u32 = 12;
pub const NVENC_TARGET_MINOR: u32 = 0;
pub const NVENCAPI_VERSION: u32 = NVENC_TARGET_MAJOR | (NVENC_TARGET_MINOR << 24);
pub const NVENC_INFINITE_GOPLENGTH: u32 = 0xffff_ffff;

pub const fn nvenc_struct_version(ver: u32) -> u32 {
    NVENCAPI_VERSION | (ver << 16) | (0x7 << 28)
}

pub const fn nvenc_struct_version_v2(ver: u32) -> u32 {
    nvenc_struct_version(ver) | (1u32 << 31)
}

pub const NV_ENCODE_API_FUNCTION_LIST_VER: u32 = nvenc_struct_version(2);
pub const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = nvenc_struct_version(1);
pub const NV_ENC_PRESET_CONFIG_VER: u32 = nvenc_struct_version_v2(4);
pub const NV_ENC_CONFIG_VER: u32 = nvenc_struct_version_v2(8);
pub const NV_ENC_RC_PARAMS_VER: u32 = nvenc_struct_version(1);
pub const NV_ENC_INITIALIZE_PARAMS_VER: u32 = nvenc_struct_version_v2(5);
pub const NV_ENC_RECONFIGURE_PARAMS_VER: u32 = nvenc_struct_version_v2(1);
pub const NV_ENC_REGISTER_RESOURCE_VER: u32 = nvenc_struct_version(4);
pub const NV_ENC_MAP_INPUT_RESOURCE_VER: u32 = nvenc_struct_version(4);
pub const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = nvenc_struct_version(1);
pub const NV_ENC_PIC_PARAMS_VER: u32 = nvenc_struct_version_v2(6);
pub const NV_ENC_LOCK_BITSTREAM_VER: u32 = nvenc_struct_version(2);
pub const NV_ENC_CAPS_PARAM_VER: u32 = nvenc_struct_version(1);

// Status codes
pub const NV_ENC_SUCCESS: u32 = 0;

// Device / resource / usage enums
pub const NV_ENC_DEVICE_TYPE_CUDA: u32 = 0x1;
pub const NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR: u32 = 0x1;
pub const NV_ENC_INPUT_IMAGE: u32 = 0x0;

// Buffer formats (little-endian word order in nvEncodeAPI.h):
// ARGB = bytes [B, G, R, A] (matches PipeWire/X11 bgr0/bgra)
// ABGR = bytes [R, G, B, A] (matches PipeWire/xcap rgb0/rgba)
pub const NV_ENC_BUFFER_FORMAT_ARGB: u32 = 0x0100_0000;
pub const NV_ENC_BUFFER_FORMAT_ABGR: u32 = 0x1000_0000;

// Tuning / rate-control / picture enums
pub const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
pub const NV_ENC_PARAMS_RC_CBR: u32 = 0x2;
pub const NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME: u32 = 0x1;
pub const NV_ENC_PIC_STRUCT_FRAME: u32 = 0x1;
pub const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x2;
pub const NV_ENC_PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;
pub const NV_ENC_PIC_FLAG_EOS: u32 = 0x8;
pub const NV_ENC_CAPS_SUPPORT_INTRA_REFRESH: u32 = 25;

// VUI BT.709 color constants
pub const NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED: u32 = 5;
pub const NV_ENC_VUI_COLOR_PRIMARIES_BT709: u32 = 1;
pub const NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709: u32 = 1;
pub const NV_ENC_VUI_MATRIX_COEFFS_BT709: u32 = 1;

// Bitfield masks verified against official nvEncodeAPI.h v12.0
pub const RC_FLAG_ENABLE_LOOKAHEAD: u32 = 1 << 5;
pub const RC_FLAG_ZERO_REORDER_DELAY: u32 = 1 << 9;

pub const H264_FLAG_OUTPUT_AUD: u32 = 1 << 6;
pub const H264_FLAG_ENABLE_INTRA_REFRESH: u32 = 1 << 10;
pub const H264_FLAG_REPEAT_SPSPPS: u32 = 1 << 12;
pub const H264_FLAG_ENABLE_FILLER_DATA: u32 = 1 << 17;

pub const HEVC_FLAG_OUTPUT_AUD: u32 = 1 << 4;
pub const HEVC_FLAG_REPEAT_SPSPPS: u32 = 1 << 7;
pub const HEVC_FLAG_ENABLE_INTRA_REFRESH: u32 = 1 << 8;
pub const HEVC_CHROMA_FORMAT_SHIFT: u32 = 9;
pub const HEVC_CHROMA_FORMAT_MASK: u32 = 0x3 << HEVC_CHROMA_FORMAT_SHIFT;
pub const HEVC_BIT_DEPTH_MINUS8_MASK: u32 = 0x7 << 11;
pub const HEVC_FLAG_ENABLE_FILLER_DATA: u32 = 1 << 14;

pub const AV1_FLAG_OUTPUT_ANNEX_B: u32 = 1 << 0;
pub const AV1_FLAG_REPEAT_SEQ_HDR: u32 = 1 << 5;
pub const AV1_FLAG_ENABLE_INTRA_REFRESH: u32 = 1 << 6;
pub const AV1_CHROMA_FORMAT_SHIFT: u32 = 7;
pub const AV1_CHROMA_FORMAT_MASK: u32 = 0x3 << AV1_CHROMA_FORMAT_SHIFT;
pub const AV1_INPUT_BIT_DEPTH_MINUS8_MASK: u32 = 0x7 << 12;
pub const AV1_PIXEL_BIT_DEPTH_MINUS8_MASK: u32 = 0x7 << 15;

pub const RECONFIG_FLAG_RESET_ENCODER: u32 = 1 << 0;
pub const RECONFIG_FLAG_FORCE_IDR: u32 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NvEncGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

pub const NV_ENC_CODEC_H264_GUID: NvEncGuid = NvEncGuid {
    data1: 0x6bc8_2762,
    data2: 0x4e63,
    data3: 0x4ca4,
    data4: [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
};

pub const NV_ENC_CODEC_HEVC_GUID: NvEncGuid = NvEncGuid {
    data1: 0x790c_dc88,
    data2: 0x4522,
    data3: 0x4d7b,
    data4: [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
};

pub const NV_ENC_CODEC_AV1_GUID: NvEncGuid = NvEncGuid {
    data1: 0x0a35_2289,
    data2: 0x0aa7,
    data3: 0x4759,
    data4: [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
};

pub const NV_ENC_H264_PROFILE_HIGH_GUID: NvEncGuid = NvEncGuid {
    data1: 0xe7cb_c309,
    data2: 0x4f7a,
    data3: 0x4b89,
    data4: [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
};

pub const NV_ENC_HEVC_PROFILE_MAIN_GUID: NvEncGuid = NvEncGuid {
    data1: 0xb514_c39a,
    data2: 0xb55b,
    data3: 0x40fa,
    data4: [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
};

pub const NV_ENC_AV1_PROFILE_MAIN_GUID: NvEncGuid = NvEncGuid {
    data1: 0x5f2a_39f5,
    data2: 0xf14e,
    data3: 0x4f95,
    data4: [0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
};

pub const NV_ENC_PRESET_P1_GUID: NvEncGuid = NvEncGuid {
    data1: 0xfc0a_8d3e,
    data2: 0x45f8,
    data3: 0x4cf8,
    data4: [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
};

pub fn codec_guid(codec: Codec) -> NvEncGuid {
    match codec {
        Codec::H264 => NV_ENC_CODEC_H264_GUID,
        Codec::Hevc => NV_ENC_CODEC_HEVC_GUID,
        Codec::Av1 => NV_ENC_CODEC_AV1_GUID,
    }
}

pub fn profile_guid(codec: Codec) -> NvEncGuid {
    match codec {
        Codec::H264 => NV_ENC_H264_PROFILE_HIGH_GUID,
        Codec::Hevc => NV_ENC_HEVC_PROFILE_MAIN_GUID,
        Codec::Av1 => NV_ENC_AV1_PROFILE_MAIN_GUID,
    }
}

/// Decodes `(major, minor)` from `NvEncodeAPIGetMaxSupportedVersion`.
pub const fn parse_driver_nvenc_version(raw: u32) -> (u32, u32) {
    (raw >> 4, raw & 0x0f)
}

/// Returns true when the installed NVIDIA driver supports NVENC API >= 12.0.
pub const fn driver_supports_nvenc_12(raw: u32) -> bool {
    let (major, minor) = parse_driver_nvenc_version(raw);
    major > NVENC_TARGET_MAJOR || (major == NVENC_TARGET_MAJOR && minor >= NVENC_TARGET_MINOR)
}

/// Maps a desktop pixel format name to NVENC's native 32-bit packed RGB format.
pub fn pixel_format_to_nvenc(pix_fmt: &str) -> Option<u32> {
    match pix_fmt {
        "bgr0" | "bgra" => Some(NV_ENC_BUFFER_FORMAT_ARGB),
        "rgb0" | "rgba" => Some(NV_ENC_BUFFER_FORMAT_ABGR),
        _ => None,
    }
}

/// Computes Sunshine-style single-frame VBV size in bits: `(bitrate / fps) * 1.25`,
/// clamped to a safe lower floor so NVENC never rejects extreme low-bitrate edge cases.
pub fn single_frame_vbv_bits(bitrate_bps: u32, fps: u16) -> u32 {
    let fps = u64::from(fps.max(1));
    let bitrate = u64::from(bitrate_bps.max(500_000));
    let bits_per_frame = bitrate / fps;
    let vbv = (bits_per_frame * 5) / 4;
    vbv.clamp(65_536, bitrate) as u32
}

/// Computes `(intra_refresh_period, intra_refresh_cnt)` in frames for gradual decoder refresh.
pub fn intra_refresh_params(fps: u16) -> (u32, u32) {
    let fps = u32::from(fps.max(1));
    let period = fps.saturating_mul(2).max(2);
    let count = (fps / 4).clamp(5, 30).min(period - 1).max(1);
    (period, count)
}

// ---------------------------------------------------------------------------
// NVENC 12.0 & CUDA Driver API C ABI structs
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CudaMemcpy2d {
    pub src_x_in_bytes: usize,
    pub src_y: usize,
    pub src_memory_type: u32,
    pub src_host: *const c_void,
    pub src_device: u64,
    pub src_array: *mut c_void,
    pub src_pitch: usize,
    pub dst_x_in_bytes: usize,
    pub dst_y: usize,
    pub dst_memory_type: u32,
    pub dst_host: *mut c_void,
    pub dst_device: u64,
    pub dst_array: *mut c_void,
    pub dst_pitch: usize,
    pub width_in_bytes: usize,
    pub height: usize,
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NvEncQp {
    pub qp_inter_p: u32,
    pub qp_inter_b: u32,
    pub qp_intra: u32,
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncRcParams {
    pub version: u32,
    pub rate_control_mode: u32,
    pub const_qp: NvEncQp,
    pub average_bit_rate: u32,
    pub max_bit_rate: u32,
    pub vbv_buffer_size: u32,
    pub vbv_initial_delay: u32,
    pub flags: u32,
    pub min_qp: NvEncQp,
    pub max_qp: NvEncQp,
    pub initial_rc_qp: NvEncQp,
    pub temporal_layer_idx_mask: u32,
    pub temporal_layer_qp: [u8; 8],
    pub target_quality: u8,
    pub target_quality_lsb: u8,
    pub lookahead_depth: u16,
    pub low_delay_key_frame_scale: u8,
    pub y_dc_qp_index_offset: i8,
    pub u_dc_qp_index_offset: i8,
    pub v_dc_qp_index_offset: i8,
    pub qp_map_mode: u32,
    pub multi_pass: u32,
    pub alpha_layer_bitrate_ratio: u32,
    pub cb_qp_index_offset: i8,
    pub cr_qp_index_offset: i8,
    pub reserved2: u16,
    pub reserved: [u32; 4],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncConfigVuiParameters {
    pub overscan_info_present_flag: u32,
    pub overscan_info: u32,
    pub video_signal_type_present_flag: u32,
    pub video_format: u32,
    pub video_full_range_flag: u32,
    pub colour_description_present_flag: u32,
    pub colour_primaries: u32,
    pub transfer_characteristics: u32,
    pub colour_matrix: u32,
    pub chroma_sample_location_flag: u32,
    pub chroma_sample_location_top: u32,
    pub chroma_sample_location_bot: u32,
    pub bitstream_restriction_flag: u32,
    pub timing_info_present_flag: u32,
    pub num_unit_in_ticks: u32,
    pub time_scale: u32,
    pub reserved: [u32; 12],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncConfigH264 {
    pub flags: u32,
    pub level: u32,
    pub idr_period: u32,
    pub separate_colour_plane_flag: u32,
    pub disable_deblocking_filter_idc: u32,
    pub num_temporal_layers: u32,
    pub sps_id: u32,
    pub pps_id: u32,
    pub adaptive_transform_mode: u32,
    pub fmo_mode: u32,
    pub bdirect_mode: u32,
    pub entropy_coding_mode: u32,
    pub stereo_mode: u32,
    pub intra_refresh_period: u32,
    pub intra_refresh_cnt: u32,
    pub max_num_ref_frames: u32,
    pub slice_mode: u32,
    pub slice_mode_data: u32,
    pub h264_vui_parameters: NvEncConfigVuiParameters,
    pub ltr_num_frames: u32,
    pub ltr_trust_mode: u32,
    pub chroma_format_idc: u32,
    pub max_temporal_layers: u32,
    pub use_b_frames_as_ref: u32,
    pub num_ref_l0: u32,
    pub num_ref_l1: u32,
    pub reserved1: [u32; 267],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncConfigHevc {
    pub level: u32,
    pub tier: u32,
    pub min_cu_size: u32,
    pub max_cu_size: u32,
    pub flags: u32,
    pub idr_period: u32,
    pub intra_refresh_period: u32,
    pub intra_refresh_cnt: u32,
    pub max_num_ref_frames_in_dpb: u32,
    pub ltr_num_frames: u32,
    pub vps_id: u32,
    pub sps_id: u32,
    pub pps_id: u32,
    pub slice_mode: u32,
    pub slice_mode_data: u32,
    pub max_temporal_layers_minus1: u32,
    pub hevc_vui_parameters: NvEncConfigVuiParameters,
    pub ltr_trust_mode: u32,
    pub use_b_frames_as_ref: u32,
    pub num_ref_l0: u32,
    pub num_ref_l1: u32,
    pub reserved1: [u32; 214],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncConfigAv1 {
    pub level: u32,
    pub tier: u32,
    pub min_part_size: u32,
    pub max_part_size: u32,
    pub flags: u32,
    pub idr_period: u32,
    pub intra_refresh_period: u32,
    pub intra_refresh_cnt: u32,
    pub max_num_ref_frames_in_dpb: u32,
    pub num_tile_columns: u32,
    pub num_tile_rows: u32,
    pub tile_widths: *mut u32,
    pub tile_heights: *mut u32,
    pub max_temporal_layers_minus1: u32,
    pub color_primaries: u32,
    pub transfer_characteristics: u32,
    pub matrix_coefficients: u32,
    pub color_range: u32,
    pub chroma_sample_position: u32,
    pub use_b_frames_as_ref: u32,
    pub film_grain_params: *mut c_void,
    pub num_fwd_refs: u32,
    pub num_bwd_refs: u32,
    pub reserved1: [u32; 235],
    pub reserved2: [*mut c_void; 62],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub union NvEncCodecConfig {
    pub h264: NvEncConfigH264,
    pub hevc: NvEncConfigHevc,
    pub av1: NvEncConfigAv1,
    pub reserved: [u32; 320],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncConfig {
    pub version: u32,
    pub profile_guid: NvEncGuid,
    pub gop_length: u32,
    pub frame_interval_p: i32,
    pub mono_chrome_encoding: u32,
    pub frame_field_mode: u32,
    pub mv_precision: u32,
    pub rc_params: NvEncRcParams,
    pub encode_codec_config: NvEncCodecConfig,
    pub reserved: [u32; 278],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncPresetConfig {
    pub version: u32,
    pub preset_cfg: NvEncConfig,
    pub reserved1: [u32; 255],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncInitializeParams {
    pub version: u32,
    pub encode_guid: NvEncGuid,
    pub preset_guid: NvEncGuid,
    pub encode_width: u32,
    pub encode_height: u32,
    pub dar_width: u32,
    pub dar_height: u32,
    pub frame_rate_num: u32,
    pub frame_rate_den: u32,
    pub enable_encode_async: u32,
    pub enable_ptd: u32,
    pub flags: u32,
    pub priv_data_size: u32,
    pub priv_data: *mut c_void,
    pub encode_config: *mut NvEncConfig,
    pub max_encode_width: u32,
    pub max_encode_height: u32,
    pub max_me_hint_counts_per_block: [[u32; 4]; 2],
    pub tuning_info: u32,
    pub buffer_format: u32,
    pub reserved: [u32; 287],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncReconfigureParams {
    pub version: u32,
    pub re_init_encode_params: NvEncInitializeParams,
    pub flags: u32,
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncOpenEncodeSessionExParams {
    pub version: u32,
    pub device_type: u32,
    pub device: *mut c_void,
    pub reserved: *mut c_void,
    pub api_version: u32,
    pub reserved1: [u32; 253],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncRegisterResource {
    pub version: u32,
    pub resource_type: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub sub_resource_index: u32,
    pub resource_to_register: *mut c_void,
    pub registered_resource: *mut c_void,
    pub buffer_format: u32,
    pub buffer_usage: u32,
    pub p_input_fence_point: *mut c_void,
    pub reserved1: [u32; 247],
    pub reserved2: [*mut c_void; 61],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncMapInputResource {
    pub version: u32,
    pub sub_resource_index: u32,
    pub input_resource: *mut c_void,
    pub registered_resource: *mut c_void,
    pub mapped_resource: *mut c_void,
    pub mapped_buffer_fmt: u32,
    pub reserved1: [u32; 251],
    pub reserved2: [*mut c_void; 63],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncCreateBitstreamBuffer {
    pub version: u32,
    pub size: u32,
    pub memory_heap: u32,
    pub reserved: u32,
    pub bitstream_buffer: *mut c_void,
    pub bitstream_buffer_ptr: *mut c_void,
    pub reserved1: [u32; 58],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncPicParams {
    pub version: u32,
    pub input_width: u32,
    pub input_height: u32,
    pub input_pitch: u32,
    pub encode_pic_flags: u32,
    pub frame_idx: u32,
    pub input_timestamp: u64,
    pub input_duration: u64,
    pub input_buffer: *mut c_void,
    pub output_bitstream: *mut c_void,
    pub completion_event: *mut c_void,
    pub buffer_fmt: u32,
    pub picture_struct: u32,
    pub picture_type: u32,
    pub _pad_after_pic_type: u32,
    pub _codec_and_tail: [u64; 410],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncLockBitstream {
    pub version: u32,
    pub flags: u32,
    pub output_bitstream: *mut c_void,
    pub slice_offsets: *mut u32,
    pub frame_idx: u32,
    pub hw_encode_status: u32,
    pub num_slices: u32,
    pub bitstream_size_in_bytes: u32,
    pub output_timestamp: u64,
    pub output_duration: u64,
    pub bitstream_buffer_ptr: *mut c_void,
    pub picture_type: u32,
    pub picture_struct: u32,
    pub frame_avg_qp: u32,
    pub frame_satd: u32,
    pub ltr_frame_idx: u32,
    pub ltr_frame_bitmap: u32,
    pub temporal_id: u32,
    pub reserved: [u32; 12],
    pub intra_mb_count: u32,
    pub inter_mb_count: u32,
    pub average_mvx: i32,
    pub average_mvy: i32,
    pub alpha_layer_size_in_bytes: u32,
    pub reserved1: [u32; 218],
    pub reserved2: [*mut c_void; 64],
}

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NvEncCapsParam {
    pub version: u32,
    pub caps_to_query: u32,
    pub reserved: [u32; 62],
}

#[allow(dead_code)]
#[repr(C)]
pub struct NvEncodeApiFunctionList {
    pub version: u32,
    pub reserved: u32,
    pub nv_enc_open_encode_session: *mut c_void,
    pub nv_enc_get_encode_guid_count: Option<unsafe extern "C" fn(*mut c_void, *mut u32) -> u32>,
    pub nv_enc_get_encode_profile_guid_count: *mut c_void,
    pub nv_enc_get_encode_profile_guids: *mut c_void,
    pub nv_enc_get_encode_guids:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncGuid, u32, *mut u32) -> u32>,
    pub nv_enc_get_input_format_count:
        Option<unsafe extern "C" fn(*mut c_void, NvEncGuid, *mut u32) -> u32>,
    pub nv_enc_get_input_formats:
        Option<unsafe extern "C" fn(*mut c_void, NvEncGuid, *mut u32, u32, *mut u32) -> u32>,
    pub nv_enc_get_encode_caps:
        Option<unsafe extern "C" fn(*mut c_void, NvEncGuid, *mut NvEncCapsParam, *mut i32) -> u32>,
    pub nv_enc_get_encode_preset_count: *mut c_void,
    pub nv_enc_get_encode_preset_guids: *mut c_void,
    pub nv_enc_get_encode_preset_config: *mut c_void,
    pub nv_enc_initialize_encoder:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncInitializeParams) -> u32>,
    pub nv_enc_create_input_buffer: *mut c_void,
    pub nv_enc_destroy_input_buffer: *mut c_void,
    pub nv_enc_create_bitstream_buffer:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncCreateBitstreamBuffer) -> u32>,
    pub nv_enc_destroy_bitstream_buffer:
        Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>,
    pub nv_enc_encode_picture:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncPicParams) -> u32>,
    pub nv_enc_lock_bitstream:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncLockBitstream) -> u32>,
    pub nv_enc_unlock_bitstream: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>,
    pub nv_enc_lock_input_buffer: *mut c_void,
    pub nv_enc_unlock_input_buffer: *mut c_void,
    pub nv_enc_get_encode_stats: *mut c_void,
    pub nv_enc_get_sequence_params: *mut c_void,
    pub nv_enc_register_async_event: *mut c_void,
    pub nv_enc_unregister_async_event: *mut c_void,
    pub nv_enc_map_input_resource:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncMapInputResource) -> u32>,
    pub nv_enc_unmap_input_resource: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>,
    pub nv_enc_destroy_encoder: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    pub nv_enc_invalidate_ref_frames: *mut c_void,
    pub nv_enc_open_encode_session_ex:
        Option<unsafe extern "C" fn(*mut NvEncOpenEncodeSessionExParams, *mut *mut c_void) -> u32>,
    pub nv_enc_register_resource:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncRegisterResource) -> u32>,
    pub nv_enc_unregister_resource: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>,
    pub nv_enc_reconfigure_encoder:
        Option<unsafe extern "C" fn(*mut c_void, *mut NvEncReconfigureParams) -> u32>,
    pub reserved1: *mut c_void,
    pub nv_enc_create_mv_buffer: *mut c_void,
    pub nv_enc_destroy_mv_buffer: *mut c_void,
    pub nv_enc_run_motion_estimation_only: *mut c_void,
    pub nv_enc_get_last_error_string:
        Option<unsafe extern "C" fn(*mut c_void) -> *const core::ffi::c_char>,
    pub nv_enc_set_io_cuda_streams: *mut c_void,
    pub nv_enc_get_encode_preset_config_ex: Option<
        unsafe extern "C" fn(*mut c_void, NvEncGuid, NvEncGuid, u32, *mut NvEncPresetConfig) -> u32,
    >,
    pub nv_enc_get_sequence_param_ex: *mut c_void,
    pub reserved2: [*mut c_void; 277],
}

/// Applies Sunshine-grade ultra-low-latency CBR + single-frame VBV + periodic
/// intra-refresh parameters onto a preset `NvEncConfig`.
pub fn configure_ultra_low_latency(
    cfg: &mut NvEncConfig,
    codec: Codec,
    fps: u16,
    bitrate_bps: u32,
    supports_intra_refresh: bool,
) {
    let fps = fps.max(1);
    let vbv_bits = single_frame_vbv_bits(bitrate_bps, fps);
    let (ir_period, ir_cnt) = intra_refresh_params(fps);

    cfg.version = NV_ENC_CONFIG_VER;
    cfg.profile_guid = profile_guid(codec);
    cfg.gop_length = if supports_intra_refresh {
        NVENC_INFINITE_GOPLENGTH
    } else {
        u32::from(fps) * 2
    };
    cfg.frame_interval_p = 1; // 0 B-frames (IPPP...)
    cfg.frame_field_mode = NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;

    cfg.rc_params.version = NV_ENC_RC_PARAMS_VER;
    cfg.rc_params.rate_control_mode = NV_ENC_PARAMS_RC_CBR;
    cfg.rc_params.average_bit_rate = bitrate_bps;
    cfg.rc_params.max_bit_rate = bitrate_bps;
    cfg.rc_params.vbv_buffer_size = vbv_bits;
    cfg.rc_params.vbv_initial_delay = vbv_bits;
    cfg.rc_params.flags =
        (cfg.rc_params.flags & !RC_FLAG_ENABLE_LOOKAHEAD) | RC_FLAG_ZERO_REORDER_DELAY;
    cfg.rc_params.lookahead_depth = 0;
    cfg.rc_params.low_delay_key_frame_scale = 1;

    match codec {
        Codec::H264 => {
            let h264 = unsafe { &mut cfg.encode_codec_config.h264 };
            h264.flags = (h264.flags & !H264_FLAG_ENABLE_FILLER_DATA)
                | H264_FLAG_OUTPUT_AUD
                | H264_FLAG_REPEAT_SPSPPS;
            if supports_intra_refresh {
                h264.flags |= H264_FLAG_ENABLE_INTRA_REFRESH;
                h264.idr_period = NVENC_INFINITE_GOPLENGTH;
                h264.intra_refresh_period = ir_period;
                h264.intra_refresh_cnt = ir_cnt;
            } else {
                h264.flags &= !H264_FLAG_ENABLE_INTRA_REFRESH;
                h264.idr_period = cfg.gop_length;
            }
            h264.chroma_format_idc = 1;
            apply_bt709_vui(&mut h264.h264_vui_parameters);
        }
        Codec::Hevc => {
            let hevc = unsafe { &mut cfg.encode_codec_config.hevc };
            hevc.flags = (hevc.flags
                & !(HEVC_CHROMA_FORMAT_MASK
                    | HEVC_BIT_DEPTH_MINUS8_MASK
                    | HEVC_FLAG_ENABLE_FILLER_DATA))
                | HEVC_FLAG_OUTPUT_AUD
                | HEVC_FLAG_REPEAT_SPSPPS
                | (1u32 << HEVC_CHROMA_FORMAT_SHIFT);
            if supports_intra_refresh {
                hevc.flags |= HEVC_FLAG_ENABLE_INTRA_REFRESH;
                hevc.idr_period = NVENC_INFINITE_GOPLENGTH;
                hevc.intra_refresh_period = ir_period;
                hevc.intra_refresh_cnt = ir_cnt;
            } else {
                hevc.flags &= !HEVC_FLAG_ENABLE_INTRA_REFRESH;
                hevc.idr_period = cfg.gop_length;
            }
            apply_bt709_vui(&mut hevc.hevc_vui_parameters);
        }
        Codec::Av1 => {
            let av1 = unsafe { &mut cfg.encode_codec_config.av1 };
            av1.flags = (av1.flags
                & !(AV1_FLAG_OUTPUT_ANNEX_B
                    | AV1_CHROMA_FORMAT_MASK
                    | AV1_INPUT_BIT_DEPTH_MINUS8_MASK
                    | AV1_PIXEL_BIT_DEPTH_MINUS8_MASK))
                | AV1_FLAG_REPEAT_SEQ_HDR
                | (1u32 << AV1_CHROMA_FORMAT_SHIFT);
            if supports_intra_refresh {
                av1.flags |= AV1_FLAG_ENABLE_INTRA_REFRESH;
                av1.idr_period = NVENC_INFINITE_GOPLENGTH;
                av1.intra_refresh_period = ir_period;
                av1.intra_refresh_cnt = ir_cnt;
            } else {
                av1.flags &= !AV1_FLAG_ENABLE_INTRA_REFRESH;
                av1.idr_period = cfg.gop_length;
            }
            av1.color_primaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
            av1.transfer_characteristics = NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
            av1.matrix_coefficients = NV_ENC_VUI_MATRIX_COEFFS_BT709;
            av1.color_range = 0; // studio swing
        }
    }
}

fn apply_bt709_vui(vui: &mut NvEncConfigVuiParameters) {
    vui.video_signal_type_present_flag = 1;
    vui.video_format = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
    vui.video_full_range_flag = 0;
    vui.colour_description_present_flag = 1;
    vui.colour_primaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
    vui.transfer_characteristics = NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
    vui.colour_matrix = NV_ENC_VUI_MATRIX_COEFFS_BT709;
}

/// Updates rate-control fields in place for live `NvEncReconfigureEncoder`.
pub fn apply_live_bitrate(cfg: &mut NvEncConfig, fps: u16, bitrate_bps: u32) {
    let vbv_bits = single_frame_vbv_bits(bitrate_bps, fps);
    cfg.rc_params.average_bit_rate = bitrate_bps;
    cfg.rc_params.max_bit_rate = bitrate_bps;
    cfg.rc_params.vbv_buffer_size = vbv_bits;
    cfg.rc_params.vbv_initial_delay = vbv_bits;
}
