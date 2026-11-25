use std::{
    collections::VecDeque,
    mem::ManuallyDrop,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, ensure};
use gpui::{ID3D11Device, ID3D11Texture2D};
use remoters_core::video::{AccessUnit, Codec, Format, MAX_ACCESS_UNIT, ReceivedFrame, nal_units};
use tokio::sync::mpsc;
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{
            CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, E_NOTIMPL, ERROR_DEVICE_NOT_CONNECTED,
            HANDLE, HMODULE, POINT, RECT, TRUE,
        },
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN},
            Direct3D10::ID3D10Multithread,
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE, D3D11_VIDEO_PROCESSOR_CAPS,
                D3D11_VIDEO_PROCESSOR_COLOR_SPACE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
                D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
                D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_STREAM,
                D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
                D3D11_VPOV_DIMENSION_TEXTURE2D, D3D11CreateDevice, ID3D11DeviceContext,
                ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
                ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
                ID3D11VideoProcessorOutputView,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC},
                CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_INVALID_CALL,
                DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
                DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR,
                DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME, IDXGIAdapter, IDXGIAdapter1,
                IDXGIDevice, IDXGIFactory1, IDXGIKeyedMutex, IDXGIOutput1, IDXGIOutputDuplication,
                IDXGIResource,
            },
            Gdi::{HMONITOR, MONITOR_DEFAULTTONULL, MonitorFromPoint},
        },
        Media::MediaFoundation::{
            CMSH264DecoderMFT, CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMaxBitRate,
            CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonQualityVsSpeed,
            CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
            CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
            ICodecAPI, IMF2DBuffer, IMFActivate, IMFAsyncCallback, IMFAsyncCallback_Impl,
            IMFAsyncResult, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaEvent,
            IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFShutdown, IMFTransform,
            METransformHaveOutput, METransformNeedInput, MF_E_INVALIDMEDIATYPE, MF_E_NOTACCEPTING,
            MF_E_SHUTDOWN, MF_E_TOPO_CODEC_NOT_FOUND, MF_E_TRANSFORM_NEED_MORE_INPUT,
            MF_E_TRANSFORM_STREAM_CHANGE, MF_E_UNSUPPORTED_D3D_TYPE, MF_LOW_LATENCY,
            MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
            MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE,
            MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX,
            MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFCreateAlignedMemoryBuffer,
            MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
            MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFNominalRange_0_255,
            MFSampleExtension_CleanPoint, MFShutdown, MFSTARTUP_LITE, MFStartup,
            MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
            MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_FLUSH,
            MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
            MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER,
            MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
            MFT_OUTPUT_STREAM_INFO, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO,
            MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
            MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR,
            eAVEncCommonRateControlMode_LowDelayVBR,
            eAVEncCommonRateControlMode_PeakConstrainedVBR, eAVEncH264VProfile_Base,
        },
        System::{
            Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            Threading::{
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CancelWaitableTimer, CreateEventW,
                CreateWaitableTimerExW, CreateWaitableTimerW, GetCurrentProcess, GetCurrentThread,
                SetEvent, SetThreadPriority, SetWaitableTimer, THREAD_PRIORITY_HIGHEST,
                TIMER_MODIFY_STATE, WaitForMultipleObjects, WaitForSingleObject,
            },
            Variant::VARIANT,
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
    },
};
use windows_core::{GUID, HRESULT, IInspectable, IUnknownImpl, Interface, factory, implement};

use crate::platform::VideoFrame;

// D3D11_VIDEO_PROCESSOR_COLOR_SPACE bitfield:
// bit 2 = YCbCr_Matrix (1 = BT.709), bits 4..5 = Nominal_Range (2 = 0..255 Full Range).
const BT709_FULL_RANGE: D3D11_VIDEO_PROCESSOR_COLOR_SPACE = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
    _bitfield: (1 << 2) | (2 << 4),
};

const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(u_period: u32) -> u32;
    fn timeEndPeriod(u_period: u32) -> u32;
}

struct TimerResolutionGuard(bool);

impl TimerResolutionGuard {
    fn new() -> Self {
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

fn qpc_to_100ns(qpc: i64) -> i64 {
    let freq = qpc_frequency();
    (i128::from(qpc) * 10_000_000 / i128::from(freq)) as i64
}

fn query_qpc_100ns() -> i64 {
    let mut qpc = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut qpc);
    }
    qpc_to_100ns(qpc)
}

fn pack_u64(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn unpack_u64(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, value as u32)
}

struct OwnedHandle(HANDLE);

