//! CoreMedia and VideoToolbox FFI declarations and RAII helpers for macOS.

use std::{ffi::c_void, ptr::null};

use anyhow::{Result, ensure};
use core_foundation::{
    base::{CFRelease, CFType, TCFType},
    string::{CFString, CFStringRef},
};
use core_video::image_buffer::{
    kCVImageBufferColorPrimaries_ITU_R_2020, kCVImageBufferColorPrimariesKey,
    kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ, kCVImageBufferTransferFunctionKey,
    kCVImageBufferYCbCrMatrix_ITU_R_2020, kCVImageBufferYCbCrMatrixKey,
};

pub(super) type Ref = *const c_void;
pub(super) const RTP_CLOCK: i32 = 90_000;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Time {
    value: i64,
    scale: i32,
    flags: u32,
    epoch: i64,
}

impl Time {
    pub(super) const INVALID: Self = Self {
        value: 0,
        scale: 0,
        flags: 0,
        epoch: 0,
    };

    pub(super) fn new(value: i64, scale: i32) -> Self {
        Self {
            value,
            scale,
            flags: 1,
            epoch: 0,
        }
    }

    pub(super) fn ticks(value: i64) -> Self {
        Self::new(value, RTP_CLOCK)
    }
}

#[repr(C)]
pub(super) struct Timing {
    pub(super) duration: Time,
    pub(super) presentation: Time,
    pub(super) decode: Time,
}

pub(super) struct Owned(pub(super) Ref);

impl Owned {
    pub(super) fn create(label: &str, start: impl FnOnce(&mut Ref) -> i32) -> Result<Self> {
        let mut result = null();
        check(start(&mut result), label)?;
        ensure!(!result.is_null(), "{label} returned no object.");
        Ok(Self(result))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: Each Owned value holds one retained CF reference.
        unsafe { CFRelease(self.0.cast()) };
    }
}

pub(super) fn check(status: i32, label: &str) -> Result<()> {
    ensure!(
        status == 0,
        "{label} returned VideoToolbox status {status}."
    );
    Ok(())
}

pub(super) type DecodeCallback = extern "C" fn(*mut c_void, *mut c_void, i32, u32, Ref, Time, Time);

#[repr(C)]
pub(super) struct DecodeCallbacks {
    pub(super) callback: DecodeCallback,
    pub(super) context: *mut c_void,
}

