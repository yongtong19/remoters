//! Direct NVIDIA NVENC runtime loader & D3D11 hardware video encoder.
//!
//! Dynamically loads `nvEncodeAPI64.dll` via `LoadLibraryW` / `GetProcAddress`,
//! registers `ID3D11Texture2D` surfaces zero-copy (`NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX`),
//! and configures Sunshine-grade ultra-low-latency CBR + single-frame VBV +
//! periodic intra refresh with live `NvEncReconfigureEncoder` bitrate updates.

use std::ffi::{CStr, c_void};

use anyhow::{Context, Result, anyhow, bail, ensure};
use gpui::{ID3D11Device, ID3D11Texture2D};
use remoters_core::video::Codec;
use windows_core::Interface;

use super::nvenc_ffi::*;

type PfnGetMaxSupportedVersion = unsafe extern "system" fn(*mut u32) -> NvencStatus;
type PfnCreateInstance = unsafe extern "system" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NvencStatus;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(lp_lib_file_name: *const u16) -> *mut c_void;
    fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const u8) -> *mut c_void;
    fn FreeLibrary(h_lib_module: *mut c_void) -> i32;
}

struct NvencLibrary {
    module: *mut c_void,
    versions: NvencApiVersions,
    fns: NV_ENCODE_API_FUNCTION_LIST,
}

impl NvencLibrary {
    fn load() -> Result<Self> {
        let dll_name: Vec<u16> = "nvEncodeAPI64.dll\0".encode_utf16().collect();
        let module = unsafe { LoadLibraryW(dll_name.as_ptr()) };
        if module.is_null() {
            bail!("nvEncodeAPI64.dll not found (non-NVIDIA GPU or driver unavailable).");
        }

        let load_result = (|| -> Result<(NvencApiVersions, NV_ENCODE_API_FUNCTION_LIST)> {
            let get_max_raw =
                unsafe { GetProcAddress(module, b"NvEncodeAPIGetMaxSupportedVersion\0".as_ptr()) };
            ensure!(
                !get_max_raw.is_null(),
                "nvEncodeAPI64.dll missing NvEncodeAPIGetMaxSupportedVersion."
            );
            let get_max: PfnGetMaxSupportedVersion = unsafe { std::mem::transmute(get_max_raw) };

            let mut max_ver = 0u32;
            let st = unsafe { get_max(&mut max_ver) };
            ensure!(
                st == NV_ENC_SUCCESS,
                "NvEncodeAPIGetMaxSupportedVersion failed (status={st})."
            );
            let (drv_major, drv_minor) = decode_max_supported_version(max_ver);
            let versions = NvencApiVersions::negotiate(max_ver).ok_or_else(|| {
                anyhow!(
                    "Installed NVIDIA NVENC driver API {drv_major}.{drv_minor} is older than required 12.0."
                )
            })?;

            let create_raw =
                unsafe { GetProcAddress(module, b"NvEncodeAPICreateInstance\0".as_ptr()) };
            ensure!(
                !create_raw.is_null(),
                "nvEncodeAPI64.dll missing NvEncodeAPICreateInstance."
            );
            let create_instance: PfnCreateInstance = unsafe { std::mem::transmute(create_raw) };

            let mut fns: NV_ENCODE_API_FUNCTION_LIST = unsafe { std::mem::zeroed() };
            fns.version = versions.function_list_ver;
            let st = unsafe { create_instance(&mut fns) };
            ensure!(
                st == NV_ENC_SUCCESS,
                "NvEncodeAPICreateInstance failed for NVENC {}.{} (status={st}).",
                versions.major,
                versions.minor
            );
            Ok((versions, fns))
        })();

        match load_result {
            Ok((versions, fns)) => Ok(Self {
                module,
                versions,
                fns,
            }),
            Err(err) => {
                unsafe { FreeLibrary(module) };
                Err(err)
            }
        }
    }
}

impl Drop for NvencLibrary {
    fn drop(&mut self) {
        if !self.module.is_null() {
            unsafe { FreeLibrary(self.module) };
            self.module = std::ptr::null_mut();
        }
    }
}

struct RegisteredSurface {
    raw_ptr: usize,
    // Retain a COM refcount so the underlying ID3D11Texture2D cannot be
    // freed before `nvEncUnregisterResource` runs.
    _texture: ID3D11Texture2D,
    registered: *mut c_void,
    buffer_format: u32,
}