// SAFETY: Win32 Event/Timer HANDLEs are safe to share and signal across threads.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    fn new_event() -> Result<Self> {
        let handle = unsafe { CreateEventW(None, false, false, None) }
            .context("Could not create Windows wake event.")?;
        Ok(Self(handle))
    }

    fn new_high_res_timer() -> Option<Self> {
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

    fn duplicate(&self) -> Result<Self> {
        let mut out = HANDLE::default();
        let process = unsafe { GetCurrentProcess() };
        unsafe {
            DuplicateHandle(
                process,
                self.0,
                process,
                &mut out,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .context("Could not duplicate Windows wake handle.")?;
        Ok(Self(out))
    }

    fn signal(&self) {
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

struct ApartmentGuard {
    com: bool,
    mf: bool,
}

impl ApartmentGuard {
    fn start() -> Result<Self> {
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

struct VideoDevice {
    // Declared before `_guard` so COM interfaces are released before MFShutdown/CoUninitialize.
    manager: IMFDXGIDeviceManager,
    video_context: ID3D11VideoContext,
    video: ID3D11VideoDevice,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    _guard: ApartmentGuard,
}

impl VideoDevice {
    fn new(supplied: Option<ID3D11Device>) -> Result<Self> {
        Self::new_with_adapter(supplied, None)
    }

    fn new_with_adapter(
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

fn find_monitor_output(monitor: HMONITOR) -> Result<(IDXGIAdapter1, IDXGIOutput1)> {
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

// ---------------------------------------------------------------------------
// Hardware H.264 Decoder (Media Foundation + D3D11 VideoProcessor)
// ---------------------------------------------------------------------------

struct NativeDecoder {
    processor: Option<ID3D11VideoProcessor>,
    enumerator: Option<ID3D11VideoProcessorEnumerator>,
    transform: IMFTransform,
    gpu: VideoDevice,
    frame_width: u32,
    frame_height: u32,
    target_width: u32,
    target_height: u32,
}

impl NativeDecoder {
    fn new(device: ID3D11Device) -> Result<Self> {
        let gpu = VideoDevice::new(Some(device))?;
        let transform: IMFTransform =
            unsafe { CoCreateInstance(&CMSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                .context("Could not create Windows H.264 hardware decoder MFT.")?;
        let attributes = unsafe { transform.GetAttributes() }
            .context("Could not query H.264 decoder attributes.")?;
        let aware = unsafe { attributes.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0);
        if aware == 0 {
            return Err(windows_core::Error::from(MF_E_UNSUPPORTED_D3D_TYPE).into());
        }
        unsafe { attributes.SetUINT32(&MF_LOW_LATENCY, 1) }?;
        unsafe {
            transform.ProcessMessage(
                MFT_MESSAGE_SET_D3D_MANAGER,
                gpu.manager.as_raw() as usize,
            )
        }?;

        let input = unsafe { MFCreateMediaType() }?;
        unsafe {
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0)?;
        }

        let mut decoder = Self {
            processor: None,
            enumerator: None,
            transform,
            gpu,
            frame_width: 0,
            frame_height: 0,
            target_width: 0,
            target_height: 0,
        };
        decoder.set_output()?;
        unsafe {
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(decoder)
    }

    fn set_output(&mut self) -> Result<()> {
        for index in 0u32.. {
            let output = unsafe { self.transform.GetOutputAvailableType(0, index) }?;
            let subtype = unsafe { output.GetGUID(&MF_MT_SUBTYPE) }?;
            if subtype != MFVideoFormat_NV12 {
                continue;
            }
            unsafe { self.transform.SetOutputType(0, &output, 0) }?;
            if let Ok(size) = unsafe { output.GetUINT64(&MF_MT_FRAME_SIZE) } {
                let (w, h) = unpack_u64(size);
                self.frame_width = w;
                self.frame_height = h;
            }
            return Ok(());
        }
        Err(anyhow!("Windows hardware decoder offers no NV12 output."))
    }

    fn submit(&mut self, data: &[u8], time: i64) -> Result<Option<VideoFrame>> {
        let size = u32::try_from(data.len())?;
        let buffer = unsafe { MFCreateMemoryBuffer(size) }?;
        let mut dest = std::ptr::null_mut();
        unsafe {
            buffer.Lock(&mut dest, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(size)?;
        }
        let sample = unsafe { MFCreateSample() }?;
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
        }
        let mut latest = None;
        let result = unsafe { self.transform.ProcessInput(0, &sample, 0) };
        if let Err(err) = result {
            if err.code() == MF_E_NOTACCEPTING {
                if let Some(frame) = self.drain()? {
                    latest = Some(frame);
                }
                unsafe { self.transform.ProcessInput(0, &sample, 0) }?;
            } else {
                return Err(err.into());
            }
        }
        if let Some(frame) = self.drain()? {
            latest = Some(frame);
        }
        Ok(latest)
    }

    fn drain(&mut self) -> Result<Option<VideoFrame>> {
        let mut latest = None;
        for _ in 0..8 {
            let info = unsafe { self.transform.GetOutputStreamInfo(0) }?;
            let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            if (info.dwFlags & provides) == 0 {
                return Err(windows_core::Error::from(MF_E_UNSUPPORTED_D3D_TYPE).into());
            }
            let mut output = [MFT_OUTPUT_DATA_BUFFER::default()];
            let mut status = 0u32;
            let result = unsafe { self.transform.ProcessOutput(0, &mut output, &mut status) };
            let sample = unsafe { ManuallyDrop::take(&mut output[0].pSample) };
            let _events = unsafe { ManuallyDrop::take(&mut output[0].pEvents) };
            match result {
                Ok(()) => {
                    if let Some(sample) = sample {
                        latest = Some(self.present(&sample)?);
                    }
                }
                Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                Err(err) if err.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.set_output()?;
                }
                Err(err) => return Err(err.into()),
            }
        }
        Ok(latest)
    }

    fn ensure_processor(&mut self, width: u32, height: u32) -> Result<()> {
        if self.processor.is_some() && self.target_width == width && self.target_height == height {
            return Ok(());
        }
        self.processor = None;
        self.enumerator = None;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputWidth: width,
            InputHeight: height,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            ..Default::default()
        };
        let enumerator = unsafe { self.gpu.video.CreateVideoProcessorEnumerator(&content) }?;
        let processor = unsafe { self.gpu.video.CreateVideoProcessor(&enumerator, 0) }?;
        self.enumerator = Some(enumerator);
        self.processor = Some(processor);
        self.target_width = width;
        self.target_height = height;
        Ok(())
    }

    fn present(&mut self, sample: &IMFSample) -> Result<VideoFrame> {
        let buffer = unsafe { sample.GetBufferByIndex(0) }?;
        let gpu_buffer: IMFDXGIBuffer = buffer
            .cast()
            .context("Hardware decoder did not return a DXGI surface.")?;
        let mut picture: Option<ID3D11Texture2D> = None;
        unsafe {
            gpu_buffer.GetResource(&ID3D11Texture2D::IID, std::ptr::from_mut(&mut picture).cast())
        }?;
        let picture = picture.context("Missing decoded D3D11 texture.")?;
        let subresource = unsafe { gpu_buffer.GetSubresourceIndex() }?;
        let mut layout = D3D11_TEXTURE2D_DESC::default();
        unsafe { picture.GetDesc(&mut layout) };
        let width = if self.frame_width != 0 {
            self.frame_width
        } else {
            layout.Width
        };
        let height = if self.frame_height != 0 {
            self.frame_height
        } else {
            layout.Height
        };
        if width == 0
            || height == 0
            || width > layout.Width
            || height > layout.Height
            || width > 3840
            || height > 3840
            || u64::from(width) * u64::from(height) > 3840 * 2160
        {
            return Err(windows_core::Error::from(MF_E_INVALIDMEDIATYPE).into());
        }
        self.ensure_processor(width, height)?;
        let enumerator = self.enumerator.as_ref().unwrap();
        let processor = self.processor.as_ref().unwrap();

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            ..Default::default()
        };
        let mut target = None;
        unsafe { self.gpu.device.CreateTexture2D(&desc, None, Some(&mut target)) }?;
        let target = target.context("Could not allocate BGRA render target texture.")?;

        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let mut output_view = None;
        unsafe {
            self.gpu.video.CreateVideoProcessorOutputView(
                &target,
                enumerator,
                &out_desc,
                Some(&mut output_view),
            )
        }?;
        let output_view = output_view.context("Could not create video processor output view.")?;

        let mut in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0::default(),
            ..Default::default()
        };
        in_desc.Anonymous.Texture2D.ArraySlice = subresource / layout.MipLevels.max(1);
        in_desc.Anonymous.Texture2D.MipSlice = subresource % layout.MipLevels.max(1);
        let mut input_view = None;
        unsafe {
            self.gpu.video.CreateVideoProcessorInputView(
                &picture,
                enumerator,
                &in_desc,
                Some(&mut input_view),
            )
        }?;
        let input_view = input_view.context("Could not create video processor input view.")?;

        let rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(input_view)),
            ..Default::default()
        }];
        unsafe {
            self.gpu
                .video_context
                .VideoProcessorSetStreamSourceRect(processor, 0, true, Some(&rect));
            self.gpu
                .video_context
                .VideoProcessorSetStreamDestRect(processor, 0, true, Some(&rect));
            self.gpu
                .video_context
                .VideoProcessorSetStreamAutoProcessingMode(processor, 0, false);
            self.gpu
                .video_context
                .VideoProcessorSetStreamColorSpace(processor, 0, &BT709_FULL_RANGE);
            self.gpu.video_context.VideoProcessorSetOutputAlphaFillMode(
                processor,
                D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE,
                0,
            );
            let blt = self
                .gpu
                .video_context
                .VideoProcessorBlt(processor, &output_view, 0, &streams);
            let _ = ManuallyDrop::take(&mut streams[0].pInputSurface);
            blt?;
            self.gpu.context.Flush();
        }

        Ok(VideoFrame {
            surface: target,
            width,
            height,
        })
    }
}

pub struct Decoder {
    device: ID3D11Device,
    inner: Option<NativeDecoder>,
}

impl Decoder {
    pub fn new(device: ID3D11Device) -> Self {
        Self {
            device,
            inner: None,
        }
    }

    pub fn decode(&mut self, frame: &ReceivedFrame) -> Result<Option<VideoFrame>> {
        ensure!(
            frame.codec == Codec::H264,
            "The Windows hardware decoder requires H.264."
        );
        let decoder = match self.inner.as_mut() {
            Some(decoder) => decoder,
            None => self.inner.insert(NativeDecoder::new(self.device.clone())?),
        };
        let time = i64::try_from(u128::from(frame.rtp_timestamp) * 10_000_000 / 90_000)?;
        decoder.submit(&frame.bytes, time)
    }
}

// ---------------------------------------------------------------------------
// Event-driven Hardware H.264 Encoder (WGC + Media Foundation async MFT)
// ---------------------------------------------------------------------------

struct EventQueueState {
    pending: VecDeque<IMFMediaEvent>,
    source: Option<IMFMediaEventGenerator>,
    error: Option<HRESULT>,
    stopped: bool,
}

struct EventQueue {
    state: Mutex<EventQueueState>,
    wake: Arc<OwnedHandle>,
}

#[implement(IMFAsyncCallback)]
struct EventRelay {
    queue: Arc<EventQueue>,
}

impl IMFAsyncCallback_Impl for EventRelay_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows_core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, result: windows_core::Ref<'_, IMFAsyncResult>) -> windows_core::Result<()> {
        let source = {
            let state = self.queue.state.lock().unwrap();
            if state.stopped {
                return Ok(());
            }
            match state.source.clone() {
                Some(source) => source,
                None => return Ok(()),
            }
        };
        let end = unsafe { source.EndGetEvent(result.as_ref()) };
        let mut rearm = false;
        {
            let mut state = self.queue.state.lock().unwrap();
            if state.stopped {
                return Ok(());
            }
            match end {
                Ok(event) => {
                    state.pending.push_back(event);
                    rearm = true;
                }
                Err(err) if err.code() != MF_E_SHUTDOWN => {
                    if state.error.is_none() {
                        state.error = Some(err.code());
                    }
                }
                Err(_) => {}
            }
        }
        self.queue.wake.signal();
        if rearm {
            let callback = self.as_interface::<IMFAsyncCallback>();
            if let Err(err) = unsafe { source.BeginGetEvent(&*callback, None) }
                && err.code() != MF_E_SHUTDOWN
            {
                let mut state = self.queue.state.lock().unwrap();
                if !state.stopped && state.error.is_none() {
                    state.error = Some(err.code());
                    self.queue.wake.signal();
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct KeyframeTrigger {
    request: Arc<AtomicBool>,
    wake: Arc<OwnedHandle>,
}

impl KeyframeTrigger {
    pub fn request(&self) {
        self.request.store(true, Ordering::Release);
        self.wake.signal();
    }
}

pub struct Encoder {
    request: Arc<AtomicBool>,
    wake: Arc<OwnedHandle>,
    pub units: mpsc::Receiver<Result<AccessUnit>>,
}

impl Encoder {
    pub fn start(format: Format, monitor: &xcap::Monitor) -> Result<Self> {
        ensure!(
            format.codec == Codec::H264,
            "The Windows hardware encoder requires H.264."
        );
        let center = (
            monitor.x()? + i32::try_from(monitor.width()? / 2)?,
            monitor.y()? + i32::try_from(monitor.height()? / 2)?,
        );
        let request = Arc::new(AtomicBool::new(true));
        let wake = Arc::new(OwnedHandle::new_event()?);
        let worker_request = request.clone();
        let worker_wake = wake.clone();
        let (output, units) = mpsc::channel(2);
        thread::Builder::new()
            .name("windows-screen-encoder".into())
            .spawn(move || {
                if let Err(error) = capture(format, center, &worker_request, &worker_wake, &output)
                {
                    let _ = output.blocking_send(Err(error));
                }
            })?;
        Ok(Self {
            request,
            wake,
            units,
        })
    }

    pub fn keyframe_trigger(&self) -> KeyframeTrigger {
        KeyframeTrigger {
            request: self.request.clone(),
            wake: self.wake.clone(),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.units.close();
        self.wake.signal();
    }
}

struct Nv12Slot {
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorOutputView,
}

#[derive(Clone)]
struct CursorBitmap {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

struct CursorOverlay {
    x: i32,
    y: i32,
    generation: u64,
    bitmap: Arc<CursorBitmap>,
}

struct CursorCache {
    generation: u64,
    width: u32,
    height: u32,
    texture: ID3D11Texture2D,
    view: ID3D11VideoProcessorInputView,
}

fn convert_pointer_shape(
    info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    raw: &[u8],
) -> Option<CursorBitmap> {
    let width = info.Width;
    if width == 0 || info.Height == 0 {
        return None;
    }
    let pitch = usize::try_from(info.Pitch).ok()?;
    let w = usize::try_from(width).ok()?;
    let kind = info.Type as i32;

    if kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 {
        let height = info.Height;
        let h = usize::try_from(height).ok()?;
        let row_bytes = w.checked_mul(4)?;
        if pitch < row_bytes || raw.len() < (h - 1).checked_mul(pitch)?.checked_add(row_bytes)? {
            return None;
        }
        let mut bgra = Vec::with_capacity(h * row_bytes);
        for y in 0..h {
            let start = y * pitch;
            bgra.extend_from_slice(&raw[start..start + row_bytes]);
        }
        return Some(CursorBitmap {
            width,
            height,
            bgra,
        });
    }

    if kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 {
        let height = info.Height / 2;
        if height == 0 {
            return None;
        }
        let h = usize::try_from(height).ok()?;
        let total_h = usize::try_from(info.Height).ok()?;
        if raw.len() < total_h.checked_mul(pitch)? || pitch * 8 < w {
            return None;
        }
        let mut bgra = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let and_row = &raw[y * pitch..(y + 1) * pitch];
            let xor_row = &raw[(y + h) * pitch..(y + h + 1) * pitch];
            for x in 0..w {
                let mask = 0x80u8 >> (x & 7);
                let and_bit = (and_row[x / 8] & mask) != 0;
                let xor_bit = (xor_row[x / 8] & mask) != 0;
                let pixel = match (and_bit, xor_bit) {
                    (false, false) => [0, 0, 0, 255],
                    (false, true) => [255, 255, 255, 255],
                    (true, false) => [0, 0, 0, 0],
                    (true, true) => [0, 0, 0, 255],
                };
                bgra.extend_from_slice(&pixel);
            }
        }
        return Some(CursorBitmap {
            width,
            height,
            bgra,
        });
    }

    if kind == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 {
        let height = info.Height;
        let h = usize::try_from(height).ok()?;
        let row_bytes = w.checked_mul(4)?;
        if pitch < row_bytes || raw.len() < (h - 1).checked_mul(pitch)?.checked_add(row_bytes)? {
            return None;
        }
        let mut bgra = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let row = &raw[y * pitch..y * pitch + row_bytes];
            for px in row.chunks_exact(4) {
                let (b, g, r, mask) = (px[0], px[1], px[2], px[3]);
                if mask == 0 || (b | g | r) != 0 {
                    bgra.extend_from_slice(&[b, g, r, 255]);
                } else {
                    bgra.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        return Some(CursorBitmap {
            width,
            height,
            bgra,
        });
    }

    None
}

fn create_bgra_texture(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width.max(1),
        Height: height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        ..Default::default()
    };
    let mut texture = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
    texture.context("Could not allocate BGRA texture.")
}

struct SharedSlot {
    capture_tex: ID3D11Texture2D,
    capture_mutex: IDXGIKeyedMutex,
    gpu_tex: ID3D11Texture2D,
    gpu_mutex: IDXGIKeyedMutex,
}

fn create_shared_bgra_pair(
    capture_device: &ID3D11Device,
    encode_device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<SharedSlot> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width.max(1),
        Height: height.max(1),
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        MiscFlags: D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 as u32,
        ..Default::default()
    };
    let mut cap_tex = None;
    unsafe { capture_device.CreateTexture2D(&desc, None, Some(&mut cap_tex)) }?;
    let cap_tex: ID3D11Texture2D = cap_tex.context("Could not allocate shared BGRA texture.")?;
    let cap_mutex: IDXGIKeyedMutex = cap_tex.cast()?;
    let dxgi_res: IDXGIResource = cap_tex.cast()?;
    let handle = unsafe { dxgi_res.GetSharedHandle() }?;
    let mut enc_tex: Option<ID3D11Texture2D> = None;
    unsafe { encode_device.OpenSharedResource(handle, &mut enc_tex) }?;
    let enc_tex = enc_tex.context("Could not open shared BGRA texture on encode device.")?;
    let enc_mutex: IDXGIKeyedMutex = enc_tex.cast()?;
    Ok(SharedSlot {
        capture_tex: cap_tex,
        capture_mutex: cap_mutex,
        gpu_tex: enc_tex,
        gpu_mutex: enc_mutex,
    })
}

fn create_shared_bgra_ring(
    capture_device: &ID3D11Device,
    encode_device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<[SharedSlot; 3]> {
    Ok([
        create_shared_bgra_pair(capture_device, encode_device, width, height)?,
        create_shared_bgra_pair(capture_device, encode_device, width, height)?,
        create_shared_bgra_pair(capture_device, encode_device, width, height)?,
    ])
}

struct DdaState {
    slots: [SharedSlot; 3],
    slot_keys: [u64; 3],
    ring_gen: u64,
    slot_size: (u32, u32),
    latest_idx: usize,
    reading_idx: Option<usize>,
    has_desktop: bool,
    frame_seq: u64,
    present_100ns: i64,
    arrived_100ns: i64,
    cursor_visible: bool,
    cursor_pos: (i32, i32),
    cursor_gen: u64,
    cursor_bitmap: Option<Arc<CursorBitmap>>,
    error: Option<anyhow::Error>,
}

struct DdaSlotLease {
    state: Arc<Mutex<DdaState>>,
    gpu_mutex: IDXGIKeyedMutex,
    idx: usize,
    next_key: u64,
    ring_gen: u64,
}

impl Drop for DdaSlotLease {
    fn drop(&mut self) {
        let _ = unsafe { self.gpu_mutex.ReleaseSync(self.next_key) };
        let mut guard = self.state.lock().unwrap();
        if guard.ring_gen == self.ring_gen {
            guard.slot_keys[self.idx] = self.next_key;
            guard.reading_idx = None;
        }
    }
}

struct WgcFrameLease(Option<Direct3D11CaptureFrame>);

impl Drop for WgcFrameLease {
    fn drop(&mut self) {
        if let Some(frame) = self.0.take() {
            let _ = frame.Close();
        }
    }
}

#[allow(dead_code)]
enum InputLease {
    Dda(DdaSlotLease),
    Wgc(WgcFrameLease),
}

struct AcquiredInput {
    texture: ID3D11Texture2D,
    source_width: i32,
    source_height: i32,
    capture_stamp: i64,
    present_100ns: i64,
    arrived_100ns: i64,
    cursor: Option<CursorOverlay>,
    _lease: InputLease,
}

struct DdaCapture {
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<DdaState>>,
    worker: Option<JoinHandle<()>>,
    last_seq: u64,
}

impl DdaCapture {
    fn start(
        capture_device: ID3D11Device,
        capture_context: ID3D11DeviceContext,
        encode_device: ID3D11Device,
        output1: IDXGIOutput1,
        duplication: IDXGIOutputDuplication,
        desktop_size: (u32, u32),
        wake: Arc<OwnedHandle>,
    ) -> Result<Self> {
        let slots = create_shared_bgra_ring(
            &capture_device,
            &encode_device,
            desktop_size.0,
            desktop_size.1,
        )?;
        let state = Arc::new(Mutex::new(DdaState {
            slots,
            slot_keys: [0, 0, 0],
            ring_gen: 0,
            slot_size: desktop_size,
            latest_idx: 0,
            reading_idx: None,
            has_desktop: false,
            frame_seq: 0,
            present_100ns: 0,
            arrived_100ns: 0,
            cursor_visible: false,
            cursor_pos: (0, 0),
            cursor_gen: 0,
            cursor_bitmap: None,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_state = state.clone();
        let worker = thread::Builder::new()
            .name("dda-capture".into())
            .spawn(move || {
                dda_worker(
                    capture_device,
                    capture_context,
                    encode_device,
                    output1,
                    duplication,
                    worker_state,
                    worker_stop,
                    wake,
                );
            })?;
        Ok(Self {
            stop,
            state,
            worker: Some(worker),
            last_seq: 0,
        })
    }

    fn acquire(&mut self) -> Result<Option<AcquiredInput>> {
        let mut state = self.state.lock().unwrap();
        if let Some(error) = state.error.take() {
            return Err(error);
        }
        if !state.has_desktop || state.frame_seq == self.last_seq {
            return Ok(None);
        }
        let idx = state.latest_idx;
        state.reading_idx = Some(idx);
        let key = state.slot_keys[idx];
        let ring_gen = state.ring_gen;
        let texture = state.slots[idx].gpu_tex.clone();
        let gpu_mutex = state.slots[idx].gpu_mutex.clone();
        let (w, h) = state.slot_size;
        let present_100ns = state.present_100ns;
        let arrived_100ns = state.arrived_100ns;
        self.last_seq = state.frame_seq;
        let cursor = if state.cursor_visible
            && let Some(bitmap) = &state.cursor_bitmap
        {
            Some(CursorOverlay {
                x: state.cursor_pos.0,
                y: state.cursor_pos.1,
                generation: state.cursor_gen,
                bitmap: bitmap.clone(),
            })
        } else {
            None
        };
        drop(state);
        if let Err(err) = unsafe { gpu_mutex.AcquireSync(key, u32::MAX) } {
            let mut state = self.state.lock().unwrap();
            if state.ring_gen == ring_gen {
                state.reading_idx = None;
            }
            return Err(err.into());
        }
        Ok(Some(AcquiredInput {
            texture,
            source_width: w as i32,
            source_height: h as i32,
            capture_stamp: arrived_100ns.max(1),
            present_100ns,
            arrived_100ns,
            cursor,
            _lease: InputLease::Dda(DdaSlotLease {
                state: self.state.clone(),
                gpu_mutex,
                idx,
                next_key: key + 1,
                ring_gen,
            }),
        }))
    }
}

impl Drop for DdaCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct ComThreadGuard(bool);

impl ComThreadGuard {
    fn new() -> Self {
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

fn dda_worker(
    capture_device: ID3D11Device,
    capture_context: ID3D11DeviceContext,
    encode_device: ID3D11Device,
    output1: IDXGIOutput1,
    mut duplication: IDXGIOutputDuplication,
    state: Arc<Mutex<DdaState>>,
    stop: Arc<AtomicBool>,
    wake: Arc<OwnedHandle>,
) {
    let _com = ComThreadGuard::new();
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    let mut ptr_buf: Vec<u8> = Vec::new();
    let mut has_desktop_yet = false;

    while !stop.load(Ordering::Acquire) {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        let acquired = unsafe { duplication.AcquireNextFrame(100, &mut info, &mut resource) };
        match acquired {
            Ok(()) => {
                let arrived_100ns = query_qpc_100ns();
                let step = (|| -> Result<bool> {
                    let mut updated = false;
                    if (info.AccumulatedFrames > 0 || !has_desktop_yet)
                        && let Some(resource) = resource
                    {
                        let tex: ID3D11Texture2D = resource.cast()?;
                        let mut desc = D3D11_TEXTURE2D_DESC::default();
                        unsafe { tex.GetDesc(&mut desc) };
                        let (write_idx, target_tex, cap_mutex, key, ring_gen) = {
                            let mut guard = state.lock().unwrap();
                            if guard.slot_size != (desc.Width, desc.Height) {
                                guard.slots = create_shared_bgra_ring(
                                    &capture_device,
                                    &encode_device,
                                    desc.Width,
                                    desc.Height,
                                )?;
                                guard.slot_keys = [0, 0, 0];
                                guard.reading_idx = None;
                                guard.ring_gen = guard.ring_gen.wrapping_add(1);
                                guard.slot_size = (desc.Width, desc.Height);
                            }
                            let idx = (0..3)
                                .find(|&i| i != guard.latest_idx && Some(i) != guard.reading_idx)
                                .unwrap_or(0);
                            (
                                idx,
                                guard.slots[idx].capture_tex.clone(),
                                guard.slots[idx].capture_mutex.clone(),
                                guard.slot_keys[idx],
                                guard.ring_gen,
                            )
                        };
                        unsafe {
                            cap_mutex.AcquireSync(key, u32::MAX)?;
                            capture_context.CopyResource(&target_tex, &tex);
                            cap_mutex.ReleaseSync(key + 1)?;
                        }
                        let mut guard = state.lock().unwrap();
                        if guard.ring_gen == ring_gen {
                            guard.slot_keys[write_idx] = key + 1;
                            guard.latest_idx = write_idx;
                        }
                        guard.has_desktop = true;
                        has_desktop_yet = true;
                        updated = true;
                    }

                    if info.LastMouseUpdateTime != 0 {
                        let mut new_cursor = None;
                        if info.PointerShapeBufferSize > 0 {
                            let cap = info.PointerShapeBufferSize as usize;
                            if ptr_buf.len() < cap {
                                ptr_buf.resize(cap, 0);
                            }
                            let mut required = 0u32;
                            let mut shape_info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                            unsafe {
                                duplication.GetFramePointerShape(
                                    info.PointerShapeBufferSize,
                                    ptr_buf.as_mut_ptr().cast(),
                                    &mut required,
                                    &mut shape_info,
                                )
                            }?;
                            if let Some(bitmap) =
                                convert_pointer_shape(&shape_info, &ptr_buf[..required as usize])
                            {
                                new_cursor = Some(Arc::new(bitmap));
                            }
                        }
                        let next_visible = info.PointerPosition.Visible.as_bool();
                        let next_pos =
                            (info.PointerPosition.Position.x, info.PointerPosition.Position.y);
                        let mut guard = state.lock().unwrap();
                        let cursor_changed = guard.cursor_visible != next_visible
                            || (next_visible && guard.cursor_pos != next_pos)
                            || new_cursor.is_some();
                        guard.cursor_visible = next_visible;
                        guard.cursor_pos = next_pos;
                        if let Some(bitmap) = new_cursor {
                            guard.cursor_bitmap = Some(bitmap);
                            guard.cursor_gen = guard.cursor_gen.wrapping_add(1);
                        }
                        if cursor_changed {
                            updated = true;
                        }
                    }

                    if updated && has_desktop_yet {
                        let present_100ns = if info.AccumulatedFrames > 0 && info.LastPresentTime > 0 {
                            qpc_to_100ns(info.LastPresentTime)
                        } else if info.LastMouseUpdateTime > 0 {
                            qpc_to_100ns(info.LastMouseUpdateTime)
                        } else {
                            arrived_100ns
                        };
                        let mut guard = state.lock().unwrap();
                        guard.present_100ns = present_100ns;
                        guard.arrived_100ns = arrived_100ns;
                        guard.frame_seq = guard.frame_seq.wrapping_add(1);
                        return Ok(true);
                    }
                    Ok(false)
                })();
                let _ = unsafe { duplication.ReleaseFrame() };
                match step {
                    Ok(true) => wake.signal(),
                    Ok(false) => {}
                    Err(err) => {
                        state.lock().unwrap().error = Some(err);
                        wake.signal();
                        break;
                    }
                }
            }
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
            Err(err)
                if err.code() == DXGI_ERROR_ACCESS_LOST
                    || err.code() == DXGI_ERROR_INVALID_CALL =>
            {
                drop(duplication);
                let mut rebuilt = None;
                for _ in 0..100 {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(20));
                    if let Ok(next) = unsafe { output1.DuplicateOutput(&capture_device) } {
                        rebuilt = Some(next);
                        break;
                    }
                }
                match rebuilt {
                    Some(next) => {
                        duplication = next;
                        has_desktop_yet = false;
                    }
                    None => {
                        state.lock().unwrap().error = Some(err.into());
                        wake.signal();
                        break;
                    }
                }
            }
            Err(err) => {
                state.lock().unwrap().error = Some(err.into());
                wake.signal();
                break;
            }
        }
    }
}

struct WgcCapture {
    pending: Option<Direct3D11CaptureFrame>,
    pending_arrived_100ns: i64,
    session: Option<GraphicsCaptureSession>,
    pool: Option<Direct3D11CaptureFramePool>,
    frame_arrived_token: Option<i64>,
    capture_device: IDirect3DDevice,
    capture_size: SizeInt32,
    last_capture: i64,
}

impl WgcCapture {
    fn start(monitor: HMONITOR, gpu: &VideoDevice, wake: &Arc<OwnedHandle>) -> Result<Self> {
        let interop: IGraphicsCaptureItemInterop = factory::<GraphicsCaptureItem, _>()
            .context("Could not get GraphicsCaptureItem interop factory.")?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }
            .context("Could not create GraphicsCaptureItem for monitor.")?;

        let dxgi: IDXGIDevice = gpu.device.cast()?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .context("Could not wrap D3D11 device for Windows Graphics Capture.")?;
        let capture_device: IDirect3DDevice = inspectable.cast()?;
        let capture_size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &capture_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            capture_size,
        )
        .context("Could not create WGC frame pool.")?;

        let wake_for_wgc = wake.clone();
        let token = pool.FrameArrived(&TypedEventHandler::<
            Direct3D11CaptureFramePool,
            IInspectable,
        >::new(move |_, _| {
            wake_for_wgc.signal();
            Ok(())
        }))?;
        let session = pool.CreateCaptureSession(&item)?;
        session.StartCapture()?;

        Ok(Self {
            pending: None,
            pending_arrived_100ns: 0,
            session: Some(session),
            pool: Some(pool),
            frame_arrived_token: Some(token),
            capture_device,
            capture_size,
            last_capture: 0,
        })
    }

    fn drain_pool(&mut self) -> Result<()> {
        let Some(pool) = self.pool.as_ref() else {
            return Ok(());
        };
        while let Ok(next) = pool.TryGetNextFrame() {
            let size = next.ContentSize()?;
            if size.Width != self.capture_size.Width || size.Height != self.capture_size.Height {
                let _ = next.Close();
                if let Some(old) = self.pending.take() {
                    let _ = old.Close();
                }
                self.capture_size = size;
                pool.Recreate(
                    &self.capture_device,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    self.capture_size,
                )?;
                return Ok(());
            }
            self.pending_arrived_100ns = query_qpc_100ns();
            if let Some(old) = self.pending.replace(next) {
                let _ = old.Close();
            }
        }
        Ok(())
    }

    fn acquire(&mut self, has_encoded: bool) -> Result<Option<AcquiredInput>> {
        self.drain_pool()?;
        let Some(frame) = self.pending.as_ref() else {
            return Ok(None);
        };
        let captured = frame.SystemRelativeTime()?.Duration;
        if has_encoded && captured <= self.last_capture {
            if let Some(old) = self.pending.take() {
                let _ = old.Close();
            }
            return Ok(None);
        }
        let frame = self.pending.take().unwrap();
        self.last_capture = captured;
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
        Ok(Some(AcquiredInput {
            texture,
            source_width: self.capture_size.Width,
            source_height: self.capture_size.Height,
            capture_stamp: captured,
            present_100ns: captured,
            arrived_100ns: self.pending_arrived_100ns,
            cursor: None,
            _lease: InputLease::Wgc(WgcFrameLease(Some(frame))),
        }))
    }
}

impl Drop for WgcCapture {
    fn drop(&mut self) {
        if let (Some(pool), Some(token)) = (&self.pool, self.frame_arrived_token.take()) {
            let _ = pool.RemoveFrameArrived(token);
        }
        if let Some(session) = self.session.take() {
            let _ = session.Close();
        }
        if let Some(pool) = self.pool.take() {
            let _ = pool.Close();
        }
        if let Some(pending) = self.pending.take() {
            let _ = pending.Close();
        }
    }
}

enum CaptureBackend {
    Dda(DdaCapture),
    Wgc(WgcCapture),
}

impl CaptureBackend {
    fn name(&self) -> &'static str {
        match self {
            Self::Dda(_) => "dda",
            Self::Wgc(_) => "wgc",
        }
    }

    fn drain(&mut self) -> Result<()> {
        if let Self::Wgc(wgc) = self {
            wgc.drain_pool()?;
        }
        Ok(())
    }

    fn acquire(&mut self, has_encoded: bool) -> Result<Option<AcquiredInput>> {
        match self {
            Self::Dda(dda) => dda.acquire(),
            Self::Wgc(wgc) => wgc.acquire(has_encoded),
        }
    }
}

#[derive(Default)]
struct StageStats {
    cap_age_sum_ms: f64,
    queue_sum_ms: f64,
    conv_sum_ms: f64,
    enc_sum_ms: f64,
    total_sum_ms: f64,
    total_max_ms: f64,
    count: u32,
}

struct NativeEncoder {
    // Field order matters: capture/COM/MFT objects drop BEFORE `gpu` (which runs MFShutdown/CoUninitialize).
    capture: CaptureBackend,
    cursor_cache: Option<CursorCache>,
    nv12_pool: Vec<Nv12Slot>,
    nv12_index: usize,
    last_nv12_idx: Option<usize>,
    processor: Option<ID3D11VideoProcessor>,
    enumerator: Option<ID3D11VideoProcessorEnumerator>,
    max_input_streams: u32,
    _event_relay: IMFAsyncCallback,
    event_queue: Arc<EventQueue>,
    codec: ICodecAPI,
    _events: IMFMediaEventGenerator,
    transform: IMFTransform,
    gpu: VideoDevice,
    wake: Arc<OwnedHandle>,
    pace_timer: Option<OwnedHandle>,
    _timer_resolution: TimerResolutionGuard,
    input_id: u32,
    output_id: u32,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    duration: i64,
    first_capture: i64,
    last_time: i64,
    submit_at: Instant,
    last_submit: Instant,
    pending_cap_age_ms: f64,
    pending_queue_ms: f64,
    pending_conv_ms: f64,
    pending_present_100ns: i64,
    stats: StageStats,
    log_enabled: bool,
    keyframe: bool,
    wants_input: bool,
    waiting_for_output: bool,
    has_encoded: bool,
}

impl Drop for NativeEncoder {
    fn drop(&mut self) {
        {
            let mut state = self.event_queue.state.lock().unwrap();
            state.stopped = true;
            state.pending.clear();
            let _detached = state.source.take();
        }
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
        self.cursor_cache = None;
        self.nv12_pool.clear();
        self.processor = None;
        self.enumerator = None;
    }
}

fn try_init_dda(
    monitor: HMONITOR,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    wake: &Arc<OwnedHandle>,
    log_enabled: bool,
) -> Result<(
    VideoDevice,
    IMFTransform,
    IMFMediaEventGenerator,
    ICodecAPI,
    u32,
    u32,
    DdaCapture,
)> {
    let (adapter, output1) = find_monitor_output(monitor)?;
    let gpu = VideoDevice::new_with_adapter(None, Some(&adapter))?;
    let (transform, events, codec, input_id, output_id) =
        create_transform(&gpu, width, height, fps, bitrate, log_enabled)?;

    // Use a dedicated D3D11 device for Desktop Duplication so AcquireNextFrame's
    // internal device lock never contends with VideoProcessorBlt or NVENC MFT.
    let dxgi_adapter: IDXGIAdapter = adapter.cast()?;
    let mut cap_device = None;
    let mut cap_context = None;
    unsafe {
        D3D11CreateDevice(
            Some(&dxgi_adapter),
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut cap_device),
            None,
            Some(&mut cap_context),
        )
    }
    .context("Could not create dedicated D3D11 device for Desktop Duplication.")?;
    let cap_device = cap_device.context("Missing capture D3D11 device.")?;
    let cap_context = cap_context.context("Missing capture D3D11 context.")?;
    if let Ok(dxgi) = cap_device.cast::<IDXGIDevice>() {
        let _ = unsafe { dxgi.SetGPUThreadPriority(7) };
    }

    let duplication = unsafe { output1.DuplicateOutput(&cap_device) }
        .context("Could not initialize DXGI Desktop Duplication.")?;
    let desc = unsafe { output1.GetDesc() }?;
    let dw = u32::try_from(desc.DesktopCoordinates.right - desc.DesktopCoordinates.left)?;
    let dh = u32::try_from(desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top)?;
    ensure!(dw > 0 && dh > 0, "Invalid DXGI output dimensions.");
    let dda = DdaCapture::start(
        cap_device,
        cap_context,
        gpu.device.clone(),
        output1,
        duplication,
        (dw, dh),
        wake.clone(),
    )?;
    Ok((gpu, transform, events, codec, input_id, output_id, dda))
}

impl NativeEncoder {
    fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        center: (i32, i32),
        wake_handle: &OwnedHandle,
    ) -> Result<Self> {
        let timer_resolution = TimerResolutionGuard::new();
        let wake = Arc::new(wake_handle.duplicate()?);
        let pace_timer = OwnedHandle::new_high_res_timer();
        let log_enabled = crate::metrics::enabled();

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

        let force_wgc = std::env::var("REMOTERS_WIN_CAPTURE")
            .ok()
            .is_some_and(|v| v.eq_ignore_ascii_case("wgc"));

        let (gpu, transform, events, codec, input_id, output_id, capture) = if !force_wgc {
            match try_init_dda(monitor, width, height, fps, bitrate, &wake, log_enabled) {
                Ok((gpu, transform, events, codec, input_id, output_id, dda)) => (
                    gpu,
                    transform,
                    events,
                    codec,
                    input_id,
                    output_id,
                    CaptureBackend::Dda(dda),
                ),
                Err(err) => {
                    if log_enabled {
                        eprintln!("[remoters:win] dda_unavailable={err:#}; falling back to wgc");
                    }
                    let gpu = VideoDevice::new(None)?;
                    let (transform, events, codec, input_id, output_id) =
                        create_transform(&gpu, width, height, fps, bitrate, log_enabled)?;
                    let wgc = WgcCapture::start(monitor, &gpu, &wake)?;
                    (
                        gpu,
                        transform,
                        events,
                        codec,
                        input_id,
                        output_id,
                        CaptureBackend::Wgc(wgc),
                    )
                }
            }
        } else {
            let gpu = VideoDevice::new(None)?;
            let (transform, events, codec, input_id, output_id) =
                create_transform(&gpu, width, height, fps, bitrate, log_enabled)?;
            let wgc = WgcCapture::start(monitor, &gpu, &wake)?;
            (
                gpu,
                transform,
                events,
                codec,
                input_id,
                output_id,
                CaptureBackend::Wgc(wgc),
            )
        };

        if log_enabled {
            eprintln!("[remoters:win] capture_backend={}", capture.name());
        }

        let event_queue = Arc::new(EventQueue {
            state: Mutex::new(EventQueueState {
                pending: VecDeque::new(),
                source: Some(events.clone()),
                error: None,
                stopped: false,
            }),
            wake: wake.clone(),
        });
        let event_relay: IMFAsyncCallback = EventRelay {
            queue: event_queue.clone(),
        }
        .into();
        unsafe { events.BeginGetEvent(&event_relay, None) }
            .context("Could not start async Media Foundation event pump.")?;

        let now = Instant::now();
        Ok(Self {
            capture,
            cursor_cache: None,
            nv12_pool: Vec::new(),
            nv12_index: 0,
            last_nv12_idx: None,
            processor: None,
            enumerator: None,
            max_input_streams: 1,
            _event_relay: event_relay,
            event_queue,
            codec,
            _events: events,
            transform,
            gpu,
            wake,
            pace_timer,
            _timer_resolution: timer_resolution,
            input_id,
            output_id,
            input_width: 0,
            input_height: 0,
            output_width: width,
            output_height: height,
            duration: 10_000_000 / i64::from(fps),
            first_capture: 0,
            last_time: -1,
            submit_at: now,
            last_submit: now,
            pending_cap_age_ms: 0.0,
            pending_queue_ms: 0.0,
            pending_conv_ms: 0.0,
            pending_present_100ns: 0,
            stats: StageStats::default(),
            log_enabled,
            keyframe: true,
            wants_input: false,
            waiting_for_output: false,
            has_encoded: false,
        })
    }

    fn has_pending_events(&self) -> bool {
        let state = self.event_queue.state.lock().unwrap();
        !state.pending.is_empty() || state.error.is_some()
    }

    fn poll(&mut self, force: bool, wait_ms: u32, state: &mut EncodedOutput<'_>) -> Result<()> {
        self.keyframe |= force;
        let mut produced = false;
        self.update_events(state, &mut produced)?;
        let mut pace_remaining_100ns = None;
        self.try_submit(&mut pace_remaining_100ns)?;
        if produced {
            return Ok(());
        }

        if wait_ms > 0 && !self.has_pending_events() {
            if let (Some(remaining_100ns), Some(timer)) = (pace_remaining_100ns, &self.pace_timer) {
                let due = -remaining_100ns.max(1);
                unsafe {
                    let _ = SetWaitableTimer(timer.0, &due, 0, None, None, false);
                    let handles = [self.wake.0, timer.0];
                    let _ = WaitForMultipleObjects(&handles, false, wait_ms);
                    let _ = CancelWaitableTimer(timer.0);
                }
            } else {
                unsafe {
                    let _ = WaitForSingleObject(self.wake.0, wait_ms);
                }
            }
            self.update_events(state, &mut produced)?;
            let mut ignored = None;
            self.try_submit(&mut ignored)?;
        }
        Ok(())
    }

    fn update_events(&mut self, out: &mut EncodedOutput<'_>, produced: &mut bool) -> Result<()> {
        let (batch, async_error) = {
            let mut state = self.event_queue.state.lock().unwrap();
            (std::mem::take(&mut state.pending), state.error)
        };
        if let Some(hr) = async_error {
            return Err(windows_core::Error::from(hr).into());
        }
        for event in batch {
            let status = unsafe { event.GetStatus() }?;
            status.ok()?;
            let kind = unsafe { event.GetType() }?;
            if kind == METransformNeedInput.0 as u32 {
                self.wants_input = true;
            }
            if kind == METransformHaveOutput.0 as u32 {
                self.receive(out)?;
                self.waiting_for_output = false;
                *produced = true;
            }
        }
        Ok(())
    }

    fn try_submit(&mut self, pace_remaining_100ns: &mut Option<i64>) -> Result<()> {
        *pace_remaining_100ns = None;
        self.capture.drain()?;
        if !self.wants_input || self.waiting_for_output {
            return Ok(());
        }

        let (nv12, time, cap_age_ms, queue_ms, conv_ms, present_100ns) =
            if let Some(acquired) = self.capture.acquire(self.has_encoded)? {
                let submit_start_100ns = query_qpc_100ns();
                let cap_age_ms =
                    ((acquired.arrived_100ns - acquired.present_100ns) as f64 / 10_000.0).max(0.0);
                let queue_ms =
                    ((submit_start_100ns - acquired.arrived_100ns) as f64 / 10_000.0).max(0.0);
                if self.first_capture == 0 {
                    self.first_capture = acquired.capture_stamp;
                }
                let mut time = acquired.capture_stamp - self.first_capture;
                if self.last_time >= time {
                    time = self.last_time + self.duration;
                }
                let conv_start = Instant::now();
                let idx = self.convert(
                    &acquired.texture,
                    acquired.source_width,
                    acquired.source_height,
                    acquired.cursor.as_ref(),
                )?;
                let conv_ms = conv_start.elapsed().as_secs_f64() * 1000.0;
                let present_100ns = acquired.present_100ns;
                drop(acquired);
                self.last_nv12_idx = Some(idx);
                (
                    self.nv12_pool[idx].texture.clone(),
                    time,
                    cap_age_ms,
                    queue_ms,
                    conv_ms,
                    present_100ns,
                )
            } else if self.keyframe && self.has_encoded {
                let Some(idx) = self.last_nv12_idx else {
                    return Ok(());
                };
                let elapsed_100ns =
                    (Instant::now().saturating_duration_since(self.last_submit).as_nanos() / 100)
                        as i64;
                if elapsed_100ns < self.duration {
                    *pace_remaining_100ns = Some(self.duration - elapsed_100ns);
                    return Ok(());
                }
                let time = self.last_time + self.duration;
                let now_100ns = query_qpc_100ns();
                (
                    self.nv12_pool[idx].texture.clone(),
                    time,
                    0.0,
                    0.0,
                    0.0,
                    now_100ns,
                )
            } else {
                return Ok(());
            };

        let buffer = unsafe {
            MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &nv12, 0, false)
        }?;
        let texture_buffer: IMF2DBuffer = buffer.cast()?;
        let length = unsafe { texture_buffer.GetContiguousLength() }?;
        unsafe { buffer.SetCurrentLength(length) }?;
        let sample = unsafe { MFCreateSample() }?;
        unsafe {
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(self.duration)?;
        }
        if self.keyframe {
            set_codec_u32(&self.codec, &CODECAPI_AVEncVideoForceKeyFrame, 1)?;
        }
        self.pending_cap_age_ms = cap_age_ms;
        self.pending_queue_ms = queue_ms;
        self.pending_conv_ms = conv_ms;
        self.pending_present_100ns = present_100ns;
        self.submit_at = Instant::now();
        unsafe { self.transform.ProcessInput(self.input_id, &sample, 0) }?;
        self.keyframe = false;
        self.wants_input = false;
        self.waiting_for_output = true;
        self.has_encoded = true;
        self.last_time = time;
        self.last_submit = self.submit_at;
        Ok(())
    }

    fn receive(&mut self, out: &mut EncodedOutput<'_>) -> Result<()> {
        let info: MFT_OUTPUT_STREAM_INFO =
            unsafe { self.transform.GetOutputStreamInfo(self.output_id) }?;
        let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
            | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        let mut output = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: self.output_id,
            ..Default::default()
        }];
        if (info.dwFlags & provides) == 0 {
            let align = info.cbAlignment.saturating_sub(1);
            let buffer = unsafe { MFCreateAlignedMemoryBuffer(info.cbSize, align) }?;
            let storage = unsafe { MFCreateSample() }?;
            unsafe { storage.AddBuffer(&buffer) }?;
            output[0].pSample = ManuallyDrop::new(Some(storage));
        }
        let mut flags = 0u32;
        let result = unsafe { self.transform.ProcessOutput(0, &mut output, &mut flags) };
        let storage = unsafe { ManuallyDrop::take(&mut output[0].pSample) };
        let _events = unsafe { ManuallyDrop::take(&mut output[0].pEvents) };
        result?;
        let Some(storage) = storage else {
            return Ok(());
        };

        let encode_elapsed = self.submit_at.elapsed();
        let encode_us = encode_elapsed.as_micros().min(u128::from(u32::MAX)) as u32;
        let time = unsafe { storage.GetSampleTime() }?;
        if self.log_enabled {
            if time != self.last_time {
                eprintln!(
                    "[remoters:win] MFT_REORDER_LAG out_time={time} last_in_time={} diff_ms={:.2}",
                    self.last_time,
                    (self.last_time - time) as f64 / 10_000.0
                );
            }
            let enc_ms = encode_elapsed.as_secs_f64() * 1000.0;
            let total_ms = ((query_qpc_100ns() - self.pending_present_100ns) as f64 / 10_000.0)
                .max(0.0);
            self.stats.cap_age_sum_ms += self.pending_cap_age_ms;
            self.stats.queue_sum_ms += self.pending_queue_ms;
            self.stats.conv_sum_ms += self.pending_conv_ms;
            self.stats.enc_sum_ms += enc_ms;
            self.stats.total_sum_ms += total_ms;
            self.stats.total_max_ms = self.stats.total_max_ms.max(total_ms);
            self.stats.count += 1;
            if self.stats.count >= 60 {
                let n = f64::from(self.stats.count);
                eprintln!(
                    "[remoters:win] backend={} cap_age={:.2}ms queue={:.2}ms conv={:.2}ms enc={:.2}ms total={:.2}ms max={:.2}ms (n={})",
                    self.capture.name(),
                    self.stats.cap_age_sum_ms / n,
                    self.stats.queue_sum_ms / n,
                    self.stats.conv_sum_ms / n,
                    self.stats.enc_sum_ms / n,
                    self.stats.total_sum_ms / n,
                    self.stats.total_max_ms,
                    self.stats.count,
                );
                self.stats = StageStats::default();
            }
        }

        let buffer: IMFMediaBuffer = unsafe { storage.ConvertToContiguousBuffer() }?;
        let key = unsafe { storage.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0);
        let mut header_bytes = Vec::new();
        if key != 0
            && let Ok(media) = unsafe { self.transform.GetOutputCurrentType(self.output_id) }
        {
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            if unsafe { media.GetAllocatedBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut ptr, &mut len) }
                .is_ok()
                && !ptr.is_null()
            {
                if (1..=4096).contains(&len) {
                    header_bytes.extend_from_slice(unsafe {
                        std::slice::from_raw_parts(ptr, len as usize)
                    });
                }
                unsafe { CoTaskMemFree(Some(ptr.cast())) };
            }
        }

        let mut bytes_ptr = std::ptr::null_mut();
        let mut length = 0u32;
        unsafe { buffer.Lock(&mut bytes_ptr, None, Some(&mut length)) }?;
        let slice = if !bytes_ptr.is_null() && length > 0 {
            unsafe { std::slice::from_raw_parts(bytes_ptr, length as usize) }
        } else {
            &[]
        };
        out.emit(slice, &header_bytes, time, encode_us);
        unsafe { buffer.Unlock() }?;
        Ok(())
    }

    fn ensure_nv12_pool(&mut self) -> Result<()> {
        if !self.nv12_pool.is_empty() {
            return Ok(());
        }
        let enumerator = self.enumerator.as_ref().unwrap();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.output_width,
            Height: self.output_height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let target = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        for _ in 0..4 {
            let mut texture = None;
            unsafe { self.gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
            let texture = texture.context("Could not allocate NV12 texture.")?;
            let mut view = None;
            unsafe {
                self.gpu.video.CreateVideoProcessorOutputView(
                    &texture,
                    enumerator,
                    &target,
                    Some(&mut view),
                )
            }?;
            let view = view.context("Could not create NV12 output view.")?;
            self.nv12_pool.push(Nv12Slot { texture, view });
        }
        Ok(())
    }

    fn ensure_cursor_view(
        &mut self,
        cursor: &CursorOverlay,
    ) -> Result<(ID3D11VideoProcessorInputView, u32, u32)> {
        let enumerator = self.enumerator.as_ref().unwrap();
        let bw = cursor.bitmap.width;
        let bh = cursor.bitmap.height;
        let needs_alloc = match &self.cursor_cache {
            Some(cache) => cache.width != bw || cache.height != bh,
            None => true,
        };
        if needs_alloc {
            let texture = create_bgra_texture(&self.gpu.device, bw, bh)?;
            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                ..Default::default()
            };
            let mut view = None;
            unsafe {
                self.gpu.video.CreateVideoProcessorInputView(
                    &texture,
                    enumerator,
                    &in_desc,
                    Some(&mut view),
                )
            }?;
            let view = view.context("Could not create cursor input view.")?;
            unsafe {
                self.gpu.context.UpdateSubresource(
                    &texture,
                    0,
                    None,
                    cursor.bitmap.bgra.as_ptr().cast(),
                    bw * 4,
                    0,
                );
            }
            self.cursor_cache = Some(CursorCache {
                generation: cursor.generation,
                width: bw,
                height: bh,
                texture,
                view,
            });
        } else if let Some(cache) = &mut self.cursor_cache
            && cache.generation != cursor.generation
        {
            unsafe {
                self.gpu.context.UpdateSubresource(
                    &cache.texture,
                    0,
                    None,
                    cursor.bitmap.bgra.as_ptr().cast(),
                    bw * 4,
                    0,
                );
            }
            cache.generation = cursor.generation;
        }
        let cache = self.cursor_cache.as_ref().unwrap();
        Ok((cache.view.clone(), cache.width, cache.height))
    }

    fn convert(
        &mut self,
        picture: &ID3D11Texture2D,
        source_width: i32,
        source_height: i32,
        cursor: Option<&CursorOverlay>,
    ) -> Result<usize> {
        let mut source = D3D11_TEXTURE2D_DESC::default();
        unsafe { picture.GetDesc(&mut source) };
        if self.processor.is_none()
            || self.input_width != source.Width
            || self.input_height != source.Height
        {
            self.cursor_cache = None;
            self.nv12_pool.clear();
            self.last_nv12_idx = None;
            self.enumerator = None;
            self.processor = None;
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputWidth: source.Width,
                InputHeight: source.Height,
                OutputWidth: self.output_width,
                OutputHeight: self.output_height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                ..Default::default()
            };
            let enumerator = unsafe { self.gpu.video.CreateVideoProcessorEnumerator(&content) }?;
            let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
            let max_streams = if unsafe { enumerator.GetVideoProcessorCaps(&mut caps) }.is_ok() {
                caps.MaxInputStreams
            } else {
                1
            };
            let processor = unsafe { self.gpu.video.CreateVideoProcessor(&enumerator, 0) }?;
            self.enumerator = Some(enumerator);
            self.processor = Some(processor);
            self.max_input_streams = max_streams;
            self.input_width = source.Width;
            self.input_height = source.Height;
        }
        self.ensure_nv12_pool()?;

        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let mut input_view = None;
        unsafe {
            self.gpu.video.CreateVideoProcessorInputView(
                picture,
                self.enumerator.as_ref().unwrap(),
                &in_desc,
                Some(&mut input_view),
            )
        }?;
        let input_view = input_view.context("Could not create BGRA input view.")?;

        let mut cursor_rects = None;
        if self.max_input_streams >= 2
            && source_width > 0
            && source_height > 0
            && let Some(cursor) = cursor
        {
            let bw = cursor.bitmap.width as i32;
            let bh = cursor.bitmap.height as i32;
            let left = cursor.x.max(0);
            let top = cursor.y.max(0);
            let right = (cursor.x + bw).min(source_width);
            let bottom = (cursor.y + bh).min(source_height);
            if left < right && top < bottom {
                let (cursor_view, _, _) = self.ensure_cursor_view(cursor)?;
                let src_rect = RECT {
                    left: left - cursor.x,
                    top: top - cursor.y,
                    right: right - cursor.x,
                    bottom: bottom - cursor.y,
                };
                let scale_x = f64::from(self.output_width) / f64::from(source_width);
                let scale_y = f64::from(self.output_height) / f64::from(source_height);
                let dst_left = (f64::from(left) * scale_x).round() as i32;
                let dst_top = (f64::from(top) * scale_y).round() as i32;
                let dst_right = ((f64::from(right) * scale_x).round() as i32)
                    .max(dst_left + 1)
                    .min(self.output_width as i32);
                let dst_bottom = ((f64::from(bottom) * scale_y).round() as i32)
                    .max(dst_top + 1)
                    .min(self.output_height as i32);
                let dst_rect = RECT {
                    left: dst_left,
                    top: dst_top,
                    right: dst_right,
                    bottom: dst_bottom,
                };
                cursor_rects = Some((cursor_view, src_rect, dst_rect));
            }
        }

        let processor = self.processor.as_ref().unwrap();
        let idx = self.nv12_index;
        self.nv12_index = (self.nv12_index + 1) % self.nv12_pool.len();
        let slot = &self.nv12_pool[idx];

        let source_rect = RECT {
            left: 0,
            top: 0,
            right: source_width,
            bottom: source_height,
        };
        let dest_rect = RECT {
            left: 0,
            top: 0,
            right: self.output_width as i32,
            bottom: self.output_height as i32,
        };
        let mut streams = [
            D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: TRUE,
                pInputSurface: ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            },
            D3D11_VIDEO_PROCESSOR_STREAM::default(),
        ];
        let stream_count = if let Some((cursor_view, c_src, c_dst)) = cursor_rects {
            streams[1] = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: TRUE,
                pInputSurface: ManuallyDrop::new(Some(cursor_view)),
                ..Default::default()
            };
            unsafe {
                self.gpu
                    .video_context
                    .VideoProcessorSetStreamFrameFormat(
                        processor,
                        1,
                        D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                    );
                self.gpu
                    .video_context
                    .VideoProcessorSetStreamSourceRect(processor, 1, true, Some(&c_src));
                self.gpu
                    .video_context
                    .VideoProcessorSetStreamDestRect(processor, 1, true, Some(&c_dst));
                self.gpu
                    .video_context
                    .VideoProcessorSetStreamAutoProcessingMode(processor, 1, false);
                self.gpu
                    .video_context
                    .VideoProcessorSetStreamAlpha(processor, 1, true, 1.0);
            }
            2
        } else {
            1
        };

        unsafe {
            self.gpu
                .video_context
                .VideoProcessorSetStreamSourceRect(processor, 0, true, Some(&source_rect));
            self.gpu
                .video_context
                .VideoProcessorSetStreamDestRect(processor, 0, true, Some(&dest_rect));
            self.gpu
                .video_context
                .VideoProcessorSetStreamAutoProcessingMode(processor, 0, false);
            self.gpu
                .video_context
                .VideoProcessorSetOutputColorSpace(processor, &BT709_FULL_RANGE);
            let blt = self.gpu.video_context.VideoProcessorBlt(
                processor,
                &slot.view,
                0,
                &streams[..stream_count],
            );
            let _ = ManuallyDrop::take(&mut streams[0].pInputSurface);
            if stream_count == 2 {
                let _ = ManuallyDrop::take(&mut streams[1].pInputSurface);
            }
            blt?;
            self.gpu.context.Flush();
        }
        Ok(idx)
    }
}

fn set_codec_u32(codec: &ICodecAPI, attribute: &GUID, value: u32) -> windows_core::Result<()> {
    let variant = VARIANT::from(value);
    unsafe { codec.SetValue(attribute, &variant) }
}

fn set_codec_bool(codec: &ICodecAPI, attribute: &GUID, value: bool) {
    let variant = VARIANT::from(value);
    if unsafe { codec.SetValue(attribute, &variant) }.is_err() {
        let _ = set_codec_u32(codec, attribute, u32::from(value));
    }
}

fn apply_low_latency_settings(codec: &ICodecAPI, fps: u32, bitrate: u32) {
    set_codec_bool(codec, &CODECAPI_AVLowLatencyMode, true);
    let _ = set_codec_u32(codec, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
    // Avoid periodic large IDR spikes on LAN; keyframes are requested on start and loss recovery.
    let _ = set_codec_u32(codec, &CODECAPI_AVEncMPVGOPSize, fps * 300);
    let _ = set_codec_u32(codec, &CODECAPI_AVEncCommonQualityVsSpeed, 0);
    // PeakConstrainedVBR keeps small desktop updates compact instead of inflating P-frames.
    if set_codec_u32(
        codec,
        &CODECAPI_AVEncCommonRateControlMode,
        eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32,
    )
    .is_err()
        && set_codec_u32(
            codec,
            &CODECAPI_AVEncCommonRateControlMode,
            eAVEncCommonRateControlMode_LowDelayVBR.0 as u32,
        )
        .is_err()
    {
        let _ = set_codec_u32(
            codec,
            &CODECAPI_AVEncCommonRateControlMode,
            eAVEncCommonRateControlMode_CBR.0 as u32,
        );
    }
    let _ = set_codec_u32(codec, &CODECAPI_AVEncCommonMeanBitRate, bitrate);
    let _ = set_codec_u32(codec, &CODECAPI_AVEncCommonMaxBitRate, bitrate + bitrate / 2);
    // 250 ms VBV buffer in bytes (bitrate is bits/sec).
    let _ = set_codec_u32(
        codec,
        &CODECAPI_AVEncCommonBufferSize,
        (bitrate / 32).max(256 * 1024),
    );
}

fn build_media_type(width: u32, height: u32, fps: u32, subtype: &GUID) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        media_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_0_255.0 as u32)?;
    }
    Ok(media_type)
}

