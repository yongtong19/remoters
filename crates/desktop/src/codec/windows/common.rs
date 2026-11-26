//! Shared Windows Win32/COM/D3D11/Media Foundation helpers, constants,
//! cursor polling, synchronization primitives, and hardware codec probes.

use std::{
    sync::{Condvar, Mutex, OnceLock},
    time::Duration,
};

use anyhow::{Context, Result};
use gpui::ID3D11Device;
use remoters_core::{session::CursorShape, video::Codec};
use windows::Win32::{
    Foundation::{
        CloseHandle, ERROR_DEVICE_NOT_CONNECTED, HANDLE, HMODULE, LPARAM, POINT, RECT, TRUE,
    },
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN},
        Direct3D10::ID3D10Multithread,
        Direct3D11::{
            D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
            D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11CreateDevice, ID3D11DeviceContext,
            ID3D11VideoContext, ID3D11VideoDevice,
        },
        Dxgi::{
            Common::{DXGI_COLOR_SPACE_TYPE, DXGI_FORMAT},
            CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
            IDXGIOutput1,
        },
        Gdi::{EnumDisplayMonitors, HDC, HMONITOR, MONITOR_DEFAULTTONULL, MonitorFromPoint},
    },
    Media::MediaFoundation::{
        IMFActivate, IMFDXGIDeviceManager, MF_VERSION, MFCreateDXGIDeviceManager,
        MFMediaType_Video, MFSTARTUP_LITE, MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_DECODER,
        MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_REGISTER_TYPE_INFO, MFTEnumEx,
        MFVideoFormat_HEVC,
    },
    System::{
        Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        Threading::{
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW,
            CreateWaitableTimerW, SetEvent, TIMER_MODIFY_STATE,
        },
    },
    UI::WindowsAndMessaging::{
        CURSOR_SHOWING, CURSORINFO, GetCursorInfo, HCURSOR, IDC_ARROW, IDC_CROSS, IDC_HAND,
        IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE,
        LoadCursorW,
    },
};
use windows_core::{BOOL, GUID, Interface, PCSTR, PCWSTR};

use super::{nvenc, nvenc_ffi};

pub struct CursorDetector {
    standard: Vec<(HCURSOR, CursorShape)>,
}

impl CursorDetector {
    pub fn new() -> Self {
        let pairs = [
            (IDC_ARROW, CursorShape::Arrow),
            (IDC_IBEAM, CursorShape::IBeam),
            (IDC_HAND, CursorShape::PointingHand),
            (IDC_CROSS, CursorShape::Crosshair),
            (IDC_SIZEWE, CursorShape::ResizeLeftRight),
            (IDC_SIZENS, CursorShape::ResizeUpDown),
            (IDC_SIZENWSE, CursorShape::ResizeUpLeftDownRight),
            (IDC_SIZENESW, CursorShape::ResizeUpRightDownLeft),
            (IDC_SIZEALL, CursorShape::OpenHand),
            (IDC_NO, CursorShape::OperationNotAllowed),
        ];
        let mut standard = Vec::with_capacity(pairs.len());
        for (idc, shape) in pairs {
            if let Ok(handle) = unsafe { LoadCursorW(None, idc) } {
                standard.push((handle, shape));
            }
        }
        Self { standard }
    }

    pub fn poll(&self) -> CursorShape {
        let mut info = CURSORINFO {
            cbSize: std::mem::size_of::<CURSORINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetCursorInfo(&mut info) }.is_err() {
            return CursorShape::Arrow;
        }
        if (info.flags.0 & CURSOR_SHOWING.0) == 0 {
            return CursorShape::Hidden;
        }
        self.standard
            .iter()
            .find_map(|&(handle, shape)| (handle == info.hCursor).then_some(shape))
            .unwrap_or(CursorShape::Arrow)
    }
}

