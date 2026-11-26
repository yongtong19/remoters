//! Direct NVIDIA NVENC + CUDA Driver API hardware video encoder for Linux.
//!
//! Dynamically loads `libcuda.so.1` and `libnvidia-encode.so.1` at runtime via
//! `dlopen`/`dlsym`, requiring zero build-time CUDA/NVENC SDK headers or extra
//! crate dependencies.

#[cfg(target_os = "linux")]
use core::ffi::{CStr, c_char, c_int, c_void};
#[cfg(target_os = "linux")]
use std::{
    ptr,
    sync::{Mutex, OnceLock},
};

#[cfg(target_os = "linux")]
use anyhow::{Context, Result, anyhow, bail, ensure};
#[cfg(target_os = "linux")]
use remoters_core::video::Codec;

#[cfg(target_os = "linux")]
use super::nvenc_ffi::*;

#[cfg(target_os = "linux")]
const CU_SUCCESS: i32 = 0;
#[cfg(target_os = "linux")]
const CU_MEMORYTYPE_HOST: u32 = 1;
#[cfg(target_os = "linux")]
const CU_MEMORYTYPE_DEVICE: u32 = 2;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

#[cfg(target_os = "linux")]
const RTLD_NOW: c_int = 2;

#[cfg(target_os = "linux")]
unsafe fn open_shared_library(names: &[&CStr]) -> Result<*mut c_void> {
    let mut last_err = String::new();
    for name in names {
        let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
        if !handle.is_null() {
            return Ok(handle);
        }
        let err_ptr = unsafe { dlerror() };
        if !err_ptr.is_null() {
            last_err = unsafe { CStr::from_ptr(err_ptr) }
                .to_string_lossy()
                .into_owned();
        }
    }
    bail!("Failed to dlopen library ({last_err})")
}

#[cfg(target_os = "linux")]
unsafe fn load_symbol<T: Copy>(handle: *mut c_void, name: &CStr) -> Result<T> {
    let sym = unsafe { dlsym(handle, name.as_ptr()) };
    ensure!(
        !sym.is_null(),
        "Missing symbol {:?} in dynamic library.",
        name
    );
    Ok(unsafe { std::mem::transmute_copy(&sym) })
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct CudaApi {
    cu_init: unsafe extern "C" fn(u32) -> i32,
    cu_device_get_count: unsafe extern "C" fn(*mut i32) -> i32,
    cu_device_get: unsafe extern "C" fn(*mut i32, i32) -> i32,
    cu_ctx_create_v2: unsafe extern "C" fn(*mut *mut c_void, u32, i32) -> i32,
    cu_ctx_destroy_v2: unsafe extern "C" fn(*mut c_void) -> i32,
    cu_ctx_push_current_v2: unsafe extern "C" fn(*mut c_void) -> i32,
    cu_ctx_pop_current_v2: unsafe extern "C" fn(*mut *mut c_void) -> i32,
    cu_mem_alloc_pitch_v2: unsafe extern "C" fn(*mut u64, *mut usize, usize, usize, u32) -> i32,
    cu_mem_free_v2: unsafe extern "C" fn(u64) -> i32,
    cu_memcpy_2d_v2: unsafe extern "C" fn(*const CudaMemcpy2d) -> i32,
}

#[cfg(target_os = "linux")]
struct NvencRuntime {
    cuda: CudaApi,
    nvenc: NvEncodeApiFunctionList,
    driver_version: (u32, u32),
}

// Function pointers loaded from process-global shared libraries are thread-safe to share.
#[cfg(target_os = "linux")]
unsafe impl Send for NvencRuntime {}
#[cfg(target_os = "linux")]
unsafe impl Sync for NvencRuntime {}

#[cfg(target_os = "linux")]
fn load_runtime() -> Result<&'static NvencRuntime> {
    static RUNTIME: OnceLock<std::result::Result<NvencRuntime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| init_runtime().map_err(|err| format!("{err:#}"))) {
        Ok(rt) => Ok(rt),
        Err(msg) => Err(anyhow!("{msg}")),
    }
}