fn configure_transform(
    gpu: &VideoDevice,
    activation: &IMFActivate,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
) -> Result<(IMFTransform, IMFMediaEventGenerator, ICodecAPI, u32, u32)> {
    let transform: IMFTransform = unsafe { activation.ActivateObject() }?;
    let events: IMFMediaEventGenerator = transform.cast()?;
    let codec: ICodecAPI = transform.cast()?;
    let attributes = unsafe { transform.GetAttributes() }?;
    unsafe {
        attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
        attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
        transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)?;
    }
    let mut input_ids = [0u32];
    let mut output_ids = [0u32];
    let (input_id, output_id) = match unsafe { transform.GetStreamIDs(&mut input_ids, &mut output_ids) } {
        Ok(()) => (input_ids[0], output_ids[0]),
        Err(err) if err.code() == E_NOTIMPL => (0, 0),
        Err(err) => return Err(err.into()),
    };

    apply_low_latency_settings(&codec, fps, bitrate);
    let output = build_media_type(width, height, fps, &MFVideoFormat_H264)?;
    unsafe {
        output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
        transform.SetOutputType(output_id, &output, 0)?;
    }
    let input = build_media_type(width, height, fps, &MFVideoFormat_NV12)?;
    unsafe { transform.SetInputType(input_id, &input, 0) }?;
    // Re-apply after media type negotiation because some vendor MFTs only honor
    // ICodecAPI settings once input and output types are locked in.
    apply_low_latency_settings(&codec, fps, bitrate);
    unsafe {
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }
    Ok((transform, events, codec, input_id, output_id))
}