// D3D11_VIDEO_PROCESSOR_COLOR_SPACE bitfield:
// bit 2 = YCbCr_Matrix (1 = BT.709), bits 4..5 = Nominal_Range (2 = 0..255 Full Range).
pub(super) const BT709_FULL_RANGE: D3D11_VIDEO_PROCESSOR_COLOR_SPACE =
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
        _bitfield: (1 << 2) | (2 << 4),
    };

// WinSDK FourCC Media Foundation subtypes ('AV01' and 'P010').
pub(super) const MF_VIDEO_FORMAT_AV1: GUID =
    GUID::from_u128(0x31305641_0000_0010_8000_00AA00389B71);
pub(super) const MF_VIDEO_FORMAT_P010: GUID =
    GUID::from_u128(0x30313050_0000_0010_8000_00AA00389B71);

// Documented WinSDK DXGI_FORMAT values (`dxgiformat.h`).
pub(super) const DXGI_FORMAT_R16G16B16A16_FLOAT: DXGI_FORMAT = DXGI_FORMAT(10);
pub(super) const DXGI_FORMAT_R10G10B10A2_UNORM: DXGI_FORMAT = DXGI_FORMAT(24);
pub(super) const DXGI_FORMAT_R16G16_UNORM: DXGI_FORMAT = DXGI_FORMAT(35);
pub(super) const DXGI_FORMAT_R16_UNORM: DXGI_FORMAT = DXGI_FORMAT(56);
pub(super) const DXGI_FORMAT_P010: DXGI_FORMAT = DXGI_FORMAT(104);
pub(super) const D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT: u32 = 1;

// Documented WinSDK DXGI_COLOR_SPACE_TYPE values (`dxgicommon.h`).
pub(super) const DXGI_CS_RGB_FULL_G22_NONE_P709: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_TYPE(0);
pub(super) const DXGI_CS_RGB_FULL_G10_NONE_P709: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_TYPE(1);
pub(super) const DXGI_CS_YCBCR_FULL_G22_LEFT_P709: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_TYPE(9);
pub(super) const DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020: DXGI_COLOR_SPACE_TYPE =
    DXGI_COLOR_SPACE_TYPE(13);

// Documented WinSDK Media Foundation attribute GUIDs & enum values (`mfapi.h` / `mfobjects.h`).
pub(super) const MF_MT_TRANSFER_FUNCTION_GUID: GUID =
    GUID::from_u128(0x5fb0fce9_be5c_4935_a811_ec838f8eed93);
pub(super) const MF_MT_VIDEO_PRIMARIES_GUID: GUID =
    GUID::from_u128(0xdbfbe4d7_0740_4ee0_8192_850ab0e21935);
pub(super) const MF_VIDEO_PRIMARIES_BT2020: u32 = 9;
pub(super) const MF_VIDEO_TRANS_FUNC_2084: u32 = 15;
pub(super) const MF_VIDEO_TRANSFER_MATRIX_BT2020_10: u32 = 4;
pub(super) const MF_NOMINAL_RANGE_16_235: u32 = 2;

const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(u_period: u32) -> u32;
    fn timeEndPeriod(u_period: u32) -> u32;
}

unsafe extern "C" {
    pub(super) fn remoters_nudge_monitor(x: i32, y: i32);
    pub(super) fn remoters_nudge_pointer(x: i32, y: i32);
}

pub(super) struct TimerResolutionGuard(bool);

impl TimerResolutionGuard {
    pub(super) fn new() -> Self {
        Self(unsafe { timeBeginPeriod(1) } == 0)
    }
}

impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { timeEndPeriod(1) };
        }
    }
}

fn qpc_frequency() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut qpf = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut qpf);
        }
        qpf.max(1)
    })
}

pub(super) fn qpc_to_100ns(qpc: i64) -> i64 {
    let freq = qpc_frequency();
    (i128::from(qpc) * 10_000_000 / i128::from(freq)) as i64
}

pub(super) fn query_qpc_100ns() -> i64 {
    let mut qpc = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut qpc);
    }
    qpc_to_100ns(qpc)
}

pub(super) fn pack_u64(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

pub(super) fn unpack_u64(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, value as u32)
}

pub(super) struct OwnedHandle(pub(super) HANDLE);