#[cfg(target_os = "linux")]
fn init_runtime() -> Result<NvencRuntime> {
    let cuda_handle = unsafe {
        open_shared_library(&[c"libcuda.so.1", c"libcuda.so"])
            .context("libcuda.so.1 is unavailable")?
    };
    let cuda = unsafe {
        CudaApi {
            cu_init: load_symbol(cuda_handle, c"cuInit")?,
            cu_device_get_count: load_symbol(cuda_handle, c"cuDeviceGetCount")?,
            cu_device_get: load_symbol(cuda_handle, c"cuDeviceGet")?,
            cu_ctx_create_v2: load_symbol(cuda_handle, c"cuCtxCreate_v2")?,
            cu_ctx_destroy_v2: load_symbol(cuda_handle, c"cuCtxDestroy_v2")?,
            cu_ctx_push_current_v2: load_symbol(cuda_handle, c"cuCtxPushCurrent_v2")?,
            cu_ctx_pop_current_v2: load_symbol(cuda_handle, c"cuCtxPopCurrent_v2")?,
            cu_mem_alloc_pitch_v2: load_symbol(cuda_handle, c"cuMemAllocPitch_v2")?,
            cu_mem_free_v2: load_symbol(cuda_handle, c"cuMemFree_v2")?,
            cu_memcpy_2d_v2: load_symbol(cuda_handle, c"cuMemcpy2D_v2")?,
        }
    };
    let rc = unsafe { (cuda.cu_init)(0) };
    ensure!(rc == CU_SUCCESS, "cuInit failed with CUDA error {rc}.");
    let mut count = 0i32;
    let rc = unsafe { (cuda.cu_device_get_count)(&mut count) };
    ensure!(
        rc == CU_SUCCESS && count > 0,
        "No CUDA-capable NVIDIA GPU detected (rc={rc}, count={count})."
    );

    let nvenc_handle = unsafe {
        open_shared_library(&[c"libnvidia-encode.so.1", c"libnvidia-encode.so"])
            .context("libnvidia-encode.so.1 is unavailable")?
    };
    let get_max_ver: unsafe extern "C" fn(*mut u32) -> u32 =
        unsafe { load_symbol(nvenc_handle, c"NvEncodeAPIGetMaxSupportedVersion")? };
    let create_instance: unsafe extern "C" fn(*mut NvEncodeApiFunctionList) -> u32 =
        unsafe { load_symbol(nvenc_handle, c"NvEncodeAPICreateInstance")? };

    let mut max_raw = 0u32;
    let st = unsafe { get_max_ver(&mut max_raw) };
    ensure!(
        st == NV_ENC_SUCCESS,
        "NvEncodeAPIGetMaxSupportedVersion failed (status={st})."
    );
    let driver_version = parse_driver_nvenc_version(max_raw);
    ensure!(
        driver_supports_nvenc_12(max_raw),
        "NVIDIA driver NVENC API {}.{} is older than required {}.{}.",
        driver_version.0,
        driver_version.1,
        NVENC_TARGET_MAJOR,
        NVENC_TARGET_MINOR
    );

    let mut fn_list: NvEncodeApiFunctionList = unsafe { std::mem::zeroed() };
    fn_list.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    let st = unsafe { create_instance(&mut fn_list) };
    ensure!(
        st == NV_ENC_SUCCESS,
        "NvEncodeAPICreateInstance failed (status={st})."
    );
    ensure!(
        fn_list.nv_enc_open_encode_session_ex.is_some()
            && fn_list.nv_enc_initialize_encoder.is_some()
            && fn_list.nv_enc_get_encode_preset_config_ex.is_some()
            && fn_list.nv_enc_register_resource.is_some()
            && fn_list.nv_enc_map_input_resource.is_some()
            && fn_list.nv_enc_encode_picture.is_some()
            && fn_list.nv_enc_lock_bitstream.is_some()
            && fn_list.nv_enc_unlock_bitstream.is_some()
            && fn_list.nv_enc_destroy_encoder.is_some(),
        "Incomplete NV_ENCODE_API_FUNCTION_LIST returned by libnvidia-encode.so.1."
    );

    Ok(NvencRuntime {
        cuda,
        nvenc: fn_list,
        driver_version,
    })
}

#[cfg(target_os = "linux")]
struct CudaCtxGuard {
    cuda: CudaApi,
}

#[cfg(target_os = "linux")]
impl CudaCtxGuard {
    fn push(cuda: CudaApi, ctx: *mut c_void) -> Result<Self> {
        let rc = unsafe { (cuda.cu_ctx_push_current_v2)(ctx) };
        ensure!(rc == CU_SUCCESS, "cuCtxPushCurrent_v2 failed (rc={rc}).");
        Ok(Self { cuda })
    }
}

#[cfg(target_os = "linux")]
impl Drop for CudaCtxGuard {
    fn drop(&mut self) {
        let mut popped: *mut c_void = ptr::null_mut();
        let _ = unsafe { (self.cuda.cu_ctx_pop_current_v2)(&mut popped) };
    }
}

