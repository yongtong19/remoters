//! Hardware/fallback H.264, HEVC & AV1 decoder using Windows Media Foundation
//! MFT + D3D11 VideoProcessor (including 10-bit P010 HDR10 -> BGRA8 tone-mapping).

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, anyhow, ensure};
use gpui::{ID3D11Device, ID3D11Texture2D};
use remoters_core::video::{Codec, Format, ReceivedFrame};
use windows::Win32::{
    Foundation::{RECT, TRUE},
    Graphics::{
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE,
            D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11VideoContext1,
            ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator,
        },
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC},
    },
    Media::MediaFoundation::{
        CMSH264DecoderMFT, IMF2DBuffer, IMFDXGIBuffer, IMFMediaBuffer, IMFSample, IMFShutdown,
        IMFTransform, MF_E_INVALIDMEDIATYPE, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
        MF_E_TRANSFORM_STREAM_CHANGE, MF_E_UNSUPPORTED_D3D_TYPE, MF_LOW_LATENCY, MF_MT_FRAME_SIZE,
        MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SA_D3D11_AWARE, MF_TRANSFORM_ASYNC_UNLOCK,
        MFCreateAlignedMemoryBuffer, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
        MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_ASYNCMFT,
        MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
        MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
        MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFVideoFormat_H264,
        MFVideoFormat_HEVC, MFVideoFormat_NV12,
    },
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
};
use windows_core::{GUID, Interface};

use crate::platform::VideoFrame;

use super::common::{
    BT709_FULL_RANGE, DXGI_CS_RGB_FULL_G22_NONE_P709, DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020,
    DXGI_FORMAT_P010, MF_VIDEO_FORMAT_AV1, MF_VIDEO_FORMAT_P010, VideoDevice, enumerate_mfts,
    unpack_u64,
};

struct NativeDecoder {
    processor: Option<ID3D11VideoProcessor>,
    enumerator: Option<ID3D11VideoProcessorEnumerator>,
    upload_surface: Option<(ID3D11Texture2D, u32, u32, bool)>,
    upload_scratch: Vec<u8>,
    transform: IMFTransform,
    gpu: VideoDevice,
    output_subtype: GUID,
    hdr: bool,
    input_is_p010: bool,
    frame_width: u32,
    frame_height: u32,
    target_width: u32,
    target_height: u32,
}

impl Drop for NativeDecoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
        self.upload_surface = None;
        self.processor = None;
        self.enumerator = None;
    }
}

fn select_decoder_output_type(
    transform: &IMFTransform,
    prefer_p010: bool,
) -> Result<(u32, u32, GUID)> {
    let mut nv12_type = None;
    let mut p010_type = None;
    for index in 0u32.. {
        let Ok(output) = (unsafe { transform.GetOutputAvailableType(0, index) }) else {
            break;
        };
        let Ok(out_subtype) = (unsafe { output.GetGUID(&MF_MT_SUBTYPE) }) else {
            continue;
        };
        if out_subtype == MF_VIDEO_FORMAT_P010 && p010_type.is_none() {
            p010_type = Some(output);
        } else if out_subtype == MFVideoFormat_NV12 && nv12_type.is_none() {
            nv12_type = Some(output);
        }
    }
    let (chosen, chosen_subtype) = if prefer_p010 {
        match (p010_type, nv12_type) {
            (Some(out), _) => (out, MF_VIDEO_FORMAT_P010),
            (None, Some(out)) => (out, MFVideoFormat_NV12),
            (None, None) => anyhow::bail!("Windows video decoder offers no P010 or NV12 output."),
        }
    } else {
        match (nv12_type, p010_type) {
            (Some(out), _) => (out, MFVideoFormat_NV12),
            (None, Some(out)) => (out, MF_VIDEO_FORMAT_P010),
            (None, None) => anyhow::bail!("Windows video decoder offers no NV12 or P010 output."),
        }
    };
    unsafe { transform.SetOutputType(0, &chosen, 0) }?;
    let mut frame_width = 0;
    let mut frame_height = 0;
    if let Ok(size) = unsafe { chosen.GetUINT64(&MF_MT_FRAME_SIZE) } {
        (frame_width, frame_height) = unpack_u64(size);
    }
    Ok((frame_width, frame_height, chosen_subtype))
}