// SAFETY: Win32 Event/Timer HANDLEs are safe to share and signal across threads.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    pub(super) fn new_event() -> Result<Self> {
        let handle = unsafe { CreateEventW(None, false, false, None) }
            .context("Could not create Windows wake event.")?;
        Ok(Self(handle))
    }

    pub(super) fn new_high_res_timer() -> Option<Self> {
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_MODIFY_STATE.0 | SYNCHRONIZE_ACCESS,
            )
        }
        .or_else(|_| unsafe { CreateWaitableTimerW(None, false, None) })
        .ok()?;
        Some(Self(handle))
    }

    pub(super) fn signal(&self) {
        let _ = unsafe { SetEvent(self.0) };
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Unified cross-thread wakeup combining a `Condvar` (for low-overhead direct
/// NVENC waiting) and a Win32 Event (for Media Foundation async `WaitForMultipleObjects`).
pub(super) struct CaptureWake {
    event: OwnedHandle,
    seq: Mutex<u64>,
    condvar: Condvar,
}

impl CaptureWake {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            event: OwnedHandle::new_event()?,
            seq: Mutex::new(0),
            condvar: Condvar::new(),
        })
    }

    pub(super) fn signal(&self) {
        {
            let mut seq = self.seq.lock().unwrap();
            *seq = seq.wrapping_add(1);
        }
        self.condvar.notify_all();
        self.event.signal();
    }

    pub(super) fn snapshot(&self) -> u64 {
        *self.seq.lock().unwrap()
    }

    pub(super) fn wait_timeout(&self, seen: u64, timeout: Duration) {
        let guard = self.seq.lock().unwrap();
        if *guard != seen {
            return;
        }
        let _ = self.condvar.wait_timeout(guard, timeout);
    }

    pub(super) fn raw_event(&self) -> HANDLE {
        self.event.0
    }
}

pub(super) struct ComThreadGuard(bool);

impl ComThreadGuard {
    pub(super) fn new() -> Self {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self(hr.is_ok())
    }
}

impl Drop for ComThreadGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

pub(super) struct ApartmentGuard {
    com: bool,
    mf: bool,
}

impl ApartmentGuard {
    pub(super) fn start() -> Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK or S_FALSE (already initialized on this thread) are both valid.
        hr.ok().context("Could not initialize COM apartment.")?;
        let com = true;
        if let Err(err) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) } {
            unsafe { CoUninitialize() };
            return Err(err).context("Could not start Media Foundation.");
        }
        Ok(Self { com, mf: true })
    }
}

impl Drop for ApartmentGuard {
    fn drop(&mut self) {
        if self.mf {
            let _ = unsafe { MFShutdown() };
        }
        if self.com {
            unsafe { CoUninitialize() };
        }
    }
}

pub(super) struct VideoDevice {
    // Declared before `_guard` so COM interfaces are released before MFShutdown/CoUninitialize.
    pub(super) manager: IMFDXGIDeviceManager,
    pub(super) video_context: ID3D11VideoContext,
    pub(super) video: ID3D11VideoDevice,
    pub(super) context: ID3D11DeviceContext,
    pub(super) device: ID3D11Device,
    _guard: ApartmentGuard,
}

impl VideoDevice {
    pub(super) fn new(supplied: Option<ID3D11Device>) -> Result<Self> {
        Self::new_with_adapter(supplied, None)
    }