#[cfg(target_os = "linux")]
fn nvenc_err(rt: &NvencRuntime, encoder: *mut c_void, op: &str, status: u32) -> anyhow::Error {
    let detail = if !encoder.is_null()
        && let Some(get_err) = rt.nvenc.nv_enc_get_last_error_string
    {
        let ptr = unsafe { get_err(encoder) };
        if !ptr.is_null() {
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .trim()
                .to_owned()
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    if detail.is_empty() {
        anyhow!("NVENC {op} failed (status={status})")
    } else {
        anyhow!("NVENC {op} failed (status={status}: {detail})")
    }
}

/// Probes whether direct NVENC hardware encoding is available for `codec` (8-bit SDR).
#[cfg(target_os = "linux")]
pub fn probe_direct_nvenc(codec: Codec) -> bool {
    static CACHE: OnceLock<Mutex<[Option<bool>; 3]>> = OnceLock::new();
    let idx = match codec {
        Codec::H264 => 0,
        Codec::Hevc => 1,
        Codec::Av1 => 2,
    };
    let cache = CACHE.get_or_init(|| Mutex::new([None; 3]));
    if let Some(cached) = cache.lock().unwrap()[idx] {
        return cached;
    }
    let supported = probe_direct_nvenc_uncached(codec);
    cache.lock().unwrap()[idx] = Some(supported);
    supported
}

#[cfg(target_os = "linux")]
fn probe_direct_nvenc_uncached(codec: Codec) -> bool {
    let Ok(rt) = load_runtime() else {
        return false;
    };
    let mut device = 0i32;
    if unsafe { (rt.cuda.cu_device_get)(&mut device, 0) } != CU_SUCCESS {
        return false;
    }
    let mut cu_ctx: *mut c_void = ptr::null_mut();
    if unsafe { (rt.cuda.cu_ctx_create_v2)(&mut cu_ctx, 0, device) } != CU_SUCCESS {
        return false;
    }
    let mut popped: *mut c_void = ptr::null_mut();
    let _ = unsafe { (rt.cuda.cu_ctx_pop_current_v2)(&mut popped) };

    let result = (|| -> Result<bool> {
        let _guard = CudaCtxGuard::push(rt.cuda, cu_ctx)?;
        let mut open_params: NvEncOpenEncodeSessionExParams = unsafe { std::mem::zeroed() };
        open_params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        open_params.device_type = NV_ENC_DEVICE_TYPE_CUDA;
        open_params.device = cu_ctx;
        open_params.api_version = NVENCAPI_VERSION;

        let mut encoder: *mut c_void = ptr::null_mut();
        let st = unsafe {
            (rt.nvenc.nv_enc_open_encode_session_ex.unwrap())(&mut open_params, &mut encoder)
        };
        if st != NV_ENC_SUCCESS || encoder.is_null() {
            return Ok(false);
        }
        let supported = codec_and_rgb_supported(rt, encoder, codec).unwrap_or(false);
        if let Some(destroy) = rt.nvenc.nv_enc_destroy_encoder {
            let _ = unsafe { destroy(encoder) };
        }
        Ok(supported)
    })()
    .unwrap_or(false);

    let _ = unsafe { (rt.cuda.cu_ctx_destroy_v2)(cu_ctx) };
    result
}

#[cfg(target_os = "linux")]
fn codec_and_rgb_supported(rt: &NvencRuntime, encoder: *mut c_void, codec: Codec) -> Result<bool> {
    let target = codec_guid(codec);
    let get_count = rt
        .nvenc
        .nv_enc_get_encode_guid_count
        .context("Missing nvEncGetEncodeGUIDCount")?;
    let get_guids = rt
        .nvenc
        .nv_enc_get_encode_guids
        .context("Missing nvEncGetEncodeGUIDs")?;

    let mut count = 0u32;
    let st = unsafe { get_count(encoder, &mut count) };
    if st != NV_ENC_SUCCESS || count == 0 {
        return Ok(false);
    }
    let mut guids = vec![NvEncGuid::default(); count as usize];
    let mut actual = 0u32;
    let st = unsafe { get_guids(encoder, guids.as_mut_ptr(), count, &mut actual) };
    if st != NV_ENC_SUCCESS {
        return Ok(false);
    }
    if !guids[..actual as usize].contains(&target) {
        return Ok(false);
    }

    let formats = query_input_formats(rt, encoder, target)?;
    Ok(
        formats.contains(&NV_ENC_BUFFER_FORMAT_ARGB)
            || formats.contains(&NV_ENC_BUFFER_FORMAT_ABGR),
    )
}

#[cfg(target_os = "linux")]
fn query_input_formats(
    rt: &NvencRuntime,
    encoder: *mut c_void,
    encode_guid: NvEncGuid,
) -> Result<Vec<u32>> {
    let Some(get_cnt) = rt.nvenc.nv_enc_get_input_format_count else {
        return Ok(vec![NV_ENC_BUFFER_FORMAT_ARGB, NV_ENC_BUFFER_FORMAT_ABGR]);
    };
    let Some(get_fmts) = rt.nvenc.nv_enc_get_input_formats else {
        return Ok(vec![NV_ENC_BUFFER_FORMAT_ARGB, NV_ENC_BUFFER_FORMAT_ABGR]);
    };
    let mut count = 0u32;
    let st = unsafe { get_cnt(encoder, encode_guid, &mut count) };
    if st != NV_ENC_SUCCESS || count == 0 {
        return Ok(Vec::new());
    }
    let mut formats = vec![0u32; count as usize];
    let mut actual = 0u32;
    let st = unsafe {
        get_fmts(
            encoder,
            encode_guid,
            formats.as_mut_ptr(),
            count,
            &mut actual,
        )
    };
    if st != NV_ENC_SUCCESS {
        return Ok(Vec::new());
    }
    formats.truncate(actual as usize);
    Ok(formats)
}

#[cfg(target_os = "linux")]
fn query_encode_cap(
    rt: &NvencRuntime,
    encoder: *mut c_void,
    encode_guid: NvEncGuid,
    cap: u32,
) -> i32 {
    let Some(get_caps) = rt.nvenc.nv_enc_get_encode_caps else {
        return 0;
    };
    let mut param: NvEncCapsParam = unsafe { std::mem::zeroed() };
    param.version = NV_ENC_CAPS_PARAM_VER;
    param.caps_to_query = cap;
    let mut val = 0i32;
    let st = unsafe { get_caps(encoder, encode_guid, &mut param, &mut val) };
    if st == NV_ENC_SUCCESS { val } else { 0 }
}

/// Direct NVIDIA NVENC hardware encoder operating on a CUDA context with
/// pitched GPU VRAM upload (`NV_ENC_BUFFER_FORMAT_ARGB` / `ABGR`).
#[cfg(target_os = "linux")]
pub struct DirectNvencEncoder {
    rt: &'static NvencRuntime,
    cu_ctx: *mut c_void,
    encoder: *mut c_void,
    dev_ptr: u64,
    dev_pitch: usize,
    registered_resource: *mut c_void,
    bitstream_buffer: *mut c_void,
    encode_config: Box<NvEncConfig>,
    init_params: NvEncInitializeParams,
    codec: Codec,
    width: u32,
    height: u32,
    fps: u16,
    bitrate_bps: u32,
    input_pix_fmt: &'static str,
    swap_rb_on_upload: bool,
    scratch_rb: Vec<u8>,
    frame_idx: u32,
}

#[cfg(target_os = "linux")]
impl DirectNvencEncoder {
    pub fn new(
        codec: Codec,
        width: u32,
        height: u32,
        fps: u16,
        bitrate_bps: u32,
        input_pix_fmt: &'static str,
    ) -> Result<Self> {
        let rt = load_runtime()?;
        let desired_fmt = pixel_format_to_nvenc(input_pix_fmt)
            .ok_or_else(|| anyhow!("Unsupported pixel format for direct NVENC: {input_pix_fmt}"))?;

        let mut device = 0i32;
        let rc = unsafe { (rt.cuda.cu_device_get)(&mut device, 0) };
        ensure!(rc == CU_SUCCESS, "cuDeviceGet(0) failed (rc={rc}).");

        let mut cu_ctx: *mut c_void = ptr::null_mut();
        let rc = unsafe { (rt.cuda.cu_ctx_create_v2)(&mut cu_ctx, 0, device) };
        ensure!(rc == CU_SUCCESS, "cuCtxCreate_v2 failed (rc={rc}).");
        // Pop immediately; every method pushes/pops via CudaCtxGuard for thread safety.
        let mut popped: *mut c_void = ptr::null_mut();
        let _ = unsafe { (rt.cuda.cu_ctx_pop_current_v2)(&mut popped) };

        let setup = (|| -> Result<Self> {
            let _guard = CudaCtxGuard::push(rt.cuda, cu_ctx)?;

            let mut open_params: NvEncOpenEncodeSessionExParams = unsafe { std::mem::zeroed() };
            open_params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
            open_params.device_type = NV_ENC_DEVICE_TYPE_CUDA;
            open_params.device = cu_ctx;
            open_params.api_version = NVENCAPI_VERSION;

            let mut encoder: *mut c_void = ptr::null_mut();
            let st = unsafe {
                (rt.nvenc.nv_enc_open_encode_session_ex.unwrap())(&mut open_params, &mut encoder)
            };
            if st != NV_ENC_SUCCESS || encoder.is_null() {
                return Err(nvenc_err(rt, ptr::null_mut(), "OpenEncodeSessionEx", st));
            }

            let res = Self::init_session(
                rt,
                cu_ctx,
                encoder,
                codec,
                width,
                height,
                fps,
                bitrate_bps,
                input_pix_fmt,
                desired_fmt,
            );
            if res.is_err()
                && let Some(destroy) = rt.nvenc.nv_enc_destroy_encoder
            {
                let _ = unsafe { destroy(encoder) };
            }
            res
        })();

        if setup.is_err() {
            let _ = unsafe { (rt.cuda.cu_ctx_destroy_v2)(cu_ctx) };
        }
        setup
    }

    #[allow(clippy::too_many_arguments)]
    fn init_session(
        rt: &'static NvencRuntime,
        cu_ctx: *mut c_void,
        encoder: *mut c_void,
        codec: Codec,
        width: u32,
        height: u32,
        fps: u16,
        bitrate_bps: u32,
        input_pix_fmt: &'static str,
        desired_fmt: u32,
    ) -> Result<Self> {
        let encode_guid = codec_guid(codec);
        let supported_formats = query_input_formats(rt, encoder, encode_guid)?;
        let (nv_buffer_format, swap_rb_on_upload) = if supported_formats.contains(&desired_fmt) {
            (desired_fmt, false)
        } else if desired_fmt == NV_ENC_BUFFER_FORMAT_ABGR
            && supported_formats.contains(&NV_ENC_BUFFER_FORMAT_ARGB)
        {
            (NV_ENC_BUFFER_FORMAT_ARGB, true)
        } else if desired_fmt == NV_ENC_BUFFER_FORMAT_ARGB
            && supported_formats.contains(&NV_ENC_BUFFER_FORMAT_ABGR)
        {
            (NV_ENC_BUFFER_FORMAT_ABGR, true)
        } else {
            bail!(
                "Direct NVENC codec {codec:?} does not support 32-bit RGB input on this GPU (driver {}.{}).",
                rt.driver_version.0,
                rt.driver_version.1
            );
        };

        let mut preset_cfg: NvEncPresetConfig = unsafe { std::mem::zeroed() };
        preset_cfg.version = NV_ENC_PRESET_CONFIG_VER;
        preset_cfg.preset_cfg.version = NV_ENC_CONFIG_VER;
        let st = unsafe {
            (rt.nvenc.nv_enc_get_encode_preset_config_ex.unwrap())(
                encoder,
                encode_guid,
                NV_ENC_PRESET_P1_GUID,
                NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                &mut preset_cfg,
            )
        };
        if st != NV_ENC_SUCCESS {
            return Err(nvenc_err(rt, encoder, "GetEncodePresetConfigEx", st));
        }

        let supports_ir =
            query_encode_cap(rt, encoder, encode_guid, NV_ENC_CAPS_SUPPORT_INTRA_REFRESH) > 0;

        let mut encode_config = Box::new(preset_cfg.preset_cfg);
        configure_ultra_low_latency(&mut encode_config, codec, fps, bitrate_bps, supports_ir);

        let mut init_params: NvEncInitializeParams = unsafe { std::mem::zeroed() };
        init_params.version = NV_ENC_INITIALIZE_PARAMS_VER;
        init_params.encode_guid = encode_guid;
        init_params.preset_guid = NV_ENC_PRESET_P1_GUID;
        init_params.encode_width = width;
        init_params.encode_height = height;
        init_params.dar_width = width;
        init_params.dar_height = height;
        init_params.frame_rate_num = u32::from(fps.max(1));
        init_params.frame_rate_den = 1;
        init_params.enable_encode_async = 0;
        init_params.enable_ptd = 1;
        init_params.encode_config = encode_config.as_mut();
        init_params.max_encode_width = width;
        init_params.max_encode_height = height;
        init_params.tuning_info = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;

        let st =
            unsafe { (rt.nvenc.nv_enc_initialize_encoder.unwrap())(encoder, &mut init_params) };
        if st != NV_ENC_SUCCESS {
            return Err(nvenc_err(rt, encoder, "InitializeEncoder", st));
        }

        let width_bytes = usize::try_from(width)? * 4;
        let height_usize = usize::try_from(height)?;
        let mut dev_ptr = 0u64;
        let mut dev_pitch = 0usize;
        let rc = unsafe {
            (rt.cuda.cu_mem_alloc_pitch_v2)(
                &mut dev_ptr,
                &mut dev_pitch,
                width_bytes,
                height_usize,
                4,
            )
        };
        ensure!(
            rc == CU_SUCCESS && dev_ptr != 0,
            "cuMemAllocPitch_v2 failed (rc={rc})."
        );

        let mut reg: NvEncRegisterResource = unsafe { std::mem::zeroed() };
        reg.version = NV_ENC_REGISTER_RESOURCE_VER;
        reg.resource_type = NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR;
        reg.width = width;
        reg.height = height;
        reg.pitch = u32::try_from(dev_pitch).context("CUDA pitch overflowed u32.")?;
        reg.resource_to_register = dev_ptr as *mut c_void;
        reg.buffer_format = nv_buffer_format;
        reg.buffer_usage = NV_ENC_INPUT_IMAGE;

        let st = unsafe { (rt.nvenc.nv_enc_register_resource.unwrap())(encoder, &mut reg) };
        if st != NV_ENC_SUCCESS {
            let _ = unsafe { (rt.cuda.cu_mem_free_v2)(dev_ptr) };
            return Err(nvenc_err(rt, encoder, "RegisterResource", st));
        }

        let mut bs: NvEncCreateBitstreamBuffer = unsafe { std::mem::zeroed() };
        bs.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
        let st = unsafe { (rt.nvenc.nv_enc_create_bitstream_buffer.unwrap())(encoder, &mut bs) };
        if st != NV_ENC_SUCCESS {
            if let Some(unreg) = rt.nvenc.nv_enc_unregister_resource {
                let _ = unsafe { unreg(encoder, reg.registered_resource) };
            }
            let _ = unsafe { (rt.cuda.cu_mem_free_v2)(dev_ptr) };
            return Err(nvenc_err(rt, encoder, "CreateBitstreamBuffer", st));
        }

        eprintln!(
            "Linux direct NVENC active: codec={codec:?} {width}x{height}@{fps} \
             bitrate={}Mbps vbv={}KB fmt={input_pix_fmt} intra_refresh={supports_ir} \
             driver_nvenc={}.{}",
            bitrate_bps / 1_000_000,
            encode_config.rc_params.vbv_buffer_size / 8_192,
            rt.driver_version.0,
            rt.driver_version.1,
        );

        Ok(Self {
            rt,
            cu_ctx,
            encoder,
            dev_ptr,
            dev_pitch,
            registered_resource: reg.registered_resource,
            bitstream_buffer: bs.bitstream_buffer,
            encode_config,
            init_params,
            codec,
            width,
            height,
            fps: fps.max(1),
            bitrate_bps,
            input_pix_fmt,
            swap_rb_on_upload,
            scratch_rb: Vec::new(),
            frame_idx: 0,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn input_pix_fmt(&self) -> &'static str {
        self.input_pix_fmt
    }

    pub fn bitrate_bps(&self) -> u32 {
        self.bitrate_bps
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Reconfigures target bitrate and single-frame VBV live without restarting NVENC.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        if bitrate_bps == self.bitrate_bps {
            return Ok(());
        }
        let Some(reconfigure) = self.rt.nvenc.nv_enc_reconfigure_encoder else {
            bail!("NVENC driver does not expose nvEncReconfigureEncoder.");
        };
        let _guard = CudaCtxGuard::push(self.rt.cuda, self.cu_ctx)?;

        apply_live_bitrate(&mut self.encode_config, self.fps, bitrate_bps);
        self.init_params.encode_config = self.encode_config.as_mut();

        let mut params: NvEncReconfigureParams = unsafe { std::mem::zeroed() };
        params.version = NV_ENC_RECONFIGURE_PARAMS_VER;
        params.re_init_encode_params = self.init_params;
        params.flags = 0;

        let mut st = unsafe { reconfigure(self.encoder, &mut params) };
        if st != NV_ENC_SUCCESS {
            params.flags = RECONFIG_FLAG_RESET_ENCODER | RECONFIG_FLAG_FORCE_IDR;
            st = unsafe { reconfigure(self.encoder, &mut params) };
        }
        if st != NV_ENC_SUCCESS {
            return Err(nvenc_err(self.rt, self.encoder, "ReconfigureEncoder", st));
        }
        self.bitrate_bps = bitrate_bps;
        Ok(())
    }

    /// Uploads a raw 32-bit RGBA/BGRA frame directly to CUDA VRAM and encodes one access unit.
    pub fn encode_frame(&mut self, pixels: &[u8], force_keyframe: bool) -> Result<Vec<u8>> {
        let row_bytes = usize::try_from(self.width)? * 4;
        let expected_len = row_bytes * usize::try_from(self.height)?;
        ensure!(
            pixels.len() >= expected_len,
            "Frame buffer too small for NVENC upload ({} < {expected_len}).",
            pixels.len()
        );

        let src_ptr = if self.swap_rb_on_upload {
            self.scratch_rb.resize(expected_len, 0);
            for (dst_px, src_px) in self
                .scratch_rb
                .chunks_exact_mut(4)
                .zip(pixels[..expected_len].chunks_exact(4))
            {
                dst_px[0] = src_px[2];
                dst_px[1] = src_px[1];
                dst_px[2] = src_px[0];
                dst_px[3] = src_px[3];
            }
            self.scratch_rb.as_ptr()
        } else {
            pixels.as_ptr()
        };

        let _guard = CudaCtxGuard::push(self.rt.cuda, self.cu_ctx)?;

        let copy = CudaMemcpy2d {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: CU_MEMORYTYPE_HOST,
            src_host: src_ptr.cast(),
            src_device: 0,
            src_array: ptr::null_mut(),
            src_pitch: row_bytes,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: CU_MEMORYTYPE_DEVICE,
            dst_host: ptr::null_mut(),
            dst_device: self.dev_ptr,
            dst_array: ptr::null_mut(),
            dst_pitch: self.dev_pitch,
            width_in_bytes: row_bytes,
            height: usize::try_from(self.height)?,
        };
        let rc = unsafe { (self.rt.cuda.cu_memcpy_2d_v2)(&copy) };
        ensure!(rc == CU_SUCCESS, "cuMemcpy2D_v2 upload failed (rc={rc}).");

        let mut map: NvEncMapInputResource = unsafe { std::mem::zeroed() };
        map.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
        map.registered_resource = self.registered_resource;
        let st =
            unsafe { (self.rt.nvenc.nv_enc_map_input_resource.unwrap())(self.encoder, &mut map) };
        if st != NV_ENC_SUCCESS {
            return Err(nvenc_err(self.rt, self.encoder, "MapInputResource", st));
        }
        let mapped = map.mapped_resource;

        let encode_res = (|| -> Result<Vec<u8>> {
            let mut pic: NvEncPicParams = unsafe { std::mem::zeroed() };
            pic.version = NV_ENC_PIC_PARAMS_VER;
            pic.input_width = self.width;
            pic.input_height = self.height;
            pic.input_pitch = u32::try_from(self.dev_pitch)?;
            pic.encode_pic_flags = if force_keyframe || self.frame_idx == 0 {
                NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS
            } else {
                0
            };
            pic.frame_idx = self.frame_idx;
            pic.input_timestamp = u64::from(self.frame_idx);
            pic.input_duration = 1;
            pic.input_buffer = mapped;
            pic.output_bitstream = self.bitstream_buffer;
            pic.buffer_fmt = map.mapped_buffer_fmt;
            pic.picture_struct = NV_ENC_PIC_STRUCT_FRAME;

            let st =
                unsafe { (self.rt.nvenc.nv_enc_encode_picture.unwrap())(self.encoder, &mut pic) };
            if st != NV_ENC_SUCCESS {
                return Err(nvenc_err(self.rt, self.encoder, "EncodePicture", st));
            }

            let mut lock: NvEncLockBitstream = unsafe { std::mem::zeroed() };
            lock.version = NV_ENC_LOCK_BITSTREAM_VER;
            lock.output_bitstream = self.bitstream_buffer;
            let st =
                unsafe { (self.rt.nvenc.nv_enc_lock_bitstream.unwrap())(self.encoder, &mut lock) };
            if st != NV_ENC_SUCCESS {
                return Err(nvenc_err(self.rt, self.encoder, "LockBitstream", st));
            }

            let bytes = if !lock.bitstream_buffer_ptr.is_null() && lock.bitstream_size_in_bytes > 0
            {
                let slice = unsafe {
                    std::slice::from_raw_parts(
                        lock.bitstream_buffer_ptr.cast::<u8>(),
                        lock.bitstream_size_in_bytes as usize,
                    )
                };
                slice.to_vec()
            } else {
                Vec::new()
            };

            let _ = unsafe {
                (self.rt.nvenc.nv_enc_unlock_bitstream.unwrap())(
                    self.encoder,
                    self.bitstream_buffer,
                )
            };
            self.frame_idx = self.frame_idx.wrapping_add(1);
            Ok(bytes)
        })();

        if let Some(unmap) = self.rt.nvenc.nv_enc_unmap_input_resource {
            let _ = unsafe { unmap(self.encoder, mapped) };
        }
        encode_res
    }
}

#[cfg(target_os = "linux")]
impl Drop for DirectNvencEncoder {
    fn drop(&mut self) {
        if let Ok(_guard) = CudaCtxGuard::push(self.rt.cuda, self.cu_ctx) {
            let mut eos: NvEncPicParams = unsafe { std::mem::zeroed() };
            eos.version = NV_ENC_PIC_PARAMS_VER;
            eos.encode_pic_flags = NV_ENC_PIC_FLAG_EOS;
            if let Some(encode_pic) = self.rt.nvenc.nv_enc_encode_picture {
                let _ = unsafe { encode_pic(self.encoder, &mut eos) };
            }
            if !self.registered_resource.is_null()
                && let Some(unreg) = self.rt.nvenc.nv_enc_unregister_resource
            {
                let _ = unsafe { unreg(self.encoder, self.registered_resource) };
            }
            if !self.bitstream_buffer.is_null()
                && let Some(destroy_bs) = self.rt.nvenc.nv_enc_destroy_bitstream_buffer
            {
                let _ = unsafe { destroy_bs(self.encoder, self.bitstream_buffer) };
            }
            if !self.encoder.is_null()
                && let Some(destroy) = self.rt.nvenc.nv_enc_destroy_encoder
            {
                let _ = unsafe { destroy(self.encoder) };
            }
            if self.dev_ptr != 0 {
                let _ = unsafe { (self.rt.cuda.cu_mem_free_v2)(self.dev_ptr) };
            }
        }
        if !self.cu_ctx.is_null() {
            let _ = unsafe { (self.rt.cuda.cu_ctx_destroy_v2)(self.cu_ctx) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use remoters_core::video::Codec;

    use super::super::nvenc_ffi::*;

    #[test]
    fn nvenc_12_0_abi_struct_sizes_and_offsets_match_official_header() {
        // Exact sizes & field offsets verified with clang against official nvEncodeAPI.h v12.0
        // and dynlink_cuda.h on LP64 platforms (Linux x86_64/aarch64, macOS arm64).
        assert_eq!(size_of::<CudaMemcpy2d>(), 128);
        assert_eq!(offset_of!(CudaMemcpy2d, src_memory_type), 16);
        assert_eq!(offset_of!(CudaMemcpy2d, src_host), 24);
        assert_eq!(offset_of!(CudaMemcpy2d, src_pitch), 48);
        assert_eq!(offset_of!(CudaMemcpy2d, dst_memory_type), 72);
        assert_eq!(offset_of!(CudaMemcpy2d, dst_device), 88);
        assert_eq!(offset_of!(CudaMemcpy2d, dst_pitch), 104);
        assert_eq!(offset_of!(CudaMemcpy2d, width_in_bytes), 112);
        assert_eq!(offset_of!(CudaMemcpy2d, height), 120);

        assert_eq!(size_of::<NvEncGuid>(), 16);
        assert_eq!(size_of::<NvEncodeApiFunctionList>(), 2552);
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_get_encode_guid_count),
            16
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_get_encode_guids),
            40
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_get_input_format_count),
            48
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_get_input_formats),
            56
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_get_encode_caps),
            64
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_initialize_encoder),
            96
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_create_bitstream_buffer),
            120
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_encode_picture),
            136
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_lock_bitstream),
            144
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_map_input_resource),
            208
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_destroy_encoder),
            224
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_open_encode_session_ex),
            240
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_register_resource),
            248
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_reconfigure_encoder),
            264
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_get_last_error_string),
            304
        );
        assert_eq!(
            offset_of!(NvEncodeApiFunctionList, nv_enc_get_encode_preset_config_ex),
            320
        );

        assert_eq!(size_of::<NvEncOpenEncodeSessionExParams>(), 1552);
        assert_eq!(offset_of!(NvEncOpenEncodeSessionExParams, device), 8);
        assert_eq!(offset_of!(NvEncOpenEncodeSessionExParams, api_version), 24);

        assert_eq!(size_of::<NvEncRcParams>(), 128);
        assert_eq!(offset_of!(NvEncRcParams, average_bit_rate), 20);
        assert_eq!(offset_of!(NvEncRcParams, vbv_buffer_size), 28);
        assert_eq!(offset_of!(NvEncRcParams, vbv_initial_delay), 32);
        assert_eq!(offset_of!(NvEncRcParams, flags), 36);
        assert_eq!(offset_of!(NvEncRcParams, low_delay_key_frame_scale), 92);
        assert_eq!(offset_of!(NvEncRcParams, multi_pass), 100);

        assert_eq!(size_of::<NvEncConfigVuiParameters>(), 112);
        assert_eq!(size_of::<NvEncConfigH264>(), 1792);
        assert_eq!(offset_of!(NvEncConfigH264, idr_period), 8);
        assert_eq!(offset_of!(NvEncConfigH264, intra_refresh_period), 52);
        assert_eq!(offset_of!(NvEncConfigH264, intra_refresh_cnt), 56);
        assert_eq!(offset_of!(NvEncConfigH264, h264_vui_parameters), 72);
        assert_eq!(offset_of!(NvEncConfigH264, chroma_format_idc), 192);

        assert_eq!(size_of::<NvEncConfigHevc>(), 1560);
        assert_eq!(offset_of!(NvEncConfigHevc, flags), 16);
        assert_eq!(offset_of!(NvEncConfigHevc, idr_period), 20);
        assert_eq!(offset_of!(NvEncConfigHevc, intra_refresh_period), 24);
        assert_eq!(offset_of!(NvEncConfigHevc, intra_refresh_cnt), 28);
        assert_eq!(offset_of!(NvEncConfigHevc, hevc_vui_parameters), 64);

        assert_eq!(size_of::<NvEncConfigAv1>(), 1552);
        assert_eq!(offset_of!(NvEncConfigAv1, flags), 16);
        assert_eq!(offset_of!(NvEncConfigAv1, idr_period), 20);
        assert_eq!(offset_of!(NvEncConfigAv1, intra_refresh_period), 24);
        assert_eq!(offset_of!(NvEncConfigAv1, intra_refresh_cnt), 28);
        assert_eq!(offset_of!(NvEncConfigAv1, color_primaries), 68);
        assert_eq!(offset_of!(NvEncConfigAv1, color_range), 80);

        assert_eq!(size_of::<NvEncCodecConfig>(), 1792);
        assert_eq!(size_of::<NvEncConfig>(), 3584);
        assert_eq!(offset_of!(NvEncConfig, rc_params), 40);
        assert_eq!(offset_of!(NvEncConfig, encode_codec_config), 168);

        assert_eq!(size_of::<NvEncPresetConfig>(), 5128);
        assert_eq!(offset_of!(NvEncPresetConfig, preset_cfg), 8);

        assert_eq!(size_of::<NvEncInitializeParams>(), 1808);
        assert_eq!(offset_of!(NvEncInitializeParams, encode_width), 36);
        assert_eq!(offset_of!(NvEncInitializeParams, encode_config), 88);
        assert_eq!(offset_of!(NvEncInitializeParams, tuning_info), 136);

        assert_eq!(size_of::<NvEncReconfigureParams>(), 1824);
        assert_eq!(offset_of!(NvEncReconfigureParams, re_init_encode_params), 8);
        assert_eq!(offset_of!(NvEncReconfigureParams, flags), 1816);

        assert_eq!(size_of::<NvEncRegisterResource>(), 1536);
        assert_eq!(offset_of!(NvEncRegisterResource, resource_to_register), 24);
        assert_eq!(offset_of!(NvEncRegisterResource, registered_resource), 32);
        assert_eq!(offset_of!(NvEncRegisterResource, buffer_format), 40);

        assert_eq!(size_of::<NvEncMapInputResource>(), 1544);
        assert_eq!(offset_of!(NvEncMapInputResource, registered_resource), 16);
        assert_eq!(offset_of!(NvEncMapInputResource, mapped_resource), 24);
        assert_eq!(offset_of!(NvEncMapInputResource, mapped_buffer_fmt), 32);

        assert_eq!(size_of::<NvEncCreateBitstreamBuffer>(), 776);
        assert_eq!(offset_of!(NvEncCreateBitstreamBuffer, bitstream_buffer), 16);

        assert_eq!(size_of::<NvEncPicParams>(), 3360);
        assert_eq!(offset_of!(NvEncPicParams, input_buffer), 40);
        assert_eq!(offset_of!(NvEncPicParams, output_bitstream), 48);
        assert_eq!(offset_of!(NvEncPicParams, buffer_fmt), 64);
        assert_eq!(offset_of!(NvEncPicParams, picture_struct), 68);
        assert_eq!(offset_of!(NvEncPicParams, picture_type), 72);

        assert_eq!(size_of::<NvEncLockBitstream>(), 1544);
        assert_eq!(offset_of!(NvEncLockBitstream, output_bitstream), 8);
        assert_eq!(offset_of!(NvEncLockBitstream, frame_idx), 24);
        assert_eq!(offset_of!(NvEncLockBitstream, bitstream_size_in_bytes), 36);
        assert_eq!(offset_of!(NvEncLockBitstream, bitstream_buffer_ptr), 56);
        assert_eq!(offset_of!(NvEncLockBitstream, picture_type), 64);

        assert_eq!(size_of::<NvEncCapsParam>(), 256);
    }

    #[test]
    fn nvenc_version_macros_and_driver_negotiation_are_exact() {
        assert_eq!(NVENCAPI_VERSION, 0x0000_000c);
        assert_eq!(NV_ENCODE_API_FUNCTION_LIST_VER, 0x7002_000c);
        assert_eq!(NV_ENC_CONFIG_VER, 0xf008_000c);
        assert_eq!(NV_ENC_INITIALIZE_PARAMS_VER, 0xf005_000c);
        assert_eq!(NV_ENC_RECONFIGURE_PARAMS_VER, 0xf001_000c);
        assert_eq!(NV_ENC_PRESET_CONFIG_VER, 0xf004_000c);
        assert_eq!(NV_ENC_PIC_PARAMS_VER, 0xf006_000c);
        assert_eq!(NV_ENC_LOCK_BITSTREAM_VER, 0x7002_000c);

        assert_eq!(parse_driver_nvenc_version((12 << 4) | 2), (12, 2));
        assert!(driver_supports_nvenc_12(12 << 4));
        assert!(driver_supports_nvenc_12((12 << 4) | 2));
        assert!(driver_supports_nvenc_12(13 << 4));
        assert!(!driver_supports_nvenc_12((11 << 4) | 1));
    }

    #[test]
    fn single_frame_vbv_and_intra_refresh_configs_are_sunshine_grade() {
        // 20 Mbps @ 60fps -> (20_000_000 / 60) * 1.25 = 416_666 bits
        assert_eq!(single_frame_vbv_bits(20_000_000, 60), 416_666);
        // 70 Mbps @ 120fps -> (70_000_000 / 120) * 1.25 = 729_166 bits
        assert_eq!(single_frame_vbv_bits(70_000_000, 120), 729_166);
        // Low bitrate clamped cleanly to floor
        assert_eq!(single_frame_vbv_bits(500_000, 120), 65_536);

        assert_eq!(intra_refresh_params(60), (120, 15));
        assert_eq!(intra_refresh_params(120), (240, 30));
        let (p, c) = intra_refresh_params(1);
        assert!(c >= 1 && c < p);

        assert_eq!(
            pixel_format_to_nvenc("bgr0"),
            Some(NV_ENC_BUFFER_FORMAT_ARGB)
        );
        assert_eq!(
            pixel_format_to_nvenc("bgra"),
            Some(NV_ENC_BUFFER_FORMAT_ARGB)
        );
        assert_eq!(
            pixel_format_to_nvenc("rgb0"),
            Some(NV_ENC_BUFFER_FORMAT_ABGR)
        );
        assert_eq!(
            pixel_format_to_nvenc("rgba"),
            Some(NV_ENC_BUFFER_FORMAT_ABGR)
        );
        assert_eq!(pixel_format_to_nvenc("nv12"), None);

        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            let mut cfg: NvEncConfig = unsafe { std::mem::zeroed() };
            cfg.rc_params.flags = RC_FLAG_ENABLE_LOOKAHEAD;
            configure_ultra_low_latency(&mut cfg, codec, 60, 35_000_000, true);
            assert_eq!(cfg.version, NV_ENC_CONFIG_VER);
            assert_eq!(cfg.profile_guid, profile_guid(codec));
            assert_eq!(cfg.gop_length, NVENC_INFINITE_GOPLENGTH);
            assert_eq!(cfg.frame_interval_p, 1);
            assert_eq!(cfg.rc_params.rate_control_mode, NV_ENC_PARAMS_RC_CBR);
            assert_eq!(cfg.rc_params.average_bit_rate, 35_000_000);
            assert_eq!(cfg.rc_params.max_bit_rate, 35_000_000);
            assert_eq!(
                cfg.rc_params.vbv_buffer_size,
                single_frame_vbv_bits(35_000_000, 60)
            );
            assert_eq!(
                cfg.rc_params.vbv_initial_delay,
                cfg.rc_params.vbv_buffer_size
            );
            assert_eq!(cfg.rc_params.flags & RC_FLAG_ENABLE_LOOKAHEAD, 0);
            assert_ne!(cfg.rc_params.flags & RC_FLAG_ZERO_REORDER_DELAY, 0);

            apply_live_bitrate(&mut cfg, 60, 50_000_000);
            assert_eq!(cfg.rc_params.average_bit_rate, 50_000_000);
            assert_eq!(
                cfg.rc_params.vbv_buffer_size,
                single_frame_vbv_bits(50_000_000, 60)
            );
            assert_eq!(
                cfg.rc_params.vbv_initial_delay,
                cfg.rc_params.vbv_buffer_size
            );
        }
    }
}