struct UnmapGuard<'a> {
    encoder: *mut c_void,
    unmap_fn: PNVENCUNMAPINPUTRESOURCE,
    mapped: *mut c_void,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl Drop for UnmapGuard<'_> {
    fn drop(&mut self) {
        if !self.mapped.is_null()
            && let Some(unmap) = self.unmap_fn
        {
            let _ = unsafe { unmap(self.encoder, self.mapped) };
        }
    }
}

struct UnlockGuard<'a> {
    encoder: *mut c_void,
    unlock_fn: PNVENCUNLOCKBITSTREAM,
    bitstream: *mut c_void,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl Drop for UnlockGuard<'_> {
    fn drop(&mut self) {
        if !self.bitstream.is_null()
            && let Some(unlock) = self.unlock_fn
        {
            let _ = unsafe { unlock(self.encoder, self.bitstream) };
        }
    }
}

pub(super) struct NvencEncoder {
    // Declared before `lib` so NVENC session teardown happens before FreeLibrary.
    registered: Vec<RegisteredSurface>,
    bitstream: *mut c_void,
    encoder: *mut c_void,
    encode_config: Box<NV_ENC_CONFIG>,
    init_params: NV_ENC_INITIALIZE_PARAMS,
    _device: ID3D11Device,
    lib: NvencLibrary,
    width: u32,
    height: u32,
    fps: u32,
    use_p010: bool,
    buffer_format: u32,
    supports_dyn_bitrate: bool,
    #[allow(dead_code)]
    supports_ref_pic_invalidation: bool,
    frame_idx: u32,
}

fn codec_to_guid(codec: Codec) -> NvGuid {
    match codec {
        Codec::H264 => NV_ENC_CODEC_H264_GUID,
        Codec::Hevc => NV_ENC_CODEC_HEVC_GUID,
        Codec::Av1 => NV_ENC_CODEC_AV1_GUID,
    }
}

fn nvenc_last_error(fns: &NV_ENCODE_API_FUNCTION_LIST, encoder: *mut c_void) -> String {
    if encoder.is_null() {
        return String::new();
    }
    let Some(get_err) = fns.nvEncGetLastErrorString else {
        return String::new();
    };
    let ptr = unsafe { get_err(encoder) };
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .trim()
        .to_owned()
}

fn check_status(
    fns: &NV_ENCODE_API_FUNCTION_LIST,
    encoder: *mut c_void,
    status: NvencStatus,
    op: &str,
) -> Result<()> {
    if status == NV_ENC_SUCCESS {
        return Ok(());
    }
    let detail = nvenc_last_error(fns, encoder);
    if detail.is_empty() {
        bail!("{op} failed (NVENCSTATUS={status}).")
    } else {
        bail!("{op} failed (NVENCSTATUS={status}: {detail}).")
    }
}

fn query_cap(
    fns: &NV_ENCODE_API_FUNCTION_LIST,
    versions: NvencApiVersions,
    encoder: *mut c_void,
    codec_guid: NvGuid,
    cap: u32,
) -> i32 {
    let Some(get_caps) = fns.nvEncGetEncodeCaps else {
        return 0;
    };
    let mut param: NV_ENC_CAPS_PARAM = unsafe { std::mem::zeroed() };
    param.version = versions.caps_param_ver;
    param.capsToQuery = cap;
    let mut val = 0i32;
    let st = unsafe { get_caps(encoder, codec_guid, &mut param, &mut val) };
    if st == NV_ENC_SUCCESS { val } else { 0 }
}

fn supports_encode_guid(
    fns: &NV_ENCODE_API_FUNCTION_LIST,
    encoder: *mut c_void,
    target: NvGuid,
) -> Result<bool> {
    let get_count = fns
        .nvEncGetEncodeGUIDCount
        .context("NVENC missing nvEncGetEncodeGUIDCount.")?;
    let get_guids = fns
        .nvEncGetEncodeGUIDs
        .context("NVENC missing nvEncGetEncodeGUIDs.")?;
    let mut count = 0u32;
    check_status(
        fns,
        encoder,
        unsafe { get_count(encoder, &mut count) },
        "nvEncGetEncodeGUIDCount",
    )?;
    if count == 0 {
        return Ok(false);
    }
    let mut guids = vec![NvGuid::default(); count as usize];
    let mut out_count = 0u32;
    check_status(
        fns,
        encoder,
        unsafe { get_guids(encoder, guids.as_mut_ptr(), count, &mut out_count) },
        "nvEncGetEncodeGUIDs",
    )?;
    Ok(guids[..out_count.min(count) as usize].contains(&target))
}