    pub(super) fn new_with_adapter(
        supplied: Option<ID3D11Device>,
        adapter: Option<&IDXGIAdapter1>,
    ) -> Result<Self> {
        let guard = ApartmentGuard::start()?;
        let (device, context) = if let Some(device) = supplied {
            let context = unsafe { device.GetImmediateContext() }
                .context("Could not get D3D11 immediate context.")?;
            (device, context)
        } else {
            let mut device = None;
            let mut context = None;
            let dxgi_adapter: Option<IDXGIAdapter> = adapter.map(|a| a.cast()).transpose()?;
            let driver_type = if dxgi_adapter.is_some() {
                D3D_DRIVER_TYPE_UNKNOWN
            } else {
                D3D_DRIVER_TYPE_HARDWARE
            };
            unsafe {
                D3D11CreateDevice(
                    dxgi_adapter.as_ref(),
                    driver_type,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            }
            .context("Could not create hardware D3D11 video device.")?;
            (
                device.context("Missing D3D11 device.")?,
                context.context("Missing D3D11 context.")?,
            )
        };
        if let Ok(dxgi) = device.cast::<IDXGIDevice>() {
            let _ = unsafe { dxgi.SetGPUThreadPriority(7) };
        }
        let multithread: ID3D10Multithread = context
            .cast()
            .context("Could not enable D3D11 multithread protection.")?;
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
        let video: ID3D11VideoDevice = device
            .cast()
            .context("D3D11 device does not support video acceleration.")?;
        let video_context: ID3D11VideoContext = context
            .cast()
            .context("D3D11 context does not support video acceleration.")?;
        let mut token = 0u32;
        let mut manager = None;
        unsafe { MFCreateDXGIDeviceManager(&mut token, &mut manager) }
            .context("Could not create DXGI device manager.")?;
        let manager = manager.context("Missing DXGI device manager.")?;
        unsafe { manager.ResetDevice(&device, token) }
            .context("Could not bind D3D11 device to DXGI device manager.")?;
        Ok(Self {
            manager,
            video_context,
            video,
            context,
            device,
            _guard: guard,
        })
    }
}

/// Ensures the calling capture/encode thread operates in Per-Monitor DPI-Aware V2
/// mode so Win32 monitor coordinates and WGC capture sizes are never DPI-virtualized.
pub(super) fn enable_per_monitor_dpi_v2() {
    type SetThreadDpiAwarenessContextFn =
        unsafe extern "system" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void;
    // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 = (DPI_AWARENESS_CONTEXT)-4
    const PER_MONITOR_AWARE_V2: isize = -4;
    let user32_name: [u16; 11] = [
        b'u' as u16,
        b's' as u16,
        b'e' as u16,
        b'r' as u16,
        b'3' as u16,
        b'2' as u16,
        b'.' as u16,
        b'd' as u16,
        b'l' as u16,
        b'l' as u16,
        0,
    ];
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR(user32_name.as_ptr())) else {
            return;
        };
        let Some(proc) = GetProcAddress(module, PCSTR(b"SetThreadDpiAwarenessContext\0".as_ptr()))
        else {
            return;
        };
        let set_ctx: SetThreadDpiAwarenessContextFn = std::mem::transmute(proc);
        let _ = set_ctx(PER_MONITOR_AWARE_V2 as *mut std::ffi::c_void);
    }
}

struct MonitorIdMatch {
    target_id: u32,
    found: Option<HMONITOR>,
}

unsafe extern "system" fn match_monitor_by_id(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let state = unsafe { &mut *(lparam.0 as *mut MonitorIdMatch) };
    if (hmonitor.0 as usize as u32) == state.target_id {
        state.found = Some(hmonitor);
        return BOOL(0);
    }
    TRUE
}

/// Resolves the target `HMONITOR` by matching `monitor_id` directly against live
/// enumerated handles first (preventing topology/DPI coordinate races from picking a
/// physical 4K monitor instead of the SudoVDA virtual display), falling back to
/// `MonitorFromPoint(center)` only if the handle changed.
pub(super) fn resolve_monitor_handle(
    monitor_id: Option<u32>,
    center: (i32, i32),
) -> Result<HMONITOR> {
    if let Some(target_id) = monitor_id.filter(|&id| id != 0) {
        let mut state = MonitorIdMatch {
            target_id,
            found: None,
        };
        let _ = unsafe {
            EnumDisplayMonitors(
                None,
                None,
                Some(match_monitor_by_id),
                LPARAM(&mut state as *mut MonitorIdMatch as isize),
            )
        };
        if let Some(monitor) = state.found {
            return Ok(monitor);
        }
    }
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: center.0,
                y: center.1,
            },
            MONITOR_DEFAULTTONULL,
        )
    };
    if monitor.is_invalid() {
        return Err(windows_core::Error::from(ERROR_DEVICE_NOT_CONNECTED).into());
    }
    Ok(monitor)
}