fn configure_decoder_transform(
    gpu: &VideoDevice,
    transform: IMFTransform,
    subtype: &GUID,
    require_d3d11: bool,
    prefer_p010: bool,
) -> Result<(IMFTransform, u32, u32, GUID)> {
    let attributes =
        unsafe { transform.GetAttributes() }.context("Could not query decoder attributes.")?;
    let _ = unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) };
    let aware = unsafe { attributes.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0);
    if aware == 0 {
        if require_d3d11 {
            return Err(windows_core::Error::from(MF_E_UNSUPPORTED_D3D_TYPE).into());
        }
    } else {
        let _ = unsafe { attributes.SetUINT32(&MF_LOW_LATENCY, 1) };
        unsafe {
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, gpu.manager.as_raw() as usize)
        }?;
    }

    let input = unsafe { MFCreateMediaType() }?;
    unsafe {
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        transform.SetInputType(0, &input, 0)?;
    }

    let (frame_width, frame_height, output_subtype) =
        select_decoder_output_type(&transform, prefer_p010)?;
    unsafe {
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }
    Ok((transform, frame_width, frame_height, output_subtype))
}

fn open_mft_decoder(
    gpu: &VideoDevice,
    subtype: GUID,
    prefer_p010: bool,
    codec_label: &str,
) -> Result<(IMFTransform, u32, u32, GUID)> {
    let input_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let flag_sets = [
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
    ];
    let mut last_error: Option<anyhow::Error> = None;
    for flags in flag_sets {
        let Ok(activations) =
            enumerate_mfts(MFT_CATEGORY_VIDEO_DECODER, flags, Some(&input_info), None)
        else {
            continue;
        };
        for activation in activations {
            let Ok(transform) = (unsafe { activation.ActivateObject::<IMFTransform>() }) else {
                let _ = unsafe { activation.ShutdownObject() };
                continue;
            };
            match configure_decoder_transform(gpu, transform, &subtype, false, prefer_p010) {
                Ok(configured) => return Ok(configured),
                Err(err) => {
                    let _ = unsafe { activation.ShutdownObject() };
                    last_error = Some(err);
                }
            }
        }
    }
    Err(last_error
        .unwrap_or_else(|| anyhow!("No Windows {codec_label} video decoder MFT is available.")))
}

impl NativeDecoder {
    fn new(device: ID3D11Device, codec: Codec, hdr: bool) -> Result<Self> {
        let gpu = VideoDevice::new(Some(device))?;
        let prefer_p010 = hdr && matches!(codec, Codec::Hevc | Codec::Av1);
        let (transform, frame_width, frame_height, output_subtype) = match codec {
            Codec::H264 => {
                let transform: IMFTransform =
                    unsafe { CoCreateInstance(&CMSH264DecoderMFT, None, CLSCTX_INPROC_SERVER) }
                        .context("Could not create Windows H.264 hardware decoder MFT.")?;
                configure_decoder_transform(&gpu, transform, &MFVideoFormat_H264, true, false)?
            }
            Codec::Hevc => open_mft_decoder(&gpu, MFVideoFormat_HEVC, prefer_p010, "HEVC")?,
            Codec::Av1 => open_mft_decoder(&gpu, MF_VIDEO_FORMAT_AV1, prefer_p010, "AV1")?,
        };

        Ok(Self {
            processor: None,
            enumerator: None,
            upload_surface: None,
            upload_scratch: Vec::new(),
            transform,
            gpu,
            output_subtype,
            hdr,
            input_is_p010: false,
            frame_width,
            frame_height,
            target_width: 0,
            target_height: 0,
        })
    }