fn create_transform(
    gpu: &VideoDevice,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    log_enabled: bool,
) -> Result<(IMFTransform, IMFMediaEventGenerator, ICodecAPI, u32, u32)> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut raw_activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &mut raw_activates,
            &mut count,
        )
    }
    .context("Could not enumerate Windows hardware H.264 encoders.")?;

    let activations: Vec<IMFActivate> = if !raw_activates.is_null() && count > 0 {
        let slice = unsafe { std::slice::from_raw_parts_mut(raw_activates, count as usize) };
        let list = slice.iter_mut().filter_map(Option::take).collect();
        unsafe { CoTaskMemFree(Some(raw_activates.cast())) };
        list
    } else {
        if !raw_activates.is_null() {
            unsafe { CoTaskMemFree(Some(raw_activates.cast())) };
        }
        Vec::new()
    };

    if log_enabled
        && let Ok(dxgi) = gpu.device.cast::<IDXGIDevice>()
        && let Ok(adapter) = unsafe { dxgi.GetAdapter() }
        && let Ok(desc) = unsafe { adapter.GetDesc() }
    {
        let end = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..end]);
        eprintln!(
            "[remoters:win] d3d11_adapter={name} mft_candidates={}",
            activations.len()
        );
    }

    let mut last_error: Option<anyhow::Error> = None;
    for activation in activations {
        match configure_transform(gpu, &activation, width, height, fps, bitrate) {
            Ok(configured) => {
                if log_enabled {
                    let mut ptr = windows_core::PWSTR::null();
                    let mut len = 0u32;
                    if unsafe {
                        activation.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len)
                    }
                    .is_ok()
                        && !ptr.is_null()
                    {
                        let name = unsafe { ptr.to_string() }.unwrap_or_default();
                        eprintln!("[remoters:win] selected_mft={name}");
                        unsafe { CoTaskMemFree(Some(ptr.0.cast())) };
                    }
                }
                return Ok(configured);
            }
            Err(err) => {
                let _ = unsafe { activation.ShutdownObject() };
                last_error = Some(err);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| windows_core::Error::from(MF_E_TOPO_CODEC_NOT_FOUND).into()))
}