fn supports_input_format(
    fns: &NV_ENCODE_API_FUNCTION_LIST,
    encoder: *mut c_void,
    codec_guid: NvGuid,
    target_fmt: u32,
) -> bool {
    let (Some(get_count), Some(get_fmts)) =
        (fns.nvEncGetInputFormatCount, fns.nvEncGetInputFormats)
    else {
        return false;
    };
    let mut count = 0u32;
    if unsafe { get_count(encoder, codec_guid, &mut count) } != NV_ENC_SUCCESS || count == 0 {
        return false;
    }
    let mut fmts = vec![0u32; count as usize];
    let mut out_count = 0u32;
    if unsafe {
        get_fmts(
            encoder,
            codec_guid,
            fmts.as_mut_ptr(),
            count,
            &mut out_count,
        )
    } != NV_ENC_SUCCESS
    {
        return false;
    }
    fmts[..out_count.min(count) as usize].contains(&target_fmt)
}

fn open_d3d11_session(lib: &NvencLibrary, device: &ID3D11Device) -> Result<*mut c_void> {
    let open_ex = lib
        .fns
        .nvEncOpenEncodeSessionEx
        .context("NVENC missing nvEncOpenEncodeSessionEx.")?;
    let mut params: NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = unsafe { std::mem::zeroed() };
    params.version = lib.versions.open_session_ex_ver;
    params.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
    params.device = device.as_raw();
    params.apiVersion = lib.versions.api_version;
    let mut encoder = std::ptr::null_mut();
    let st = unsafe { open_ex(&mut params, &mut encoder) };
    ensure!(
        st == NV_ENC_SUCCESS && !encoder.is_null(),
        "nvEncOpenEncodeSessionEx(DirectX, {}.{}) failed (status={st}).",
        lib.versions.major,
        lib.versions.minor
    );
    Ok(encoder)
}

/// Probes whether direct NVENC supports encoding the given codec on a hardware
/// D3D11 device. Fast no-op (`false`) on non-NVIDIA systems.
pub(super) fn probe_codec_support(device: &ID3D11Device, codec: Codec) -> bool {
    let Ok(lib) = NvencLibrary::load() else {
        return false;
    };
    let Ok(encoder) = open_d3d11_session(&lib, device) else {
        return false;
    };
    let supported = supports_encode_guid(&lib.fns, encoder, codec_to_guid(codec)).unwrap_or(false);
    if let Some(destroy) = lib.fns.nvEncDestroyEncoder {
        let _ = unsafe { destroy(encoder) };
    }
    supported
}