pub(super) type EncodeCallback = extern "C" fn(*mut c_void, *mut c_void, i32, u32, Ref);

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    pub(super) static kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms: CFStringRef;
    pub(super) fn CMVideoFormatDescriptionCreate(
        allocator: Ref,
        codec_type: u32,
        width: i32,
        height: i32,
        extensions: Ref,
        result: *mut Ref,
    ) -> i32;
    pub(super) fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: Ref,
        count: usize,
        parameters: *const *const u8,
        lengths: *const usize,
        header_length: i32,
        result: *mut Ref,
    ) -> i32;
    pub(super) fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: Ref,
        count: usize,
        parameters: *const *const u8,
        lengths: *const usize,
        header_length: i32,
        extensions: Ref,
        result: *mut Ref,
    ) -> i32;
    pub(super) fn CMBlockBufferCreateWithMemoryBlock(
        allocator: Ref,
        memory: *mut c_void,
        length: usize,
        block_allocator: Ref,
        source: Ref,
        offset: usize,
        data_length: usize,
        flags: u32,
        result: *mut Ref,
    ) -> i32;
    pub(super) fn CMBlockBufferReplaceDataBytes(
        source: *const u8,
        block: Ref,
        offset: usize,
        length: usize,
    ) -> i32;
    pub(super) fn CMSampleBufferCreateReady(
        allocator: Ref,
        block: Ref,
        description: Ref,
        count: isize,
        timing_count: isize,
        timing: *const Timing,
        size_count: isize,
        sizes: *const usize,
        result: *mut Ref,
    ) -> i32;
    pub(super) fn CMSampleBufferGetDataBuffer(sample: Ref) -> Ref;
    pub(super) fn CMSampleBufferGetFormatDescription(sample: Ref) -> Ref;
    pub(super) fn CMFormatDescriptionGetMediaSubType(description: Ref) -> u32;
    pub(super) fn CMBlockBufferGetDataLength(block: Ref) -> usize;
    pub(super) fn CMBlockBufferCopyDataBytes(
        block: Ref,
        offset: usize,
        length: usize,
        destination: *mut u8,
    ) -> i32;
    pub(super) fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        description: Ref,
        index: usize,
        parameter: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        length_field: *mut i32,
    ) -> i32;
    pub(super) fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        description: Ref,
        index: usize,
        parameter: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        length_field: *mut i32,
    ) -> i32;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    pub(super) static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder:
        CFStringRef;
    pub(super) static kVTDecompressionPropertyKey_RealTime: CFStringRef;
    pub(super) static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder:
        CFStringRef;
    pub(super) static kVTCompressionPropertyKey_RealTime: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    pub(super) static kVTProfileLevel_H264_Baseline_AutoLevel: CFStringRef;
    pub(super) static kVTProfileLevel_HEVC_Main_AutoLevel: CFStringRef;
    pub(super) static kVTProfileLevel_HEVC_Main10_AutoLevel: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_DataRateLimits: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_MaxFrameDelayCount: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_ColorPrimaries: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_TransferFunction: CFStringRef;
    pub(super) static kVTCompressionPropertyKey_YCbCrMatrix: CFStringRef;
    pub(super) static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;

    pub(super) fn VTIsHardwareDecodeSupported(codec_type: u32) -> u8;
    pub(super) fn VTSessionSetProperty(session: Ref, key: CFStringRef, value: Ref) -> i32;
    pub(super) fn VTDecompressionSessionCreate(
        allocator: Ref,
        description: Ref,
        specification: Ref,
        image_attributes: Ref,
        callbacks: *const DecodeCallbacks,
        result: *mut Ref,
    ) -> i32;
    pub(super) fn VTDecompressionSessionDecodeFrame(
        session: Ref,
        sample: Ref,
        flags: u32,
        source: *mut c_void,
        info: *mut u32,
    ) -> i32;
    pub(super) fn VTDecompressionSessionInvalidate(session: Ref);

    pub(super) fn VTCompressionSessionCreate(
        allocator: Ref,
        width: i32,
        height: i32,
        codec: u32,
        specification: Ref,
        attributes: Ref,
        compressed_allocator: Ref,
        callback: EncodeCallback,
        context: *mut c_void,
        result: *mut Ref,
    ) -> i32;
    pub(super) fn VTCompressionSessionPrepareToEncodeFrames(session: Ref) -> i32;
    pub(super) fn VTCompressionSessionEncodeFrame(
        session: Ref,
        image: Ref,
        presentation: Time,
        duration: Time,
        properties: Ref,
        frame_context: *mut c_void,
        info: *mut u32,
    ) -> i32;
    pub(super) fn VTCompressionSessionCompleteFrames(session: Ref, until: Time) -> i32;
    pub(super) fn VTCompressionSessionInvalidate(session: Ref);
}

pub(super) fn hdr_color_extension_pairs() -> Vec<(CFString, CFType)> {
    // SAFETY: These CoreVideo colorimetry keys and values are process-lifetime CFStringRef constants.
    unsafe {
        vec![
            (
                CFString::wrap_under_get_rule(kCVImageBufferColorPrimariesKey),
                CFString::wrap_under_get_rule(kCVImageBufferColorPrimaries_ITU_R_2020).as_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kCVImageBufferTransferFunctionKey),
                CFString::wrap_under_get_rule(kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ)
                    .as_CFType(),
            ),
            (
                CFString::wrap_under_get_rule(kCVImageBufferYCbCrMatrixKey),
                CFString::wrap_under_get_rule(kCVImageBufferYCbCrMatrix_ITU_R_2020).as_CFType(),
            ),
        ]
    }
}