pub(super) fn find_monitor_output(monitor: HMONITOR) -> Result<(IDXGIAdapter1, IDXGIOutput1)> {
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.context("Could not create DXGI factory.")?;
    for adapter_idx in 0u32.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_idx) }) else {
            break;
        };
        for output_idx in 0u32.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_idx) }) else {
                break;
            };
            let desc = unsafe { output.GetDesc() }?;
            if desc.Monitor == monitor {
                let output1: IDXGIOutput1 = output
                    .cast()
                    .context("DXGI output does not support Desktop Duplication.")?;
                return Ok((adapter, output1));
            }
        }
    }
    anyhow::bail!("Could not find DXGI output for the selected monitor.")
}

pub(super) fn enumerate_mfts(
    category: GUID,
    flags: MFT_ENUM_FLAG,
    input: Option<&MFT_REGISTER_TYPE_INFO>,
    output: Option<&MFT_REGISTER_TYPE_INFO>,
) -> Result<Vec<IMFActivate>> {
    let mut raw_activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            category,
            flags,
            input.map(|info| info as *const _),
            output.map(|info| info as *const _),
            &mut raw_activates,
            &mut count,
        )
    }?;
    if !raw_activates.is_null() && count > 0 {
        let slice = unsafe { std::slice::from_raw_parts_mut(raw_activates, count as usize) };
        let list = slice.iter_mut().filter_map(Option::take).collect();
        unsafe { CoTaskMemFree(Some(raw_activates.cast())) };
        Ok(list)
    } else {
        if !raw_activates.is_null() {
            unsafe { CoTaskMemFree(Some(raw_activates.cast())) };
        }
        Ok(Vec::new())
    }
}

fn probe_hardware_mft(subtype: GUID, is_encoder: bool, flags: MFT_ENUM_FLAG) -> bool {
    std::thread::spawn(move || {
        let Ok(_guard) = ApartmentGuard::start() else {
            return false;
        };
        let info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: subtype,
        };
        let (category, input, output) = if is_encoder {
            (MFT_CATEGORY_VIDEO_ENCODER, None, Some(&info))
        } else {
            (MFT_CATEGORY_VIDEO_DECODER, Some(&info), None)
        };
        enumerate_mfts(category, flags, input, output).is_ok_and(|list| !list.is_empty())
    })
    .join()
    .unwrap_or(false)
}

fn probe_nvenc_encoder(codec: Codec) -> bool {
    if nvenc_ffi::env_encoder_preference() == nvenc_ffi::WinEncoderPreference::Mft {
        return false;
    }
    std::thread::spawn(move || {
        let Ok(gpu) = VideoDevice::new(None) else {
            return false;
        };
        nvenc::probe_codec_support(&gpu.device, codec)
    })
    .join()
    .unwrap_or(false)
}

pub fn supports_av1_hardware_encoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        probe_hardware_mft(
            MF_VIDEO_FORMAT_AV1,
            true,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        ) || probe_nvenc_encoder(Codec::Av1)
    })
}

pub fn supports_av1_hardware_decoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        probe_hardware_mft(
            MF_VIDEO_FORMAT_AV1,
            false,
            MFT_ENUM_FLAG_HARDWARE
                | MFT_ENUM_FLAG_SYNCMFT
                | MFT_ENUM_FLAG_ASYNCMFT
                | MFT_ENUM_FLAG_SORTANDFILTER,
        )
    })
}

pub fn supports_hevc_hardware_encoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        probe_hardware_mft(
            MFVideoFormat_HEVC,
            true,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        ) || probe_nvenc_encoder(Codec::Hevc)
    })
}

pub fn supports_hevc_hardware_decoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        probe_hardware_mft(
            MFVideoFormat_HEVC,
            false,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        )
    })
}
