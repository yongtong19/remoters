//! NVENC Video Codec SDK 12.1 / 12.0 C ABI structures, GUIDs, version
//! negotiation, and pure rate-control / format-mapping helpers.
//!
//! Kept free of Win32/D3D11 crate dependencies so ABI size/offset assertions
//! and rate-control unit tests run cleanly cross-platform.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use std::ffi::c_void;

// ---------------------------------------------------------------------------
// NVENC API Version & Struct Versioning (NVENC Video Codec SDK 12.1 / 12.0)
// ---------------------------------------------------------------------------

pub const NVENC_INFINITE_GOPLENGTH: u32 = 0xffff_ffff;

pub const fn nvenc_api_version(major: u32, minor: u32) -> u32 {
    major | (minor << 24)
}

pub const fn nvenc_struct_version(api_version: u32, struct_ver: u32) -> u32 {
    api_version | (struct_ver << 16) | (0x7 << 28)
}

/// Decodes the packed version returned by `NvEncodeAPIGetMaxSupportedVersion`
/// (`(major << 4) | minor`).
pub const fn decode_max_supported_version(packed: u32) -> (u32, u32) {
    (packed >> 4, packed & 0x0f)
}

/// Negotiated NVENC API struct version tokens matching the runtime driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NvencApiVersions {
    pub major: u32,
    pub minor: u32,
    pub api_version: u32,
    pub function_list_ver: u32,
    pub open_session_ex_ver: u32,
    pub caps_param_ver: u32,
    pub preset_config_ver: u32,
    pub config_ver: u32,
    pub rc_params_ver: u32,
    pub init_params_ver: u32,
    pub reconfigure_params_ver: u32,
    pub register_resource_ver: u32,
    pub map_input_resource_ver: u32,
    pub create_bitstream_ver: u32,
    pub lock_bitstream_ver: u32,
    pub pic_params_ver: u32,
}

impl NvencApiVersions {
    /// Negotiates the highest ABI-matched NVENC API version supported by the
    /// installed NVIDIA driver. Supports 12.1 (R531.61+ through Blackwell R570+)
    /// and 12.0 (R522.25+).
    pub const fn negotiate(max_supported_packed: u32) -> Option<Self> {
        let (driver_major, driver_minor) = decode_max_supported_version(max_supported_packed);
        if driver_major > 12 || (driver_major == 12 && driver_minor >= 1) {
            Some(Self::v12_1())
        } else if driver_major == 12 {
            Some(Self::v12_0())
        } else {
            None
        }
    }

    pub const fn v12_1() -> Self {
        let v = nvenc_api_version(12, 1);
        Self {
            major: 12,
            minor: 1,
            api_version: v,
            function_list_ver: nvenc_struct_version(v, 2),
            open_session_ex_ver: nvenc_struct_version(v, 1),
            caps_param_ver: nvenc_struct_version(v, 1),
            preset_config_ver: nvenc_struct_version(v, 4) | (1 << 31),
            config_ver: nvenc_struct_version(v, 8) | (1 << 31),
            rc_params_ver: nvenc_struct_version(v, 1),
            init_params_ver: nvenc_struct_version(v, 6) | (1 << 31),
            reconfigure_params_ver: nvenc_struct_version(v, 1) | (1 << 31),
            register_resource_ver: nvenc_struct_version(v, 4),
            map_input_resource_ver: nvenc_struct_version(v, 4),
            create_bitstream_ver: nvenc_struct_version(v, 1),
            lock_bitstream_ver: nvenc_struct_version(v, 1) | (1 << 31),
            pic_params_ver: nvenc_struct_version(v, 6) | (1 << 31),
        }
    }