    fn set_output(&mut self) -> Result<()> {
        let (w, h, subtype) = select_decoder_output_type(&self.transform, self.hdr)?;
        self.frame_width = w;
        self.frame_height = h;
        self.output_subtype = subtype;
        Ok(())
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
            let mut output = [MFT_OUTPUT_DATA_BUFFER::default()];
            if (info.dwFlags & provides) == 0 {
                let align = info.cbAlignment.saturating_sub(1);
                let buffer = unsafe { MFCreateAlignedMemoryBuffer(info.cbSize.max(1), align) }?;
                let storage = unsafe { MFCreateSample() }?;
                unsafe { storage.AddBuffer(&buffer) }?;
                output[0].pSample = ManuallyDrop::new(Some(storage));
            }
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

    fn ensure_processor(&mut self, width: u32, height: u32, is_p010: bool) -> Result<()> {
        if self.processor.is_some()
            && self.target_width == width
            && self.target_height == height
            && self.input_is_p010 == is_p010
        {
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
        self.input_is_p010 = is_p010;
        Ok(())
    }

    fn upload_cpu_surface(
        &mut self,
        buffer: &IMFMediaBuffer,
        width: u32,
        height: u32,
        is_p010: bool,
    ) -> Result<ID3D11Texture2D> {
        let valid = width > 0
            && height > 0
            && width <= 3840
            && height <= 3840
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
            && u64::from(width) * u64::from(height) <= 3840 * 2160;
        if !valid {
            return Err(windows_core::Error::from(MF_E_INVALIDMEDIATYPE).into());
        }
        let bpp = if is_p010 { 2usize } else { 1usize };
        let needed = (width as usize) * (height as usize) * 3 / 2 * bpp;
        self.upload_scratch.resize(needed, 0);
        if let Ok(buf2d) = buffer.cast::<IMF2DBuffer>() {
            unsafe { buf2d.ContiguousCopyTo(&mut self.upload_scratch) }?;
        } else {
            let mut ptr = std::ptr::null_mut();
            let mut cur_len = 0u32;
            unsafe { buffer.Lock(&mut ptr, None, Some(&mut cur_len)) }?;
            let copy_result = (|| -> Result<()> {
                ensure!(
                    !ptr.is_null() && (cur_len as usize) >= needed,
                    "Decoded video buffer is smaller than expected."
                );
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        ptr.cast_const(),
                        self.upload_scratch.as_mut_ptr(),
                        needed,
                    );
                }
                Ok(())
            })();
            unsafe { buffer.Unlock() }?;
            copy_result?;
        }
        let need_new = !matches!(
            self.upload_surface,
            Some((_, w, h, p010)) if w == width && h == height && p010 == is_p010
        );
        if need_new {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: if is_p010 {
                    DXGI_FORMAT_P010
                } else {
                    DXGI_FORMAT_NV12
                },
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                ..Default::default()
            };
            let mut tex = None;
            unsafe { self.gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }?;
            let tex = tex.context("Could not allocate decoded video upload texture.")?;
            self.upload_surface = Some((tex, width, height, is_p010));
        }
        let tex = self.upload_surface.as_ref().unwrap().0.clone();
        let row_pitch = width * (bpp as u32);
        unsafe {
            self.gpu.context.UpdateSubresource(
                &tex,
                0,
                None,
                self.upload_scratch.as_ptr().cast(),
                row_pitch,
                0,
            );
        }
        Ok(tex)
    }

    fn present(&mut self, sample: &IMFSample) -> Result<VideoFrame> {
        let buffer = unsafe { sample.GetBufferByIndex(0) }?;
        let (picture, subresource, mip_levels, width, height, is_p010) =
            if let Ok(gpu_buffer) = buffer.cast::<IMFDXGIBuffer>() {
                let mut picture: Option<ID3D11Texture2D> = None;
                unsafe {
                    gpu_buffer.GetResource(
                        &ID3D11Texture2D::IID,
                        std::ptr::from_mut(&mut picture).cast(),
                    )
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
                let is_p010 = layout.Format == DXGI_FORMAT_P010
                    || self.output_subtype == MF_VIDEO_FORMAT_P010;
                (
                    picture,
                    subresource,
                    layout.MipLevels.max(1),
                    width,
                    height,
                    is_p010,
                )
            } else {
                let (mut width, mut height) = (self.frame_width, self.frame_height);
                if let Ok(media) = unsafe { self.transform.GetOutputCurrentType(0) } {
                    if let Ok(subtype) = unsafe { media.GetGUID(&MF_MT_SUBTYPE) } {
                        self.output_subtype = subtype;
                    }
                    if (width == 0 || height == 0)
                        && let Ok(size) = unsafe { media.GetUINT64(&MF_MT_FRAME_SIZE) }
                    {
                        (width, height) = unpack_u64(size);
                        self.frame_width = width;
                        self.frame_height = height;
                    }
                }
                let is_p010 = self.output_subtype == MF_VIDEO_FORMAT_P010;
                let picture = self.upload_cpu_surface(&buffer, width, height, is_p010)?;
                (picture, 0, 1, width, height, is_p010)
            };
        self.ensure_processor(width, height, is_p010)?;
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
        unsafe {
            self.gpu
                .device
                .CreateTexture2D(&desc, None, Some(&mut target))
        }?;
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
        in_desc.Anonymous.Texture2D.ArraySlice = subresource / mip_levels;
        in_desc.Anonymous.Texture2D.MipSlice = subresource % mip_levels;
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
        let video_ctx1 = (is_p010 || self.hdr)
            .then(|| self.gpu.video_context.cast::<ID3D11VideoContext1>().ok())
            .flatten();
        unsafe {
            self.gpu.video_context.VideoProcessorSetStreamSourceRect(
                processor,
                0,
                true,
                Some(&rect),
            );
            self.gpu
                .video_context
                .VideoProcessorSetStreamDestRect(processor, 0, true, Some(&rect));
            self.gpu
                .video_context
                .VideoProcessorSetStreamAutoProcessingMode(processor, 0, false);
            if let Some(ctx1) = &video_ctx1 {
                ctx1.VideoProcessorSetStreamColorSpace1(
                    processor,
                    0,
                    DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020,
                );
                ctx1.VideoProcessorSetOutputColorSpace1(processor, DXGI_CS_RGB_FULL_G22_NONE_P709);
            } else {
                self.gpu.video_context.VideoProcessorSetStreamColorSpace(
                    processor,
                    0,
                    &BT709_FULL_RANGE,
                );
            }
            self.gpu.video_context.VideoProcessorSetOutputAlphaFillMode(
                processor,
                D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE,
                0,
            );
            let blt =
                self.gpu
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
    codec: Option<Codec>,
    hdr: bool,
    inner: Option<NativeDecoder>,
}

impl Decoder {
    #[allow(dead_code)]
    pub fn new(device: ID3D11Device) -> Self {
        Self {
            device,
            codec: None,
            hdr: false,
            inner: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_format(device: ID3D11Device, format: Format) -> Self {
        Self {
            device,
            codec: None,
            hdr: format.hdr,
            inner: None,
        }
    }

    pub fn decode(&mut self, frame: &ReceivedFrame) -> Result<Option<VideoFrame>> {
        if self.codec != Some(frame.codec) {
            self.inner = None;
            self.codec = None;
            self.inner = Some(NativeDecoder::new(
                self.device.clone(),
                frame.codec,
                self.hdr,
            )?);
            self.codec = Some(frame.codec);
        }
        let decoder = self.inner.as_mut().unwrap();
        let time = i64::try_from(u128::from(frame.rtp_timestamp) * 10_000_000 / 90_000)?;
        decoder.submit(&frame.bytes, time)
    }
}