impl NvencEncoder {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        device: &ID3D11Device,
        codec: Codec,
        hdr: bool,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        log_enabled: bool,
    ) -> Result<Self> {
        let lib = NvencLibrary::load()?;
        let encoder = open_d3d11_session(&lib, device)?;

        let init_result = (|| -> Result<(
            Box<NV_ENC_CONFIG>,
            NV_ENC_INITIALIZE_PARAMS,
            *mut c_void,
            bool,
            u32,
            bool,
            bool,
        )> {
            let encode_guid = codec_to_guid(codec);
            ensure!(
                supports_encode_guid(&lib.fns, encoder, encode_guid)?,
                "NVIDIA GPU does not support hardware NVENC encoding for {codec:?}."
            );

            let caps_10bit = query_cap(
                &lib.fns,
                lib.versions,
                encoder,
                encode_guid,
                NV_ENC_CAPS_SUPPORT_10BIT_ENCODE,
            ) != 0;
            let caps_intra_refresh = query_cap(
                &lib.fns,
                lib.versions,
                encoder,
                encode_guid,
                NV_ENC_CAPS_SUPPORT_INTRA_REFRESH,
            ) != 0;
            let caps_single_slice_ir = query_cap(
                &lib.fns,
                lib.versions,
                encoder,
                encode_guid,
                NV_ENC_CAPS_SINGLE_SLICE_INTRA_REFRESH,
            ) != 0;
            let caps_custom_vbv = query_cap(
                &lib.fns,
                lib.versions,
                encoder,
                encode_guid,
                NV_ENC_CAPS_SUPPORT_CUSTOM_VBV_BUF_SIZE,
            ) != 0;
            let supports_dyn_bitrate = query_cap(
                &lib.fns,
                lib.versions,
                encoder,
                encode_guid,
                NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE,
            ) != 0;
            let supports_ref_pic_invalidation = query_cap(
                &lib.fns,
                lib.versions,
                encoder,
                encode_guid,
                NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION,
            ) != 0;

            let want_p010 = hdr && matches!(codec, Codec::Hevc | Codec::Av1);
            let use_p010 = want_p010
                && caps_10bit
                && supports_input_format(
                    &lib.fns,
                    encoder,
                    encode_guid,
                    NV_ENC_BUFFER_FORMAT_YUV420_10BIT,
                );
            if want_p010 && !use_p010 && log_enabled {
                eprintln!(
                    "[remoters:win] nvenc 10-bit P010 unavailable for {codec:?}; falling back to 8-bit NV12 SDR"
                );
            }

            let buffer_format = if use_p010 {
                NV_ENC_BUFFER_FORMAT_YUV420_10BIT
            } else {
                NV_ENC_BUFFER_FORMAT_NV12
            };

            // Seed configuration from NVIDIA's P1 + ULTRA_LOW_LATENCY preset.
            let preset_guid = NV_ENC_PRESET_P1_GUID;
            let tuning_info = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
            let mut preset_cfg: NV_ENC_PRESET_CONFIG = unsafe { std::mem::zeroed() };
            preset_cfg.version = lib.versions.preset_config_ver;
            preset_cfg.presetCfg.version = lib.versions.config_ver;
            let get_preset_ex = lib
                .fns
                .nvEncGetEncodePresetConfigEx
                .context("NVENC missing nvEncGetEncodePresetConfigEx.")?;
            check_status(
                &lib.fns,
                encoder,
                unsafe {
                    get_preset_ex(
                        encoder,
                        encode_guid,
                        preset_guid,
                        tuning_info,
                        &mut preset_cfg,
                    )
                },
                "nvEncGetEncodePresetConfigEx",
            )?;

            let mut encode_config = Box::new(preset_cfg.presetCfg);
            encode_config.version = lib.versions.config_ver;
            encode_config.gopLength = NVENC_INFINITE_GOPLENGTH;
            encode_config.frameIntervalP = 1; // 0 B-frames (IPPP)
            encode_config.frameFieldMode = NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;

            // Rate control: CBR + Single-Frame VBV (1.25x single frame)
            let vbv_bits = nvenc_single_frame_vbv_bits(bitrate, fps);
            let rc = &mut encode_config.rcParams;
            rc.version = lib.versions.rc_params_ver;
            rc.rateControlMode = NV_ENC_PARAMS_RC_CBR;
            rc.averageBitRate = bitrate;
            rc.maxBitRate = bitrate;
            if caps_custom_vbv {
                rc.vbvBufferSize = vbv_bits;
                rc.vbvInitialDelay = vbv_bits;
            }
            rc.multiPass = NV_ENC_MULTI_PASS_DISABLED;
            rc.lowDelayKeyFrameScale = 1;
            rc.flags &= !RC_FLAG_ENABLE_LOOKAHEAD;
            rc.flags |= RC_FLAG_ZERO_REORDER_DELAY;

            let (ir_period, ir_cnt) = nvenc_intra_refresh_params(fps);

            match codec {
                Codec::H264 => {
                    encode_config.profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;
                    let h264 = unsafe { &mut encode_config.encodeCodecConfig.h264Config };
                    h264.idrPeriod = NVENC_INFINITE_GOPLENGTH;
                    h264.chromaFormatIDC = 1;
                    h264.flags |= H264_FLAG_REPEAT_SPSPPS;
                    h264.flags &= !H264_FLAG_ENABLE_FILLER;
                    if caps_intra_refresh {
                        h264.flags |= H264_FLAG_ENABLE_INTRA_REFRESH;
                        if caps_single_slice_ir {
                            h264.flags |= H264_FLAG_SINGLE_SLICE_IR;
                        }
                        h264.intraRefreshPeriod = ir_period;
                        h264.intraRefreshCnt = ir_cnt;
                    }
                    h264.h264VUIParameters.videoSignalTypePresentFlag = 1;
                    h264.h264VUIParameters.videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
                    h264.h264VUIParameters.videoFullRangeFlag = 1;
                    h264.h264VUIParameters.colourDescriptionPresentFlag = 1;
                    h264.h264VUIParameters.colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
                    h264.h264VUIParameters.transferCharacteristics = NV_ENC_VUI_TRANSFER_BT709;
                    h264.h264VUIParameters.colourMatrix = NV_ENC_VUI_MATRIX_BT709;
                }
                Codec::Hevc => {
                    encode_config.profileGUID = if use_p010 {
                        NV_ENC_HEVC_PROFILE_MAIN10_GUID
                    } else {
                        NV_ENC_HEVC_PROFILE_MAIN_GUID
                    };
                    let hevc = unsafe { &mut encode_config.encodeCodecConfig.hevcConfig };
                    hevc.idrPeriod = NVENC_INFINITE_GOPLENGTH;
                    hevc.flags |= HEVC_FLAG_REPEAT_SPSPPS;
                    hevc.flags &= !(HEVC_FLAG_ENABLE_FILLER
                        | HEVC_FLAG_CHROMA_MASK
                        | HEVC_FLAG_BIT_DEPTH_MASK);
                    hevc.flags |= HEVC_FLAG_CHROMA_420;
                    if use_p010 {
                        hevc.flags |= 2 << 11; // pixelBitDepthMinus8 = 2 (10-bit)
                    }
                    if caps_intra_refresh {
                        hevc.flags |= HEVC_FLAG_ENABLE_INTRA_REFRESH;
                        if caps_single_slice_ir {
                            hevc.flags |= HEVC_FLAG_SINGLE_SLICE_IR;
                        }
                        hevc.intraRefreshPeriod = ir_period;
                        hevc.intraRefreshCnt = ir_cnt;
                    }
                    hevc.hevcVUIParameters.videoSignalTypePresentFlag = 1;
                    hevc.hevcVUIParameters.videoFormat = NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
                    hevc.hevcVUIParameters.colourDescriptionPresentFlag = 1;
                    if use_p010 {
                        hevc.hevcVUIParameters.videoFullRangeFlag = 0; // studio 16..235
                        hevc.hevcVUIParameters.colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT2020;
                        hevc.hevcVUIParameters.transferCharacteristics =
                            NV_ENC_VUI_TRANSFER_SMPTE2084;
                        hevc.hevcVUIParameters.colourMatrix = NV_ENC_VUI_MATRIX_BT2020_NCL;
                    } else {
                        hevc.hevcVUIParameters.videoFullRangeFlag = 1; // full 0..255
                        hevc.hevcVUIParameters.colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
                        hevc.hevcVUIParameters.transferCharacteristics = NV_ENC_VUI_TRANSFER_BT709;
                        hevc.hevcVUIParameters.colourMatrix = NV_ENC_VUI_MATRIX_BT709;
                    }
                }
                Codec::Av1 => {
                    encode_config.profileGUID = NV_ENC_AV1_PROFILE_MAIN_GUID;
                    let av1 = unsafe { &mut encode_config.encodeCodecConfig.av1Config };
                    av1.idrPeriod = NVENC_INFINITE_GOPLENGTH;
                    av1.flags &= !(AV1_FLAG_OUTPUT_ANNEXB
                        | AV1_FLAG_DISABLE_SEQ_HDR
                        | AV1_FLAG_ENABLE_PADDING
                        | AV1_FLAG_CHROMA_MASK
                        | AV1_FLAG_INPUT_BIT_DEPTH_MASK
                        | AV1_FLAG_OUTPUT_BIT_DEPTH_MASK);
                    av1.flags |= AV1_FLAG_REPEAT_SEQ_HDR | AV1_FLAG_CHROMA_420;
                    if use_p010 {
                        av1.flags |= (2 << 12) | (2 << 15);
                    }
                    if caps_intra_refresh {
                        av1.flags |= AV1_FLAG_ENABLE_INTRA_REFRESH;
                        av1.intraRefreshPeriod = ir_period;
                        av1.intraRefreshCnt = ir_cnt;
                    }
                    if use_p010 {
                        av1.colorPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT2020;
                        av1.transferCharacteristics = NV_ENC_VUI_TRANSFER_SMPTE2084;
                        av1.matrixCoefficients = NV_ENC_VUI_MATRIX_BT2020_NCL;
                        av1.colorRange = 0; // studio swing
                    } else {
                        av1.colorPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
                        av1.transferCharacteristics = NV_ENC_VUI_TRANSFER_BT709;
                        av1.matrixCoefficients = NV_ENC_VUI_MATRIX_BT709;
                        av1.colorRange = 1; // full swing
                    }
                    av1.chromaSamplePosition = 1;
                }
            }

            let mut init_params: NV_ENC_INITIALIZE_PARAMS = unsafe { std::mem::zeroed() };
            init_params.version = lib.versions.init_params_ver;
            init_params.encodeGUID = encode_guid;
            init_params.presetGUID = preset_guid;
            init_params.encodeWidth = width;
            init_params.encodeHeight = height;
            init_params.darWidth = width;
            init_params.darHeight = height;
            init_params.frameRateNum = fps.max(1);
            init_params.frameRateDen = 1;
            init_params.enableEncodeAsync = 0;
            init_params.enablePTD = 1;
            init_params.encodeConfig = &mut *encode_config;
            init_params.maxEncodeWidth = width;
            init_params.maxEncodeHeight = height;
            init_params.tuningInfo = tuning_info;
            init_params.bufferFormat = buffer_format;

            let init_fn = lib
                .fns
                .nvEncInitializeEncoder
                .context("NVENC missing nvEncInitializeEncoder.")?;
            check_status(
                &lib.fns,
                encoder,
                unsafe { init_fn(encoder, &mut init_params) },
                "nvEncInitializeEncoder",
            )?;

            let create_bs = lib
                .fns
                .nvEncCreateBitstreamBuffer
                .context("NVENC missing nvEncCreateBitstreamBuffer.")?;
            let mut bs_params: NV_ENC_CREATE_BITSTREAM_BUFFER = unsafe { std::mem::zeroed() };
            bs_params.version = lib.versions.create_bitstream_ver;
            check_status(
                &lib.fns,
                encoder,
                unsafe { create_bs(encoder, &mut bs_params) },
                "nvEncCreateBitstreamBuffer",
            )?;
            ensure!(
                !bs_params.bitstreamBuffer.is_null(),
                "NVENC returned a null bitstream buffer."
            );

            if log_enabled {
                eprintln!(
                    "[remoters:win] selected_encoder=nvenc api={}.{} codec={codec:?} {}x{}@{}fps p010={} vbv_kb={} ir={}({}/{}) rpi={} dyn_br={}",
                    lib.versions.major,
                    lib.versions.minor,
                    width,
                    height,
                    fps,
                    use_p010,
                    vbv_bits / (8 * 1024),
                    caps_intra_refresh,
                    ir_cnt,
                    ir_period,
                    supports_ref_pic_invalidation,
                    supports_dyn_bitrate,
                );
            }

            Ok((
                encode_config,
                init_params,
                bs_params.bitstreamBuffer,
                use_p010,
                buffer_format,
                supports_dyn_bitrate,
                supports_ref_pic_invalidation,
            ))
        })();

        let (
            encode_config,
            init_params,
            bitstream,
            use_p010,
            buffer_format,
            supports_dyn_bitrate,
            supports_ref_pic_invalidation,
        ) = match init_result {
            Ok(parts) => parts,
            Err(err) => {
                if let Some(destroy) = lib.fns.nvEncDestroyEncoder {
                    let _ = unsafe { destroy(encoder) };
                }
                return Err(err);
            }
        };

        Ok(Self {
            registered: Vec::with_capacity(4),
            bitstream,
            encoder,
            encode_config,
            init_params,
            _device: device.clone(),
            lib,
            width,
            height,
            fps: fps.max(1),
            use_p010,
            buffer_format,
            supports_dyn_bitrate,
            supports_ref_pic_invalidation,
            frame_idx: 0,
        })
    }

    pub(super) fn uses_p010(&self) -> bool {
        self.use_p010
    }

    pub(super) fn clear_registered_surfaces(&mut self) {
        if let Some(unregister) = self.lib.fns.nvEncUnregisterResource {
            for entry in self.registered.drain(..) {
                if !entry.registered.is_null() {
                    let _ = unsafe { unregister(self.encoder, entry.registered) };
                }
            }
        } else {
            self.registered.clear();
        }
    }

    pub(super) fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        if !self.supports_dyn_bitrate {
            return Ok(());
        }
        let Some(reconf_fn) = self.lib.fns.nvEncReconfigureEncoder else {
            return Ok(());
        };
        let vbv_bits = nvenc_single_frame_vbv_bits(bitrate, self.fps);
        self.encode_config.rcParams.averageBitRate = bitrate;
        self.encode_config.rcParams.maxBitRate = bitrate;
        if self.encode_config.rcParams.vbvBufferSize != 0 {
            self.encode_config.rcParams.vbvBufferSize = vbv_bits;
            self.encode_config.rcParams.vbvInitialDelay = vbv_bits;
        }
        self.init_params.encodeConfig = &mut *self.encode_config;
        let mut reconf: NV_ENC_RECONFIGURE_PARAMS = unsafe { std::mem::zeroed() };
        reconf.version = self.lib.versions.reconfigure_params_ver;
        reconf.reInitEncodeParams = self.init_params;
        reconf.flags = 0; // smooth live bitrate update without encoder reset
        check_status(
            &self.lib.fns,
            self.encoder,
            unsafe { reconf_fn(self.encoder, &mut reconf) },
            "nvEncReconfigureEncoder",
        )
    }

    #[allow(dead_code)]
    pub(super) fn invalidate_ref_frames(&mut self, timestamp_100ns: u64) {
        if self.supports_ref_pic_invalidation
            && let Some(invalidate) = self.lib.fns.nvEncInvalidateRefFrames
        {
            let _ = unsafe { invalidate(self.encoder, timestamp_100ns) };
        }
    }

    fn ensure_registered(
        &mut self,
        texture: &ID3D11Texture2D,
        buffer_format: u32,
    ) -> Result<*mut c_void> {
        let raw_ptr = texture.as_raw() as usize;
        if let Some(existing) = self
            .registered
            .iter()
            .find(|entry| entry.raw_ptr == raw_ptr && entry.buffer_format == buffer_format)
        {
            return Ok(existing.registered);
        }
        // Cap cache to avoid leaking entries if the caller rotates dynamic textures.
        if self.registered.len() >= 16 {
            self.clear_registered_surfaces();
        }
        let register_fn = self
            .lib
            .fns
            .nvEncRegisterResource
            .context("NVENC missing nvEncRegisterResource.")?;
        let mut reg: NV_ENC_REGISTER_RESOURCE = unsafe { std::mem::zeroed() };
        reg.version = self.lib.versions.register_resource_ver;
        reg.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
        reg.width = self.width;
        reg.height = self.height;
        reg.pitch = 0;
        reg.subResourceIndex = 0;
        reg.resourceToRegister = texture.as_raw();
        reg.bufferFormat = buffer_format;
        reg.bufferUsage = NV_ENC_INPUT_IMAGE;
        check_status(
            &self.lib.fns,
            self.encoder,
            unsafe { register_fn(self.encoder, &mut reg) },
            "nvEncRegisterResource",
        )?;
        ensure!(
            !reg.registeredResource.is_null(),
            "NVENC returned a null registered resource handle."
        );
        self.registered.push(RegisteredSurface {
            raw_ptr,
            _texture: texture.clone(),
            registered: reg.registeredResource,
            buffer_format,
        });
        Ok(reg.registeredResource)
    }

    pub(super) fn encode_texture<F>(
        &mut self,
        texture: &ID3D11Texture2D,
        dxgi_format: i32,
        timestamp_100ns: i64,
        duration_100ns: i64,
        force_keyframe: bool,
        on_packet: F,
    ) -> Result<()>
    where
        F: FnOnce(&[u8], i64),
    {
        let buffer_format =
            dxgi_format_to_nvenc_buffer_format(dxgi_format).unwrap_or(self.buffer_format);
        let registered = self.ensure_registered(texture, buffer_format)?;

        let map_fn = self
            .lib
            .fns
            .nvEncMapInputResource
            .context("NVENC missing nvEncMapInputResource.")?;
        let mut map: NV_ENC_MAP_INPUT_RESOURCE = unsafe { std::mem::zeroed() };
        map.version = self.lib.versions.map_input_resource_ver;
        map.registeredResource = registered;
        check_status(
            &self.lib.fns,
            self.encoder,
            unsafe { map_fn(self.encoder, &mut map) },
            "nvEncMapInputResource",
        )?;
        let _unmap_guard = UnmapGuard {
            encoder: self.encoder,
            unmap_fn: self.lib.fns.nvEncUnmapInputResource,
            mapped: map.mappedResource,
            _marker: std::marker::PhantomData,
        };

        let encode_fn = self
            .lib
            .fns
            .nvEncEncodePicture
            .context("NVENC missing nvEncEncodePicture.")?;
        let mut pic: NV_ENC_PIC_PARAMS = unsafe { std::mem::zeroed() };
        pic.version = self.lib.versions.pic_params_ver;
        pic.inputWidth = self.width;
        pic.inputHeight = self.height;
        pic.inputPitch = 0;
        pic.encodePicFlags = if force_keyframe {
            NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
        } else {
            0
        };
        pic.frameIdx = self.frame_idx;
        pic.inputTimeStamp = timestamp_100ns.max(0) as u64;
        pic.inputDuration = duration_100ns.max(1) as u64;
        pic.inputBuffer = map.mappedResource;
        pic.outputBitstream = self.bitstream;
        pic.bufferFmt = if map.mappedBufferFmt != NV_ENC_BUFFER_FORMAT_UNDEFINED {
            map.mappedBufferFmt
        } else {
            buffer_format
        };
        pic.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
        pic.pictureType = if force_keyframe {
            NV_ENC_PIC_TYPE_IDR
        } else {
            NV_ENC_PIC_TYPE_UNKNOWN
        };

        let status = unsafe { encode_fn(self.encoder, &mut pic) };
        self.frame_idx = self.frame_idx.wrapping_add(1);
        if status == NV_ENC_ERR_NEED_MORE_INPUT {
            return Ok(());
        }
        check_status(&self.lib.fns, self.encoder, status, "nvEncEncodePicture")?;

        let lock_fn = self
            .lib
            .fns
            .nvEncLockBitstream
            .context("NVENC missing nvEncLockBitstream.")?;
        let mut lock: NV_ENC_LOCK_BITSTREAM = unsafe { std::mem::zeroed() };
        lock.version = self.lib.versions.lock_bitstream_ver;
        lock.outputBitstream = self.bitstream;
        check_status(
            &self.lib.fns,
            self.encoder,
            unsafe { lock_fn(self.encoder, &mut lock) },
            "nvEncLockBitstream",
        )?;
        let _unlock_guard = UnlockGuard {
            encoder: self.encoder,
            unlock_fn: self.lib.fns.nvEncUnlockBitstream,
            bitstream: self.bitstream,
            _marker: std::marker::PhantomData,
        };

        let slice = if !lock.bitstreamBufferPtr.is_null() && lock.bitstreamSizeInBytes > 0 {
            unsafe {
                std::slice::from_raw_parts(
                    lock.bitstreamBufferPtr.cast::<u8>(),
                    lock.bitstreamSizeInBytes as usize,
                )
            }
        } else {
            &[]
        };
        let out_time = i64::try_from(lock.outputTimeStamp).unwrap_or(timestamp_100ns);
        on_packet(slice, out_time);
        Ok(())
    }
}

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        self.clear_registered_surfaces();
        if !self.bitstream.is_null()
            && let Some(destroy_bs) = self.lib.fns.nvEncDestroyBitstreamBuffer
        {
            let _ = unsafe { destroy_bs(self.encoder, self.bitstream) };
            self.bitstream = std::ptr::null_mut();
        }
        if !self.encoder.is_null()
            && let Some(destroy) = self.lib.fns.nvEncDestroyEncoder
        {
            let _ = unsafe { destroy(self.encoder) };
            self.encoder = std::ptr::null_mut();
        }
    }
}
