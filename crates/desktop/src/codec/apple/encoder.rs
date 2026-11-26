//! In-process VideoToolbox hardware video encoder using ScreenCaptureKit IOSurfaces.

use std::{
    ffi::c_void,
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use core_video::image_buffer::{
    kCVImageBufferColorPrimaries_ITU_R_2020, kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ,
    kCVImageBufferYCbCrMatrix_ITU_R_2020,
};
use remoters_core::video::{Codec, Format};
use screencapturekit::cv::CVPixelBuffer;

use super::{
    ffi::{
        CMBlockBufferCopyDataBytes, CMBlockBufferGetDataLength, CMFormatDescriptionGetMediaSubType,
        CMSampleBufferGetDataBuffer, CMSampleBufferGetFormatDescription,
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
        CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, Owned, Ref, Time,
        VTCompressionSessionCompleteFrames, VTCompressionSessionCreate,
        VTCompressionSessionEncodeFrame, VTCompressionSessionInvalidate,
        VTCompressionSessionPrepareToEncodeFrames, VTSessionSetProperty, check,
        kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
        kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_DataRateLimits,
        kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxFrameDelayCount,
        kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
        kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
        kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame,
        kVTProfileLevel_H264_Baseline_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
        kVTProfileLevel_HEVC_Main10_AutoLevel,
        kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
    },
    rate_control::{PtsClamper, data_rate_limits},
};

type Encoded = std::sync::Mutex<Option<Result<Vec<u8>>>>;

pub struct HardwareEncoder {
    session: Owned,
    output: Box<Encoded>,
    force_keyframe: CFDictionary<CFString, CFType>,
    frame_duration: Time,
    pts: PtsClamper,
    data_rate_limits_warned: AtomicBool,
}

impl HardwareEncoder {
    pub fn new(format: Format, bitrate_bps: u32) -> Result<Self> {
        let mut output = Box::new(Encoded::new(None));
        // SAFETY: These are process-lifetime framework constants.
        let hardware = CFDictionary::from_CFType_pairs(&[(
            unsafe {
                CFString::wrap_under_get_rule(
                    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                )
            },
            CFBoolean::true_value().as_CFType(),
        )]);
        let force_keyframe = CFDictionary::from_CFType_pairs(&[(
            unsafe { CFString::wrap_under_get_rule(kVTEncodeFrameOptionKey_ForceKeyFrame) },
            CFBoolean::true_value().as_CFType(),
        )]);
        // SAFETY: These are process-lifetime framework constants.
        let (codec_type, profile_level) = unsafe {
            match format.codec {
                Codec::H264 => (
                    u32::from_be_bytes(*b"avc1"),
                    kVTProfileLevel_H264_Baseline_AutoLevel,
                ),
                Codec::Hevc => (
                    u32::from_be_bytes(*b"hvc1"),
                    if format.hdr {
                        kVTProfileLevel_HEVC_Main10_AutoLevel
                    } else {
                        kVTProfileLevel_HEVC_Main_AutoLevel
                    },
                ),
                Codec::Av1 => bail!("macOS hardware encoding does not support AV1."),
            }
        };
        let session = Owned::create("Opening the macOS hardware encoder", |created| {
            // SAFETY: The output state is boxed and retained through session invalidation.
            unsafe {
                VTCompressionSessionCreate(
                    null(),
                    i32::from(format.width),
                    i32::from(format.height),
                    codec_type,
                    hardware.as_CFTypeRef().cast(),
                    null(),
                    null(),
                    encoded,
                    std::ptr::from_mut(output.as_mut()).cast(),
                    created,
                )
            }
        })?;
        let fps = format.frames_per_second.max(1);
        let fps_i32 = i32::from(fps);
        let encoder = Self {
            session,
            output,
            force_keyframe,
            frame_duration: Time::new(1, fps_i32),
            pts: PtsClamper::new(fps),
            data_rate_limits_warned: AtomicBool::new(false),
        };
        let bitrate = i32::try_from(bitrate_bps.max(1))?;
        // SAFETY: Every property key and value is valid and retained through the call.
        unsafe {
            encoder.property(kVTCompressionPropertyKey_RealTime, &CFBoolean::true_value())?;
            encoder.property(
                kVTCompressionPropertyKey_AllowFrameReordering,
                &CFBoolean::false_value(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_ProfileLevel,
                &CFString::wrap_under_get_rule(profile_level),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::from(bitrate),
            )?;
            encoder.apply_data_rate_limits(bitrate_bps);
            encoder.property(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::from(fps_i32),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::from(fps_i32 * 300),
            )?;
            let _ = encoder.property(
                kVTCompressionPropertyKey_MaxFrameDelayCount,
                &CFNumber::from(0),
            );
            if format.hdr && format.codec == Codec::Hevc {
                encoder.property(
                    kVTCompressionPropertyKey_ColorPrimaries,
                    &CFString::wrap_under_get_rule(kCVImageBufferColorPrimaries_ITU_R_2020),
                )?;
                encoder.property(
                    kVTCompressionPropertyKey_TransferFunction,
                    &CFString::wrap_under_get_rule(kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ),
                )?;
                encoder.property(
                    kVTCompressionPropertyKey_YCbCrMatrix,
                    &CFString::wrap_under_get_rule(kCVImageBufferYCbCrMatrix_ITU_R_2020),
                )?;
            }
            check(
                VTCompressionSessionPrepareToEncodeFrames(encoder.session.0),
                "Preparing the macOS hardware encoder",
            )?;
        }
        Ok(encoder)
    }

    pub fn set_bitrate(&self, bitrate_bps: u32) -> Result<()> {
        let bitrate = i32::try_from(bitrate_bps.max(1))?;
        // SAFETY: The session is live and CFNumber is retained across the call.
        unsafe {
            self.property(
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::from(bitrate),
            )?;
        }
        self.apply_data_rate_limits(bitrate_bps);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn data_rate_limits_warned(&self) -> bool {
        self.data_rate_limits_warned.load(Ordering::Relaxed)
    }

    fn apply_data_rate_limits(&self, bitrate_bps: u32) {
        let (bytes_per_window, window_seconds) = data_rate_limits(bitrate_bps);
        let limits = CFArray::from_CFTypes(&[
            CFNumber::from(bytes_per_window),
            CFNumber::from(window_seconds),
        ]);
        // SAFETY: The session and CFArray are live through the call.
        let status = unsafe { self.property(kVTCompressionPropertyKey_DataRateLimits, &limits) };
        if let Err(err) = status
            && !self.data_rate_limits_warned.swap(true, Ordering::Relaxed)
        {
            eprintln!(
                "macOS hardware encoder DataRateLimits rejected; continuing with AverageBitRate: {err:#}"
            );
        }
    }

    unsafe fn property(&self, key: CFStringRef, value: &impl TCFType) -> Result<()> {
        // SAFETY: The caller supplies a valid property key. The value and session are live.
        check(
            unsafe { VTSessionSetProperty(self.session.0, key, value.as_CFTypeRef().cast()) },
            "Configuring the macOS hardware encoder",
        )
    }

    pub fn encode(
        &mut self,
        surface: &CVPixelBuffer,
        timestamp: u64,
        keyframe: bool,
    ) -> Result<Option<Vec<u8>>> {
        let settings = if keyframe {
            self.force_keyframe.as_CFTypeRef().cast()
        } else {
            null()
        };
        let clamped_pts = self.pts.next(timestamp);
        let presentation = Time::ticks(i64::try_from(clamped_pts)?);
        // SAFETY: The image is alive through the encoder submission. VideoToolbox
        // retains it for any pending work, and the callback state remains live.
        unsafe {
            check(
                VTCompressionSessionEncodeFrame(
                    self.session.0,
                    surface.as_ptr().cast(),
                    presentation,
                    self.frame_duration,
                    settings,
                    null_mut(),
                    null_mut(),
                ),
                "Submitting a desktop frame",
            )?;
            check(
                VTCompressionSessionCompleteFrames(self.session.0, presentation),
                "Receiving the encoded desktop frame",
            )?;
        }
        self.output.lock().unwrap().take().transpose()
    }
}

impl Drop for HardwareEncoder {
    fn drop(&mut self) {
        // SAFETY: Invalidation finishes callbacks before their boxed state is freed.
        unsafe { VTCompressionSessionInvalidate(self.session.0) };
    }
}

extern "C" fn encoded(context: *mut c_void, _: *mut c_void, status: i32, _: u32, sample: Ref) {
    // SAFETY: The encoder keeps the output state alive until callbacks stop.
    let Some(output) = (unsafe { context.cast::<Encoded>().as_ref() }) else {
        return;
    };
    let result = if status == 0 {
        if sample.is_null() {
            return;
        }
        encoded_bytes(sample)
    } else {
        Err(anyhow!("Hardware video encoder returned status {status}."))
    };
    if let Ok(mut output) = output.lock() {
        *output = Some(result);
    }
}

fn encoded_bytes(sample: Ref) -> Result<Vec<u8>> {
    const AVC1: u32 = u32::from_be_bytes(*b"avc1");
    const HVC1: u32 = u32::from_be_bytes(*b"hvc1");
    // SAFETY: VideoToolbox keeps the sample and its borrowed buffers alive in the callback.
    unsafe {
        let block = CMSampleBufferGetDataBuffer(sample);
        let description = CMSampleBufferGetFormatDescription(sample);
        ensure!(
            !block.is_null() && !description.is_null(),
            "Incomplete encoded video sample."
        );
        let size = CMBlockBufferGetDataLength(block);
        ensure!(
            size <= remoters_core::video::MAX_ACCESS_UNIT,
            "Encoded desktop frame is too large."
        );
        let mut compressed = vec![0; size];
        check(
            CMBlockBufferCopyDataBytes(block, 0, size, compressed.as_mut_ptr()),
            "Reading the encoded desktop frame",
        )?;
        let subtype = CMFormatDescriptionGetMediaSubType(description);
        let is_hevc = match subtype {
            HVC1 => true,
            AVC1 => false,
            _ => anyhow::bail!("Unsupported encoded video format subtype: {subtype:#010x}."),
        };
        let mut param_count = 0usize;
        let mut length_field = 0;
        if is_hevc {
            check(
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    description,
                    0,
                    null_mut(),
                    null_mut(),
                    &mut param_count,
                    &mut length_field,
                ),
                "Reading the encoder's HEVC format",
            )?;
        } else {
            check(
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    description,
                    0,
                    null_mut(),
                    null_mut(),
                    &mut param_count,
                    &mut length_field,
                ),
                "Reading the encoder's H.264 format",
            )?;
        }
        ensure!(length_field == 4, "Unsupported encoder NAL length field.");
        let mut nals = Vec::new();
        let mut bytes = compressed.as_slice();
        while !bytes.is_empty() {
            let (length, remaining) = bytes
                .split_at_checked(4)
                .context("Truncated encoded NAL header.")?;
            let length = u32::from_be_bytes(length.try_into()?) as usize;
            ensure!(length > 0, "Empty encoded NAL unit.");
            let (nal, rest) = remaining
                .split_at_checked(length)
                .context("Truncated encoded NAL unit.")?;
            nals.push(nal);
            bytes = rest;
        }
        let is_keyframe = if is_hevc {
            nals.iter()
                .any(|nal| matches!((nal[0] >> 1) & 0x3f, 19 | 20 | 21))
        } else {
            nals.iter().any(|nal| nal[0] & 0x1f == 5)
        };
        let mut annex_b = Vec::with_capacity(size + 128);
        if is_keyframe {
            for index in 0..param_count {
                let mut data = null();
                let mut length = 0;
                let status = if is_hevc {
                    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                        description,
                        index,
                        &mut data,
                        &mut length,
                        null_mut(),
                        null_mut(),
                    )
                } else {
                    CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        description,
                        index,
                        &mut data,
                        &mut length,
                        null_mut(),
                        null_mut(),
                    )
                };
                check(status, "Reading encoder parameter sets")?;
                ensure!(!data.is_null(), "Missing encoder parameter set.");
                annex_b.extend_from_slice(&[0, 0, 0, 1]);
                annex_b.extend_from_slice(std::slice::from_raw_parts(data, length));
            }
        }
        for nal in nals {
            annex_b.extend_from_slice(&[0, 0, 0, 1]);
            annex_b.extend_from_slice(nal);
        }
        Ok(annex_b)
    }
}