    pub const fn v12_0() -> Self {
        let v = nvenc_api_version(12, 0);
        Self {
            major: 12,
            minor: 0,
            api_version: v,
            function_list_ver: nvenc_struct_version(v, 2),
            open_session_ex_ver: nvenc_struct_version(v, 1),
            caps_param_ver: nvenc_struct_version(v, 1),
            preset_config_ver: nvenc_struct_version(v, 4) | (1 << 31),
            config_ver: nvenc_struct_version(v, 8) | (1 << 31),
            rc_params_ver: nvenc_struct_version(v, 1),
            init_params_ver: nvenc_struct_version(v, 5) | (1 << 31),
            reconfigure_params_ver: nvenc_struct_version(v, 1) | (1 << 31),
            register_resource_ver: nvenc_struct_version(v, 4),
            map_input_resource_ver: nvenc_struct_version(v, 4),
            create_bitstream_ver: nvenc_struct_version(v, 1),
            lock_bitstream_ver: nvenc_struct_version(v, 2),
            pic_params_ver: nvenc_struct_version(v, 6) | (1 << 31),
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder Selection & Rate-Control Helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WinEncoderPreference {
    Auto,
    Nvenc,
    Mft,
}

pub fn parse_encoder_preference(value: Option<&str>) -> WinEncoderPreference {
    match value.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("mft") => WinEncoderPreference::Mft,
        Some(v) if v.eq_ignore_ascii_case("nvenc") => WinEncoderPreference::Nvenc,
        _ => WinEncoderPreference::Auto,
    }
}

pub fn env_encoder_preference() -> WinEncoderPreference {
    let val = std::env::var("REMOTERS_WIN_ENCODER").ok();
    parse_encoder_preference(val.as_deref())
}

/// Sunshine-grade Single-Frame VBV in bits: `(bitrate / fps) * 1.25`.
/// Keeps per-frame burst size tightly bounded even after static desktop periods.
pub fn nvenc_single_frame_vbv_bits(bitrate: u32, fps: u32) -> u32 {
    let fps = u64::from(fps.max(1));
    let frame_bits = u64::from(bitrate) / fps;
    let vbv = frame_bits.saturating_mul(5) / 4;
    let min_vbv = 64 * 1024u64;
    let max_vbv = u64::from(bitrate).max(min_vbv);
    vbv.clamp(min_vbv, max_vbv) as u32
}

/// Computes periodic intra-refresh `(period_frames, count_frames)` for the
/// given frame rate, ensuring `0 < count < period`.
pub fn nvenc_intra_refresh_params(fps: u32) -> (u32, u32) {
    let fps = fps.max(1);
    let period = fps.saturating_mul(4).max(60);
    let count = (fps / 2).clamp(15, 60).min(period.saturating_sub(1));
    (period, count)
}

/// Maps a WinSDK `DXGI_FORMAT` numeric discriminant to the matching zero-copy
/// `NV_ENC_BUFFER_FORMAT` value.
pub const fn dxgi_format_to_nvenc_buffer_format(dxgi_format: i32) -> Option<u32> {
    match dxgi_format {
        // DXGI_FORMAT_NV12
        103 => Some(NV_ENC_BUFFER_FORMAT_NV12),
        // DXGI_FORMAT_P010
        104 => Some(NV_ENC_BUFFER_FORMAT_YUV420_10BIT),
        // DXGI_FORMAT_B8G8R8A8_UNORM
        87 => Some(NV_ENC_BUFFER_FORMAT_ARGB),
        // DXGI_FORMAT_R10G10B10A2_UNORM
        24 => Some(NV_ENC_BUFFER_FORMAT_ABGR10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// NVENC C ABI Types, GUIDs & Constants (exact match to nvEncodeAPI.h 12.1)
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NvGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

// Codec GUIDs
pub const NV_ENC_CODEC_H264_GUID: NvGuid = NvGuid {
    data1: 0x6bc8_2762,
    data2: 0x4e63,
    data3: 0x4ca4,
    data4: [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
};
pub const NV_ENC_CODEC_HEVC_GUID: NvGuid = NvGuid {
    data1: 0x790c_dc88,
    data2: 0x4522,
    data3: 0x4d7b,
    data4: [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
};
pub const NV_ENC_CODEC_AV1_GUID: NvGuid = NvGuid {
    data1: 0x0a35_2289,
    data2: 0x0aa7,
    data3: 0x4759,
    data4: [0x86, 0x2d, 0x5d, 0x15, 0xcd, 0x16, 0xd2, 0x54],
};

// Profile GUIDs
pub const NV_ENC_CODEC_PROFILE_AUTOSELECT_GUID: NvGuid = NvGuid {
    data1: 0xbfd6_f8e7,
    data2: 0x233c,
    data3: 0x4341,
    data4: [0x8b, 0x3e, 0x48, 0x18, 0x52, 0x38, 0x03, 0xf4],
};
pub const NV_ENC_H264_PROFILE_HIGH_GUID: NvGuid = NvGuid {
    data1: 0xe7cb_c309,
    data2: 0x4f7a,
    data3: 0x4b89,
    data4: [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
};
pub const NV_ENC_HEVC_PROFILE_MAIN_GUID: NvGuid = NvGuid {
    data1: 0xb514_c39a,
    data2: 0xb55b,
    data3: 0x40fa,
    data4: [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
};
pub const NV_ENC_HEVC_PROFILE_MAIN10_GUID: NvGuid = NvGuid {
    data1: 0xfa4d_2b6c,
    data2: 0x3a5b,
    data3: 0x411a,
    data4: [0x80, 0x18, 0x0a, 0x3f, 0x5e, 0x3c, 0x9b, 0xe5],
};
pub const NV_ENC_AV1_PROFILE_MAIN_GUID: NvGuid = NvGuid {
    data1: 0x5f2a_39f5,
    data2: 0xf14e,
    data3: 0x4f95,
    data4: [0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
};

// Preset GUIDs (P1 = fastest ultra-low-latency through P4 = medium)
pub const NV_ENC_PRESET_P1_GUID: NvGuid = NvGuid {
    data1: 0xfc0a_8d3e,
    data2: 0x45f8,
    data3: 0x4cf8,
    data4: [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
};
pub const NV_ENC_PRESET_P2_GUID: NvGuid = NvGuid {
    data1: 0xf581_cfb8,
    data2: 0x88d6,
    data3: 0x4381,
    data4: [0x93, 0xf0, 0xdf, 0x13, 0xf9, 0xc2, 0x7d, 0xab],
};
pub const NV_ENC_PRESET_P4_GUID: NvGuid = NvGuid {
    data1: 0x90a7_b826,
    data2: 0xdf06,
    data3: 0x4862,
    data4: [0xb9, 0xd2, 0xcd, 0x6d, 0x73, 0xa0, 0x86, 0x81],
};

// NVENCSTATUS values
pub type NvencStatus = u32;
pub const NV_ENC_SUCCESS: NvencStatus = 0;
pub const NV_ENC_ERR_NEED_MORE_INPUT: NvencStatus = 17;

// NV_ENC_DEVICE_TYPE
pub const NV_ENC_DEVICE_TYPE_DIRECTX: u32 = 0;

// NV_ENC_INPUT_RESOURCE_TYPE
pub const NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0;

// NV_ENC_BUFFER_USAGE
pub const NV_ENC_INPUT_IMAGE: u32 = 0;

// NV_ENC_BUFFER_FORMAT
pub const NV_ENC_BUFFER_FORMAT_UNDEFINED: u32 = 0x0000_0000;
pub const NV_ENC_BUFFER_FORMAT_NV12: u32 = 0x0000_0001;
pub const NV_ENC_BUFFER_FORMAT_YUV420_10BIT: u32 = 0x0001_0000;
pub const NV_ENC_BUFFER_FORMAT_ARGB: u32 = 0x0100_0000;
pub const NV_ENC_BUFFER_FORMAT_ABGR10: u32 = 0x2000_0000;

// NV_ENC_TUNING_INFO
pub const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;

// NV_ENC_PARAMS_RC_MODE
pub const NV_ENC_PARAMS_RC_CBR: u32 = 2;

// NV_ENC_MULTI_PASS
pub const NV_ENC_MULTI_PASS_DISABLED: u32 = 0;

// NV_ENC_PARAMS_FRAME_FIELD_MODE
pub const NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME: u32 = 1;

// NV_ENC_PIC_STRUCT
pub const NV_ENC_PIC_STRUCT_FRAME: u32 = 1;

// NV_ENC_PIC_TYPE
pub const NV_ENC_PIC_TYPE_P: u32 = 0;
pub const NV_ENC_PIC_TYPE_IDR: u32 = 3;
pub const NV_ENC_PIC_TYPE_UNKNOWN: u32 = 0xff;

// NV_ENC_PIC_FLAGS
pub const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x2;
pub const NV_ENC_PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;

// NV_ENC_CAPS
pub const NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE: u32 = 20;
pub const NV_ENC_CAPS_SUPPORT_INTRA_REFRESH: u32 = 25;
pub const NV_ENC_CAPS_SUPPORT_CUSTOM_VBV_BUF_SIZE: u32 = 26;
pub const NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION: u32 = 28;
pub const NV_ENC_CAPS_SUPPORT_10BIT_ENCODE: u32 = 39;
pub const NV_ENC_CAPS_SINGLE_SLICE_INTRA_REFRESH: u32 = 50;

// VUI colorimetry values (ISO/IEC 23091-4 / ITU-T H.273)
pub const NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED: u32 = 5;
pub const NV_ENC_VUI_COLOR_PRIMARIES_BT709: u32 = 1;
pub const NV_ENC_VUI_COLOR_PRIMARIES_BT2020: u32 = 9;
pub const NV_ENC_VUI_TRANSFER_BT709: u32 = 1;
pub const NV_ENC_VUI_TRANSFER_SMPTE2084: u32 = 16;
pub const NV_ENC_VUI_MATRIX_BT709: u32 = 1;
pub const NV_ENC_VUI_MATRIX_BT2020_NCL: u32 = 9;

// Bitfield masks (verified against nvEncodeAPI.h 12.1)
pub const RC_FLAG_ENABLE_LOOKAHEAD: u32 = 1 << 5;
pub const RC_FLAG_ZERO_REORDER_DELAY: u32 = 1 << 9;

pub const H264_FLAG_ENABLE_INTRA_REFRESH: u32 = 1 << 10;
pub const H264_FLAG_REPEAT_SPSPPS: u32 = 1 << 12;
pub const H264_FLAG_ENABLE_FILLER: u32 = 1 << 17;
pub const H264_FLAG_SINGLE_SLICE_IR: u32 = 1 << 20;

pub const HEVC_FLAG_REPEAT_SPSPPS: u32 = 1 << 7;
pub const HEVC_FLAG_ENABLE_INTRA_REFRESH: u32 = 1 << 8;
pub const HEVC_FLAG_CHROMA_MASK: u32 = 0x3 << 9;
pub const HEVC_FLAG_CHROMA_420: u32 = 1 << 9;
pub const HEVC_FLAG_BIT_DEPTH_MASK: u32 = 0x7 << 11;
pub const HEVC_FLAG_ENABLE_FILLER: u32 = 1 << 14;
pub const HEVC_FLAG_SINGLE_SLICE_IR: u32 = 1 << 17;

pub const AV1_FLAG_OUTPUT_ANNEXB: u32 = 1 << 0;
pub const AV1_FLAG_DISABLE_SEQ_HDR: u32 = 1 << 4;
pub const AV1_FLAG_REPEAT_SEQ_HDR: u32 = 1 << 5;
pub const AV1_FLAG_ENABLE_INTRA_REFRESH: u32 = 1 << 6;
pub const AV1_FLAG_CHROMA_MASK: u32 = 0x3 << 7;
pub const AV1_FLAG_CHROMA_420: u32 = 1 << 7;
pub const AV1_FLAG_ENABLE_PADDING: u32 = 1 << 9;
pub const AV1_FLAG_INPUT_BIT_DEPTH_MASK: u32 = 0x7 << 12;
pub const AV1_FLAG_OUTPUT_BIT_DEPTH_MASK: u32 = 0x7 << 15;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
    pub version: u32,
    pub deviceType: u32,
    pub device: *mut c_void,
    pub reserved: *mut c_void,
    pub apiVersion: u32,
    pub reserved1: [u32; 253],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CAPS_PARAM {
    pub version: u32,
    pub capsToQuery: u32,
    pub reserved: [u32; 62],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NV_ENC_QP {
    pub qpInterP: u32,
    pub qpInterB: u32,
    pub qpIntra: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_RC_PARAMS {
    pub version: u32,
    pub rateControlMode: u32,
    pub constQP: NV_ENC_QP,
    pub averageBitRate: u32,
    pub maxBitRate: u32,
    pub vbvBufferSize: u32,
    pub vbvInitialDelay: u32,
    pub flags: u32,
    pub minQP: NV_ENC_QP,
    pub maxQP: NV_ENC_QP,
    pub initialRCQP: NV_ENC_QP,
    pub temporallayerIdxMask: u32,
    pub temporalLayerQP: [u8; 8],
    pub targetQuality: u8,
    pub targetQualityLSB: u8,
    pub lookaheadDepth: u16,
    pub lowDelayKeyFrameScale: u8,
    pub yDcQPIndexOffset: i8,
    pub uDcQPIndexOffset: i8,
    pub vDcQPIndexOffset: i8,
    pub qpMapMode: u32,
    pub multiPass: u32,
    pub alphaLayerBitrateRatio: u32,
    pub cbQPIndexOffset: i8,
    pub crQPIndexOffset: i8,
    pub reserved2: u16,
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NV_ENC_CONFIG_VUI_PARAMETERS {
    pub overscanInfoPresentFlag: u32,
    pub overscanInfo: u32,
    pub videoSignalTypePresentFlag: u32,
    pub videoFormat: u32,
    pub videoFullRangeFlag: u32,
    pub colourDescriptionPresentFlag: u32,
    pub colourPrimaries: u32,
    pub transferCharacteristics: u32,
    pub colourMatrix: u32,
    pub chromaSampleLocationFlag: u32,
    pub chromaSampleLocationTop: u32,
    pub chromaSampleLocationBot: u32,
    pub bitstreamRestrictionFlag: u32,
    pub timingInfoPresentFlag: u32,
    pub numUnitInTicks: u32,
    pub timeScale: u32,
    pub reserved: [u32; 12],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_H264 {
    pub flags: u32,
    pub level: u32,
    pub idrPeriod: u32,
    pub separateColourPlaneFlag: u32,
    pub disableDeblockingFilterIDC: u32,
    pub numTemporalLayers: u32,
    pub spsId: u32,
    pub ppsId: u32,
    pub adaptiveTransformMode: u32,
    pub fmoMode: u32,
    pub bdirectMode: u32,
    pub entropyCodingMode: u32,
    pub stereoMode: u32,
    pub intraRefreshPeriod: u32,
    pub intraRefreshCnt: u32,
    pub maxNumRefFrames: u32,
    pub sliceMode: u32,
    pub sliceModeData: u32,
    pub h264VUIParameters: NV_ENC_CONFIG_VUI_PARAMETERS,
    pub ltrNumFrames: u32,
    pub ltrTrustMode: u32,
    pub chromaFormatIDC: u32,
    pub maxTemporalLayers: u32,
    pub useBFramesAsRef: u32,
    pub numRefL0: u32,
    pub numRefL1: u32,
    pub reserved1: [u32; 267],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_HEVC {
    pub level: u32,
    pub tier: u32,
    pub minCUSize: u32,
    pub maxCUSize: u32,
    pub flags: u32,
    pub idrPeriod: u32,
    pub intraRefreshPeriod: u32,
    pub intraRefreshCnt: u32,
    pub maxNumRefFramesInDPB: u32,
    pub ltrNumFrames: u32,
    pub vpsId: u32,
    pub spsId: u32,
    pub ppsId: u32,
    pub sliceMode: u32,
    pub sliceModeData: u32,
    pub maxTemporalLayersMinus1: u32,
    pub hevcVUIParameters: NV_ENC_CONFIG_VUI_PARAMETERS,
    pub ltrTrustMode: u32,
    pub useBFramesAsRef: u32,
    pub numRefL0: u32,
    pub numRefL1: u32,
    pub reserved1: [u32; 214],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_AV1 {
    pub level: u32,
    pub tier: u32,
    pub minPartSize: u32,
    pub maxPartSize: u32,
    pub flags: u32,
    pub idrPeriod: u32,
    pub intraRefreshPeriod: u32,
    pub intraRefreshCnt: u32,
    pub maxNumRefFramesInDPB: u32,
    pub numTileColumns: u32,
    pub numTileRows: u32,
    pub tileWidths: *mut u32,
    pub tileHeights: *mut u32,
    pub maxTemporalLayersMinus1: u32,
    pub colorPrimaries: u32,
    pub transferCharacteristics: u32,
    pub matrixCoefficients: u32,
    pub colorRange: u32,
    pub chromaSamplePosition: u32,
    pub useBFramesAsRef: u32,
    pub filmGrainParams: *mut c_void,
    pub numFwdRefs: u32,
    pub numBwdRefs: u32,
    pub reserved1: [u32; 235],
    pub reserved2: [*mut c_void; 62],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union NV_ENC_CODEC_CONFIG {
    pub h264Config: NV_ENC_CONFIG_H264,
    pub hevcConfig: NV_ENC_CONFIG_HEVC,
    pub av1Config: NV_ENC_CONFIG_AV1,
    pub reserved: [u32; 320],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG {
    pub version: u32,
    pub profileGUID: NvGuid,
    pub gopLength: u32,
    pub frameIntervalP: i32,
    pub monoChromeEncoding: u32,
    pub frameFieldMode: u32,
    pub mvPrecision: u32,
    pub rcParams: NV_ENC_RC_PARAMS,
    pub encodeCodecConfig: NV_ENC_CODEC_CONFIG,
    pub reserved: [u32; 278],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_PRESET_CONFIG {
    pub version: u32,
    pub presetCfg: NV_ENC_CONFIG,
    pub reserved1: [u32; 255],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE {
    pub bitfields: u32,
    pub reserved1: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_INITIALIZE_PARAMS {
    pub version: u32,
    pub encodeGUID: NvGuid,
    pub presetGUID: NvGuid,
    pub encodeWidth: u32,
    pub encodeHeight: u32,
    pub darWidth: u32,
    pub darHeight: u32,
    pub frameRateNum: u32,
    pub frameRateDen: u32,
    pub enableEncodeAsync: u32,
    pub enablePTD: u32,
    pub flags: u32,
    pub privDataSize: u32,
    pub privData: *mut c_void,
    pub encodeConfig: *mut NV_ENC_CONFIG,
    pub maxEncodeWidth: u32,
    pub maxEncodeHeight: u32,
    pub maxMEHintCountsPerBlock: [NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE; 2],
    pub tuningInfo: u32,
    pub bufferFormat: u32,
    pub numStateBuffers: u32,
    pub outputStatsLevel: u32,
    pub reserved: [u32; 285],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_RECONFIGURE_PARAMS {
    pub version: u32,
    pub reInitEncodeParams: NV_ENC_INITIALIZE_PARAMS,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_REGISTER_RESOURCE {
    pub version: u32,
    pub resourceType: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub subResourceIndex: u32,
    pub resourceToRegister: *mut c_void,
    pub registeredResource: *mut c_void,
    pub bufferFormat: u32,
    pub bufferUsage: u32,
    pub pInputFencePoint: *mut c_void,
    pub chromaOffset: [u32; 2],
    pub reserved1: [u32; 245],
    pub reserved2: [*mut c_void; 61],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_MAP_INPUT_RESOURCE {
    pub version: u32,
    pub subResourceIndex: u32,
    pub inputResource: *mut c_void,
    pub registeredResource: *mut c_void,
    pub mappedResource: *mut c_void,
    pub mappedBufferFmt: u32,
    pub reserved1: [u32; 251],
    pub reserved2: [*mut c_void; 63],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CREATE_BITSTREAM_BUFFER {
    pub version: u32,
    pub size: u32,
    pub memoryHeap: u32,
    pub reserved: u32,
    pub bitstreamBuffer: *mut c_void,
    pub bitstreamBufferPtr: *mut c_void,
    pub reserved1: [u32; 58],
    pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_LOCK_BITSTREAM {
    pub version: u32,
    pub flags: u32,
    pub outputBitstream: *mut c_void,
    pub sliceOffsets: *mut u32,
    pub frameIdx: u32,
    pub hwEncodeStatus: u32,
    pub numSlices: u32,
    pub bitstreamSizeInBytes: u32,
    pub outputTimeStamp: u64,
    pub outputDuration: u64,
    pub bitstreamBufferPtr: *mut c_void,
    pub pictureType: u32,
    pub pictureStruct: u32,
    pub frameAvgQP: u32,
    pub frameSatd: u32,
    pub ltrFrameIdx: u32,
    pub ltrFrameBitmap: u32,
    pub temporalId: u32,
    pub intraMBCount: u32,
    pub interMBCount: u32,
    pub averageMVX: i32,
    pub averageMVY: i32,
    pub alphaLayerSizeInBytes: u32,
    pub outputStatsPtrSize: u32,
    pub outputStatsPtr: *mut c_void,
    pub frameIdxDisplay: u32,
    pub reserved1: [u32; 220],
    pub reserved2: [*mut c_void; 63],
    pub reservedInternal: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CODEC_PIC_PARAMS_RAW {
    // 1552 bytes, 8-byte aligned — matches `union _NV_ENC_CODEC_PIC_PARAMS`.
    pub raw: [u64; 194],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_PIC_PARAMS {
    pub version: u32,
    pub inputWidth: u32,
    pub inputHeight: u32,
    pub inputPitch: u32,
    pub encodePicFlags: u32,
    pub frameIdx: u32,
    pub inputTimeStamp: u64,
    pub inputDuration: u64,
    pub inputBuffer: *mut c_void,
    pub outputBitstream: *mut c_void,
    pub completionEvent: *mut c_void,
    pub bufferFmt: u32,
    pub pictureStruct: u32,
    pub pictureType: u32,
    pub codecPicParams: NV_ENC_CODEC_PIC_PARAMS_RAW,
    pub meHintCountsPerBlock: [NVENC_EXTERNAL_ME_HINT_COUNTS_PER_BLOCKTYPE; 2],
    pub meExternalHints: *mut c_void,
    pub reserved1: [u32; 6],
    pub reserved2: [*mut c_void; 2],
    pub qpDeltaMap: *mut i8,
    pub qpDeltaMapSize: u32,
    pub reservedBitFields: u32,
    pub meHintRefPicDist: [u16; 2],
    pub alphaBuffer: *mut c_void,
    pub meExternalSbHints: *mut c_void,
    pub meSbHintsCount: u32,
    pub stateBufferIdx: u32,
    pub outputReconBuffer: *mut c_void,
    pub reserved3: [u32; 284],
    pub reserved4: [*mut c_void; 57],
}

// Function pointer typedefs matching NVENCAPI (__stdcall / extern "system")
pub type PNVENCOPENENCODESESSION =
    Option<unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> NvencStatus>;
pub type PNVENCGETENCODEGUIDCOUNT =
    Option<unsafe extern "system" fn(*mut c_void, *mut u32) -> NvencStatus>;
pub type PNVENCGETENCODEGUIDS =
    Option<unsafe extern "system" fn(*mut c_void, *mut NvGuid, u32, *mut u32) -> NvencStatus>;
pub type PNVENCGETINPUTFORMATCOUNT =
    Option<unsafe extern "system" fn(*mut c_void, NvGuid, *mut u32) -> NvencStatus>;
pub type PNVENCGETINPUTFORMATS =
    Option<unsafe extern "system" fn(*mut c_void, NvGuid, *mut u32, u32, *mut u32) -> NvencStatus>;
pub type PNVENCGETENCODECAPS = Option<
    unsafe extern "system" fn(*mut c_void, NvGuid, *mut NV_ENC_CAPS_PARAM, *mut i32) -> NvencStatus,
>;
pub type PNVENCGETENCODEPRESETCONFIGEX = Option<
    unsafe extern "system" fn(
        *mut c_void,
        NvGuid,
        NvGuid,
        u32,
        *mut NV_ENC_PRESET_CONFIG,
    ) -> NvencStatus,
>;
pub type PNVENCINITIALIZEENCODER =
    Option<unsafe extern "system" fn(*mut c_void, *mut NV_ENC_INITIALIZE_PARAMS) -> NvencStatus>;
pub type PNVENCCREATEBITSTREAMBUFFER = Option<
    unsafe extern "system" fn(*mut c_void, *mut NV_ENC_CREATE_BITSTREAM_BUFFER) -> NvencStatus,
>;
pub type PNVENCDESTROYBITSTREAMBUFFER =
    Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> NvencStatus>;
pub type PNVENCENCODEPICTURE =
    Option<unsafe extern "system" fn(*mut c_void, *mut NV_ENC_PIC_PARAMS) -> NvencStatus>;
pub type PNVENCLOCKBITSTREAM =
    Option<unsafe extern "system" fn(*mut c_void, *mut NV_ENC_LOCK_BITSTREAM) -> NvencStatus>;
pub type PNVENCUNLOCKBITSTREAM =
    Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> NvencStatus>;
pub type PNVENCMAPINPUTRESOURCE =
    Option<unsafe extern "system" fn(*mut c_void, *mut NV_ENC_MAP_INPUT_RESOURCE) -> NvencStatus>;
pub type PNVENCUNMAPINPUTRESOURCE =
    Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> NvencStatus>;
pub type PNVENCDESTROYENCODER = Option<unsafe extern "system" fn(*mut c_void) -> NvencStatus>;
pub type PNVENCINVALIDATEREFFRAMES =
    Option<unsafe extern "system" fn(*mut c_void, u64) -> NvencStatus>;
pub type PNVENCOPENENCODESESSIONEX = Option<
    unsafe extern "system" fn(
        *mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
        *mut *mut c_void,
    ) -> NvencStatus,
>;
pub type PNVENCREGISTERRESOURCE =
    Option<unsafe extern "system" fn(*mut c_void, *mut NV_ENC_REGISTER_RESOURCE) -> NvencStatus>;
pub type PNVENCUNREGISTERRESOURCE =
    Option<unsafe extern "system" fn(*mut c_void, *mut c_void) -> NvencStatus>;
pub type PNVENCRECONFIGUREENCODER =
    Option<unsafe extern "system" fn(*mut c_void, *mut NV_ENC_RECONFIGURE_PARAMS) -> NvencStatus>;
pub type PNVENCGETLASTERROR = Option<unsafe extern "system" fn(*mut c_void) -> *const i8>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENCODE_API_FUNCTION_LIST {
    pub version: u32,
    pub reserved: u32,
    pub nvEncOpenEncodeSession: PNVENCOPENENCODESESSION,
    pub nvEncGetEncodeGUIDCount: PNVENCGETENCODEGUIDCOUNT,
    pub nvEncGetEncodeProfileGUIDCount: *mut c_void,
    pub nvEncGetEncodeProfileGUIDs: *mut c_void,
    pub nvEncGetEncodeGUIDs: PNVENCGETENCODEGUIDS,
    pub nvEncGetInputFormatCount: PNVENCGETINPUTFORMATCOUNT,
    pub nvEncGetInputFormats: PNVENCGETINPUTFORMATS,
    pub nvEncGetEncodeCaps: PNVENCGETENCODECAPS,
    pub nvEncGetEncodePresetCount: *mut c_void,
    pub nvEncGetEncodePresetGUIDs: *mut c_void,
    pub nvEncGetEncodePresetConfig: *mut c_void,
    pub nvEncInitializeEncoder: PNVENCINITIALIZEENCODER,
    pub nvEncCreateInputBuffer: *mut c_void,
    pub nvEncDestroyInputBuffer: *mut c_void,
    pub nvEncCreateBitstreamBuffer: PNVENCCREATEBITSTREAMBUFFER,
    pub nvEncDestroyBitstreamBuffer: PNVENCDESTROYBITSTREAMBUFFER,
    pub nvEncEncodePicture: PNVENCENCODEPICTURE,
    pub nvEncLockBitstream: PNVENCLOCKBITSTREAM,
    pub nvEncUnlockBitstream: PNVENCUNLOCKBITSTREAM,
    pub nvEncLockInputBuffer: *mut c_void,
    pub nvEncUnlockInputBuffer: *mut c_void,
    pub nvEncGetEncodeStats: *mut c_void,
    pub nvEncGetSequenceParams: *mut c_void,
    pub nvEncRegisterAsyncEvent: *mut c_void,
    pub nvEncUnregisterAsyncEvent: *mut c_void,
    pub nvEncMapInputResource: PNVENCMAPINPUTRESOURCE,
    pub nvEncUnmapInputResource: PNVENCUNMAPINPUTRESOURCE,
    pub nvEncDestroyEncoder: PNVENCDESTROYENCODER,
    pub nvEncInvalidateRefFrames: PNVENCINVALIDATEREFFRAMES,
    pub nvEncOpenEncodeSessionEx: PNVENCOPENENCODESESSIONEX,
    pub nvEncRegisterResource: PNVENCREGISTERRESOURCE,
    pub nvEncUnregisterResource: PNVENCUNREGISTERRESOURCE,
    pub nvEncReconfigureEncoder: PNVENCRECONFIGUREENCODER,
    pub reserved1: *mut c_void,
    pub nvEncCreateMVBuffer: *mut c_void,
    pub nvEncDestroyMVBuffer: *mut c_void,
    pub nvEncRunMotionEstimationOnly: *mut c_void,
    pub nvEncGetLastErrorString: PNVENCGETLASTERROR,
    pub nvEncSetIOCudaStreams: *mut c_void,
    pub nvEncGetEncodePresetConfigEx: PNVENCGETENCODEPRESETCONFIGEX,
    pub nvEncGetSequenceParamEx: *mut c_void,
    pub nvEncRestoreEncoderState: *mut c_void,
    pub nvEncLookaheadPicture: *mut c_void,
    pub reserved2: [*mut c_void; 275],
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    #[test]
    fn nvenc_12_1_abi_sizes_and_offsets_match_sdk_header() {
        assert_eq!(size_of::<NvGuid>(), 16);
        assert_eq!(size_of::<NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS>(), 1552);
        assert_eq!(
            offset_of!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, deviceType),
            4
        );
        assert_eq!(offset_of!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, device), 8);
        assert_eq!(
            offset_of!(NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS, apiVersion),
            24
        );

        assert_eq!(size_of::<NV_ENC_CAPS_PARAM>(), 256);
        assert_eq!(size_of::<NV_ENC_QP>(), 12);

        assert_eq!(size_of::<NV_ENC_RC_PARAMS>(), 128);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, rateControlMode), 4);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, averageBitRate), 20);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, maxBitRate), 24);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, vbvBufferSize), 28);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, vbvInitialDelay), 32);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, flags), 36);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, targetQuality), 88);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, lowDelayKeyFrameScale), 92);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, multiPass), 100);
        assert_eq!(offset_of!(NV_ENC_RC_PARAMS, reserved), 112);

        assert_eq!(size_of::<NV_ENC_CONFIG_VUI_PARAMETERS>(), 112);
        assert_eq!(size_of::<NV_ENC_CONFIG_H264>(), 1792);
        assert_eq!(offset_of!(NV_ENC_CONFIG_H264, idrPeriod), 8);
        assert_eq!(offset_of!(NV_ENC_CONFIG_H264, intraRefreshPeriod), 52);
        assert_eq!(offset_of!(NV_ENC_CONFIG_H264, intraRefreshCnt), 56);
        assert_eq!(offset_of!(NV_ENC_CONFIG_H264, h264VUIParameters), 72);
        assert_eq!(offset_of!(NV_ENC_CONFIG_H264, reserved1), 212);

        assert_eq!(size_of::<NV_ENC_CONFIG_HEVC>(), 1560);
        assert_eq!(offset_of!(NV_ENC_CONFIG_HEVC, flags), 16);
        assert_eq!(offset_of!(NV_ENC_CONFIG_HEVC, idrPeriod), 20);
        assert_eq!(offset_of!(NV_ENC_CONFIG_HEVC, intraRefreshPeriod), 24);
        assert_eq!(offset_of!(NV_ENC_CONFIG_HEVC, intraRefreshCnt), 28);
        assert_eq!(offset_of!(NV_ENC_CONFIG_HEVC, hevcVUIParameters), 64);
        assert_eq!(offset_of!(NV_ENC_CONFIG_HEVC, reserved1), 192);

        assert_eq!(size_of::<NV_ENC_CONFIG_AV1>(), 1552);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, flags), 16);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, idrPeriod), 20);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, intraRefreshPeriod), 24);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, intraRefreshCnt), 28);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, colorPrimaries), 68);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, transferCharacteristics), 72);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, matrixCoefficients), 76);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, colorRange), 80);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, chromaSamplePosition), 84);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, filmGrainParams), 96);
        assert_eq!(offset_of!(NV_ENC_CONFIG_AV1, reserved1), 112);

        assert_eq!(size_of::<NV_ENC_CODEC_CONFIG>(), 1792);
        assert_eq!(size_of::<NV_ENC_CONFIG>(), 3584);
        assert_eq!(offset_of!(NV_ENC_CONFIG, profileGUID), 4);
        assert_eq!(offset_of!(NV_ENC_CONFIG, gopLength), 20);
        assert_eq!(offset_of!(NV_ENC_CONFIG, frameIntervalP), 24);
        assert_eq!(offset_of!(NV_ENC_CONFIG, rcParams), 40);
        assert_eq!(offset_of!(NV_ENC_CONFIG, encodeCodecConfig), 168);
        assert_eq!(offset_of!(NV_ENC_CONFIG, reserved), 1960);

        assert_eq!(size_of::<NV_ENC_PRESET_CONFIG>(), 5128);
        assert_eq!(offset_of!(NV_ENC_PRESET_CONFIG, presetCfg), 8);
        assert_eq!(offset_of!(NV_ENC_PRESET_CONFIG, reserved1), 3592);

        assert_eq!(size_of::<NV_ENC_INITIALIZE_PARAMS>(), 1808);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeGUID), 4);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, presetGUID), 20);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeWidth), 36);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, frameRateNum), 52);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, enablePTD), 64);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeConfig), 88);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, tuningInfo), 136);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, bufferFormat), 140);
        assert_eq!(offset_of!(NV_ENC_INITIALIZE_PARAMS, reserved), 152);

        assert_eq!(size_of::<NV_ENC_RECONFIGURE_PARAMS>(), 1824);
        assert_eq!(offset_of!(NV_ENC_RECONFIGURE_PARAMS, reInitEncodeParams), 8);

        assert_eq!(size_of::<NV_ENC_REGISTER_RESOURCE>(), 1536);
        assert_eq!(offset_of!(NV_ENC_REGISTER_RESOURCE, resourceType), 4);
        assert_eq!(offset_of!(NV_ENC_REGISTER_RESOURCE, resourceToRegister), 24);
        assert_eq!(offset_of!(NV_ENC_REGISTER_RESOURCE, registeredResource), 32);
        assert_eq!(offset_of!(NV_ENC_REGISTER_RESOURCE, bufferFormat), 40);
        assert_eq!(offset_of!(NV_ENC_REGISTER_RESOURCE, bufferUsage), 44);
        assert_eq!(offset_of!(NV_ENC_REGISTER_RESOURCE, pInputFencePoint), 48);
        assert_eq!(offset_of!(NV_ENC_REGISTER_RESOURCE, reserved1), 64);

        assert_eq!(size_of::<NV_ENC_MAP_INPUT_RESOURCE>(), 1544);
        assert_eq!(
            offset_of!(NV_ENC_MAP_INPUT_RESOURCE, registeredResource),
            16
        );
        assert_eq!(offset_of!(NV_ENC_MAP_INPUT_RESOURCE, mappedResource), 24);
        assert_eq!(offset_of!(NV_ENC_MAP_INPUT_RESOURCE, mappedBufferFmt), 32);

        assert_eq!(size_of::<NV_ENC_CREATE_BITSTREAM_BUFFER>(), 776);
        assert_eq!(
            offset_of!(NV_ENC_CREATE_BITSTREAM_BUFFER, bitstreamBuffer),
            16
        );

        assert_eq!(size_of::<NV_ENC_LOCK_BITSTREAM>(), 1552);
        assert_eq!(offset_of!(NV_ENC_LOCK_BITSTREAM, outputBitstream), 8);
        assert_eq!(offset_of!(NV_ENC_LOCK_BITSTREAM, bitstreamSizeInBytes), 36);
        assert_eq!(offset_of!(NV_ENC_LOCK_BITSTREAM, outputTimeStamp), 40);
        assert_eq!(offset_of!(NV_ENC_LOCK_BITSTREAM, bitstreamBufferPtr), 56);
        assert_eq!(offset_of!(NV_ENC_LOCK_BITSTREAM, pictureType), 64);
        assert_eq!(offset_of!(NV_ENC_LOCK_BITSTREAM, reserved1), 132);

        assert_eq!(size_of::<NV_ENC_CODEC_PIC_PARAMS_RAW>(), 1552);
        assert_eq!(size_of::<NV_ENC_PIC_PARAMS>(), 3360);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, inputWidth), 4);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, encodePicFlags), 16);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, inputTimeStamp), 24);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, inputBuffer), 40);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, outputBitstream), 48);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, completionEvent), 56);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, bufferFmt), 64);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, pictureStruct), 68);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, pictureType), 72);
        assert_eq!(offset_of!(NV_ENC_PIC_PARAMS, codecPicParams), 80);

        assert_eq!(size_of::<NV_ENCODE_API_FUNCTION_LIST>(), 2552);
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncOpenEncodeSession),
            8
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncInitializeEncoder),
            96
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncMapInputResource),
            208
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncUnmapInputResource),
            216
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncDestroyEncoder),
            224
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncOpenEncodeSessionEx),
            240
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncRegisterResource),
            248
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncUnregisterResource),
            256
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncReconfigureEncoder),
            264
        );
        assert_eq!(
            offset_of!(NV_ENCODE_API_FUNCTION_LIST, nvEncGetEncodePresetConfigEx),
            320
        );
        assert_eq!(offset_of!(NV_ENCODE_API_FUNCTION_LIST, reserved2), 352);
    }

    #[test]
    fn nvenc_version_negotiation_handles_12_x_and_newer_drivers() {
        // Blackwell R570+ driver reporting 13.0 negotiates 12.1.
        let v13 = NvencApiVersions::negotiate((13 << 4) | 0).unwrap();
        assert_eq!((v13.major, v13.minor), (12, 1));
        assert_eq!(v13.api_version, 12 | (1 << 24));

        // 12.2 driver negotiates 12.1.
        let v12_2 = NvencApiVersions::negotiate((12 << 4) | 2).unwrap();
        assert_eq!(v12_2, NvencApiVersions::v12_1());

        // 12.0 driver negotiates 12.0.
        let v12_0 = NvencApiVersions::negotiate((12 << 4) | 0).unwrap();
        assert_eq!(v12_0, NvencApiVersions::v12_0());
        assert_ne!(v12_0.init_params_ver, v12_2.init_params_ver);
        assert_ne!(v12_0.lock_bitstream_ver, v12_2.lock_bitstream_ver);

        // Pre-12.0 driver returns None so caller falls back to MFT.
        assert!(NvencApiVersions::negotiate((11 << 4) | 1).is_none());
    }

    #[test]
    fn single_frame_vbv_and_intra_refresh_calculations() {
        let vbv_4k60 = nvenc_single_frame_vbv_bits(50_000_000, 60);
        assert_eq!(vbv_4k60, (50_000_000u64 / 60 * 5 / 4) as u32);

        let vbv_1080p60 = nvenc_single_frame_vbv_bits(20_000_000, 60);
        assert_eq!(vbv_1080p60, (20_000_000u64 / 60 * 5 / 4) as u32);

        assert_eq!(nvenc_single_frame_vbv_bits(1_000_000, 120), 64 * 1024);

        let (p60, c60) = nvenc_intra_refresh_params(60);
        assert_eq!((p60, c60), (240, 30));
        let (p120, c120) = nvenc_intra_refresh_params(120);
        assert_eq!((p120, c120), (480, 60));
        assert!(c60 > 0 && c60 < p60);
        assert!(c120 > 0 && c120 < p120);
    }

    #[test]
    fn dxgi_format_mapping_and_encoder_preference_parsing() {
        assert_eq!(
            dxgi_format_to_nvenc_buffer_format(103),
            Some(NV_ENC_BUFFER_FORMAT_NV12)
        );
        assert_eq!(
            dxgi_format_to_nvenc_buffer_format(104),
            Some(NV_ENC_BUFFER_FORMAT_YUV420_10BIT)
        );
        assert_eq!(
            dxgi_format_to_nvenc_buffer_format(87),
            Some(NV_ENC_BUFFER_FORMAT_ARGB)
        );
        assert_eq!(
            dxgi_format_to_nvenc_buffer_format(24),
            Some(NV_ENC_BUFFER_FORMAT_ABGR10)
        );
        assert_eq!(dxgi_format_to_nvenc_buffer_format(10), None);

        assert_eq!(parse_encoder_preference(None), WinEncoderPreference::Auto);
        assert_eq!(
            parse_encoder_preference(Some("mft")),
            WinEncoderPreference::Mft
        );
        assert_eq!(
            parse_encoder_preference(Some("MFT")),
            WinEncoderPreference::Mft
        );
        assert_eq!(
            parse_encoder_preference(Some("nvenc")),
            WinEncoderPreference::Nvenc
        );
        assert_eq!(
            parse_encoder_preference(Some("auto")),
            WinEncoderPreference::Auto
        );
    }
}