fn capture(
    format: Format,
    center: (i32, i32),
    request: &AtomicBool,
    wake: &OwnedHandle,
    output: &mpsc::Sender<Result<AccessUnit>>,
) -> Result<()> {
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
    let bitrate = super::target_bitrate(format);
    let mut encoder = NativeEncoder::new(
        format.width.into(),
        format.height.into(),
        format.frames_per_second.into(),
        bitrate,
        center,
        wake,
    )
    .context("Could not start the Windows hardware encoder.")?;
    let started = Instant::now();
    let mut state = EncodedOutput {
        output,
        request,
        sequence: 0,
        synchronized: false,
        stats: crate::metrics::EncodeStats::new(),
        error: None,
    };
    while !output.is_closed() {
        encoder
            .poll(request.swap(false, Ordering::AcqRel), 50, &mut state)
            .context("Windows desktop capture or encoding failed.")?;
        if let Some(error) = state.error.take() {
            return Err(error);
        }
        ensure!(
            state.synchronized || started.elapsed() < Duration::from_secs(8) || state.sequence != 0,
            "The Windows encoder did not produce a desktop frame.",
        );
    }
    Ok(())
}

struct EncodedOutput<'a> {
    output: &'a mpsc::Sender<Result<AccessUnit>>,
    request: &'a AtomicBool,
    sequence: u64,
    synchronized: bool,
    stats: crate::metrics::EncodeStats,
    error: Option<anyhow::Error>,
}

