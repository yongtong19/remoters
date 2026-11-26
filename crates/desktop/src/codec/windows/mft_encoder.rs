//! Windows Media Foundation async hardware video encoder MFT backend with
//! tightened 2-frame VBV, 1.2x peak bitrate cap, and HDR10/P010 negotiation.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use remoters_core::video::Codec;
use windows::Win32::{
    Foundation::E_NOTIMPL,
    Graphics::Dxgi::IDXGIDevice,
    Media::MediaFoundation::{
        CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMaxBitRate,
        CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonQualityVsSpeed,
        CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
        CODECAPI_AVEncMPVGOPSize, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFActivate,
        IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFMediaEvent,
        IMFMediaEventGenerator, IMFMediaType, IMFShutdown, IMFTransform, MF_E_SHUTDOWN,
        MF_E_TOPO_CODEC_NOT_FOUND, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
        MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
        MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX,
        MF_TRANSFORM_ASYNC_UNLOCK, MFCreateMediaType, MFMediaType_Video, MFNominalRange_0_255,
        MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_FRIENDLY_NAME_Attribute, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
        MFT_MESSAGE_SET_D3D_MANAGER, MFT_REGISTER_TYPE_INFO, MFVideoFormat_H264,
        MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
        MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR,
        eAVEncCommonRateControlMode_LowDelayVBR, eAVEncCommonRateControlMode_PeakConstrainedVBR,
        eAVEncH264VProfile_Base,
    },
    System::{Com::CoTaskMemFree, Variant::VARIANT},
};
use windows_core::{GUID, HRESULT, IUnknownImpl, Interface, implement};

use super::common::{
    CaptureWake, MF_MT_TRANSFER_FUNCTION_GUID, MF_MT_VIDEO_PRIMARIES_GUID, MF_NOMINAL_RANGE_16_235,
    MF_VIDEO_FORMAT_AV1, MF_VIDEO_FORMAT_P010, MF_VIDEO_PRIMARIES_BT2020, MF_VIDEO_TRANS_FUNC_2084,
    MF_VIDEO_TRANSFER_MATRIX_BT2020_10, VideoDevice, enumerate_mfts, pack_u64,
};

/// Tightened strict 2-frame MFT VBV buffer in bytes (`bitrate` is bits/sec).
pub(super) fn mft_vbv_buffer_bytes(bitrate: u32, fps: u32) -> u32 {
    let fps = u64::from(fps.max(1));
    let bytes = (u64::from(bitrate) / fps) * 2 / 8;
    (bytes as u32).clamp(48 * 1024, 384 * 1024)
}

/// Tightened MFT peak bitrate (1.2x mean bitrate) to prevent burst explosions
/// when motion resumes after a static desktop period.
pub(super) fn mft_max_bitrate(bitrate: u32) -> u32 {
    bitrate.saturating_add(bitrate / 5)
}

pub(super) struct EventQueueState {
    pub(super) pending: VecDeque<IMFMediaEvent>,
    pub(super) source: Option<IMFMediaEventGenerator>,
    pub(super) error: Option<HRESULT>,
    pub(super) stopped: bool,
}

pub(super) struct EventQueue {
    pub(super) state: Mutex<EventQueueState>,
    pub(super) wake: Arc<CaptureWake>,
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

pub(super) struct MftBackend {
    // Field order matters: async callback/event queue stop before MFT shutdown.
    _event_relay: IMFAsyncCallback,
    pub(super) event_queue: Arc<EventQueue>,
    pub(super) codec: ICodecAPI,
    _events: IMFMediaEventGenerator,
    pub(super) transform: IMFTransform,
    pub(super) input_id: u32,
    pub(super) output_id: u32,
    pub(super) wants_input: bool,
    pub(super) waiting_for_output: bool,
}

impl MftBackend {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        gpu: &VideoDevice,
        codec_kind: Codec,
        hdr: bool,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        wake: &Arc<CaptureWake>,
        log_enabled: bool,
    ) -> Result<(Self, bool)> {
        let (transform, events, codec, input_id, output_id, use_p010) = create_transform(
            gpu,
            codec_kind,
            hdr,
            width,
            height,
            fps,
            bitrate,
            log_enabled,
        )?;
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
        Ok((
            Self {
                _event_relay: event_relay,
                event_queue,
                codec,
                _events: events,
                transform,
                input_id,
                output_id,
                wants_input: false,
                waiting_for_output: false,
            },
            use_p010,
        ))
    }

    pub(super) fn has_pending_events(&self) -> bool {
        let state = self.event_queue.state.lock().unwrap();
        !state.pending.is_empty() || state.error.is_some()
    }
}

impl Drop for MftBackend {
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
    }
}

pub(super) fn set_codec_u32(
    codec: &ICodecAPI,
    attribute: &GUID,
    value: u32,
) -> windows_core::Result<()> {
    let variant = VARIANT::from(value);
    unsafe { codec.SetValue(attribute, &variant) }
}

fn set_codec_bool(codec: &ICodecAPI, attribute: &GUID, value: bool) {
    let variant = VARIANT::from(value);
    if unsafe { codec.SetValue(attribute, &variant) }.is_err() {
        let _ = set_codec_u32(codec, attribute, u32::from(value));
    }
}