impl EncodedOutput<'_> {
    fn emit(&mut self, picture: &[u8], header: &[u8], time: i64, encode_us: u32) {
        if self.error.is_some() {
            return;
        }
        let unit = (|| -> Result<AccessUnit> {
            ensure!(
                !picture.is_empty() && picture.len() <= MAX_ACCESS_UNIT,
                "Invalid hardware encoder access unit size.",
            );
            let mut parameters = 0;
            let mut keyframe = false;
            for nal in nal_units(picture) {
                match nal[0] & 31 {
                    5 => keyframe = true,
                    7 => parameters |= 1,
                    8 => parameters |= 2,
                    _ => {}
                }
            }

            let mut packet = Vec::new();
            if keyframe && parameters != 3 && !header.is_empty() {
                // MF_MT_MPEG_SEQUENCE_HEADER stores H.264 parameter sets in Annex B form.
                for nal in nal_units(header) {
                    if matches!(nal[0] & 31, 7 | 8) {
                        packet.extend_from_slice(&[0, 0, 0, 1]);
                        packet.extend_from_slice(nal);
                    }
                }
            }
            packet.extend_from_slice(picture);
            let timestamp = u64::try_from(time)? * 9 / 1_000;
            Ok(
                AccessUnit::from_annex_b(Codec::H264, self.sequence, packet.into())?
                    .with_rtp_timestamp(timestamp),
            )
        })();
        match unit {
            Ok(unit) => {
                self.sequence += 1;
                if !self.synchronized && !unit.is_random_access() {
                    self.request.store(true, Ordering::Release);
                    self.stats.record(u64::from(encode_us), true);
                    return;
                }
                match self.output.try_send(Ok(unit)) {
                    Ok(()) => {
                        self.synchronized = true;
                        self.stats.record(u64::from(encode_us), false);
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        self.synchronized = false;
                        self.request.store(true, Ordering::Release);
                        self.stats.record(u64::from(encode_us), true);
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
            Err(error) => self.error = Some(error),
        }
    }
}