pub(super) fn apply_bitrate_settings(codec: &ICodecAPI, bitrate: u32, fps: u32) {
    let _ = set_codec_u32(codec, &CODECAPI_AVEncCommonMeanBitRate, bitrate);
    let _ = set_codec_u32(
        codec,
        &CODECAPI_AVEncCommonMaxBitRate,
        mft_max_bitrate(bitrate),
    );
    // Strict 2-frame VBV buffer in bytes to avoid static-to-motion burst spikes.
    let _ = set_codec_u32(
        codec,
        &CODECAPI_AVEncCommonBufferSize,
        mft_vbv_buffer_bytes(bitrate, fps),
    );
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
    apply_bitrate_settings(codec, bitrate, fps);
}

fn build_media_type(
    width: u32,
    height: u32,
    fps: u32,
    subtype: &GUID,
    hdr: bool,
) -> Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if hdr {
            media_type.SetUINT32(&MF_MT_YUV_MATRIX, MF_VIDEO_TRANSFER_MATRIX_BT2020_10)?;
            media_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES_GUID, MF_VIDEO_PRIMARIES_BT2020)?;
            media_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION_GUID, MF_VIDEO_TRANS_FUNC_2084)?;
            media_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MF_NOMINAL_RANGE_16_235)?;
        } else {
            media_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            media_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_0_255.0 as u32)?;
        }
    }
    Ok(media_type)
}

#[allow(clippy::too_many_arguments)]
fn negotiate_mft_types(
    transform: &IMFTransform,
    codec_kind: Codec,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    input_id: u32,
    output_id: u32,
    hdr: bool,
) -> Result<()> {
    const HEVC_MAIN_PROFILE: u32 = 1; // eAVEncH265VProfile_Main_420_8
    const HEVC_MAIN10_PROFILE: u32 = 2; // eAVEncH265VProfile_Main_420_10
    let subtype = match codec_kind {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MF_VIDEO_FORMAT_AV1,
    };
    let output = build_media_type(width, height, fps, &subtype, hdr)?;
    unsafe {
        output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        match codec_kind {
            Codec::H264 => {
                output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
            }
            Codec::Hevc => {
                let profile = if hdr {
                    HEVC_MAIN10_PROFILE
                } else {
                    HEVC_MAIN_PROFILE
                };
                output.SetUINT32(&MF_MT_MPEG2_PROFILE, profile)?;
            }
            Codec::Av1 => {}
        }
        transform.SetOutputType(output_id, &output, 0)?;
    }
    let input_subtype = if hdr {
        MF_VIDEO_FORMAT_P010
    } else {
        MFVideoFormat_NV12
    };
    let input = build_media_type(width, height, fps, &input_subtype, hdr)?;
    unsafe { transform.SetInputType(input_id, &input, 0) }?;
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn configure_transform(
    gpu: &VideoDevice,
    activation: &IMFActivate,
    codec_kind: Codec,
    hdr: bool,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
) -> Result<(
    IMFTransform,
    IMFMediaEventGenerator,
    ICodecAPI,
    u32,
    u32,
    bool,
)> {
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
    let (input_id, output_id) =
        match unsafe { transform.GetStreamIDs(&mut input_ids, &mut output_ids) } {
            Ok(()) => (input_ids[0], output_ids[0]),
            Err(err) if err.code() == E_NOTIMPL => (0, 0),
            Err(err) => return Err(err.into()),
        };

    apply_low_latency_settings(&codec, fps, bitrate);
    let want_p010 = hdr && matches!(codec_kind, Codec::Hevc | Codec::Av1);
    let use_p010 = if want_p010 {
        match negotiate_mft_types(
            &transform, codec_kind, width, height, fps, bitrate, input_id, output_id, true,
        ) {
            Ok(()) => true,
            Err(err) => {
                eprintln!(
                    "[remoters:win] HDR10/P010 MFT configuration failed ({err:#}); falling back to NV12 SDR"
                );
                negotiate_mft_types(
                    &transform, codec_kind, width, height, fps, bitrate, input_id, output_id, false,
                )?;
                false
            }
        }
    } else {
        negotiate_mft_types(
            &transform, codec_kind, width, height, fps, bitrate, input_id, output_id, false,
        )?;
        false
    };
    // Re-apply after media type negotiation because some vendor MFTs only honor
    // ICodecAPI settings once input and output types are locked in.
    apply_low_latency_settings(&codec, fps, bitrate);
    unsafe {
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }
    Ok((transform, events, codec, input_id, output_id, use_p010))
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn create_transform(
    gpu: &VideoDevice,
    codec_kind: Codec,
    hdr: bool,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    log_enabled: bool,
) -> Result<(
    IMFTransform,
    IMFMediaEventGenerator,
    ICodecAPI,
    u32,
    u32,
    bool,
)> {
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: match codec_kind {
            Codec::H264 => MFVideoFormat_H264,
            Codec::Hevc => MFVideoFormat_HEVC,
            Codec::Av1 => MF_VIDEO_FORMAT_AV1,
        },
    };
    let activations = enumerate_mfts(
        MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        None,
        Some(&output),
    )
    .context("Could not enumerate Windows hardware video encoders.")?;

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
        match configure_transform(
            gpu,
            &activation,
            codec_kind,
            hdr,
            width,
            height,
            fps,
            bitrate,
        ) {
            Ok(configured) => {
                if log_enabled {
                    let mut ptr = windows_core::PWSTR::null();
                    let mut len = 0u32;
                    if unsafe {
                        activation.GetAllocatedString(
                            &MFT_FRIENDLY_NAME_Attribute,
                            &mut ptr,
                            &mut len,
                        )
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
