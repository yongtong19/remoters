//! In-process VideoToolbox codec sessions on macOS.
use std::{
    ffi::c_void,
    ptr::{null, null_mut},
};

use anyhow::{Context, Result, anyhow, ensure};
use core_foundation::{
    base::{CFRelease, CFType, TCFType},
    boolean::CFBoolean,
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use core_video::pixel_buffer::{CVPixelBufferKeys, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange};
use remoters_core::video::{Codec, Format, ReceivedFrame, nal_units};
use screencapturekit::cv::CVPixelBuffer;

use crate::platform::VideoFrame;

type Ref = *const c_void;
const RTP_CLOCK: i32 = 90_000;

#[repr(C)]
#[derive(Clone, Copy)]
struct Time {
    value: i64,
    scale: i32,
    flags: u32,
    epoch: i64,
}

impl Time {
    const INVALID: Self = Self {
        value: 0,
        scale: 0,
        flags: 0,
        epoch: 0,
    };

    fn ticks(value: i64) -> Self {
        Self {
            value,
            scale: RTP_CLOCK,
            flags: 1,
            epoch: 0,
        }
    }
}

#[repr(C)]
struct Timing {
    duration: Time,
    presentation: Time,
    decode: Time,
}

struct Owned(Ref);

impl Owned {
    fn create(label: &str, start: impl FnOnce(&mut Ref) -> i32) -> Result<Self> {
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

fn check(status: i32, label: &str) -> Result<()> {
    ensure!(
        status == 0,
        "{label} returned VideoToolbox status {status}."
    );
    Ok(())
}

type DecodeCallback = extern "C" fn(*mut c_void, *mut c_void, i32, u32, Ref, Time, Time);

#[repr(C)]
struct DecodeCallbacks {
    callback: DecodeCallback,
    context: *mut c_void,
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: Ref,
        count: usize,
        parameters: *const *const u8,
        lengths: *const usize,
        header_length: i32,
        result: *mut Ref,
    ) -> i32;
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: Ref,
        count: usize,
        parameters: *const *const u8,
        lengths: *const usize,
        header_length: i32,
        extensions: Ref,
        result: *mut Ref,
    ) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(
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
    fn CMBlockBufferReplaceDataBytes(
        source: *const u8,
        block: Ref,
        offset: usize,
        length: usize,
    ) -> i32;
    fn CMSampleBufferCreateReady(
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
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    static kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: CFStringRef;
    static kVTDecompressionPropertyKey_RealTime: CFStringRef;
    fn VTSessionSetProperty(session: Ref, key: CFStringRef, value: Ref) -> i32;
    fn VTDecompressionSessionCreate(
        allocator: Ref,
        description: Ref,
        specification: Ref,
        image_attributes: Ref,
        callbacks: *const DecodeCallbacks,
        result: *mut Ref,
    ) -> i32;
    fn VTDecompressionSessionDecodeFrame(
        session: Ref,
        sample: Ref,
        flags: u32,
        source: *mut c_void,
        info: *mut u32,
    ) -> i32;
    fn VTDecompressionSessionInvalidate(session: Ref);
}

struct DecodeSession {
    session: Owned,
    description: Owned,
}

impl DecodeSession {
    fn new(codec: Codec, parameters: &[Vec<u8>]) -> Result<Self> {
        let reference = parameters
            .iter()
            .map(|nal| nal.as_ptr())
            .collect::<Vec<_>>();
        let sizes = parameters.iter().map(Vec::len).collect::<Vec<_>>();
        let description = Owned::create("Creating the video format", |out| {
            // SAFETY: Both arrays have one valid entry per parameter and live through the call.
            unsafe {
                match codec {
                    Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                        null(),
                        reference.len(),
                        reference.as_ptr(),
                        sizes.as_ptr(),
                        4,
                        out,
                    ),
                    Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                        null(),
                        reference.len(),
                        reference.as_ptr(),
                        sizes.as_ptr(),
                        4,
                        null(),
                        out,
                    ),
                }
            }
        })?;
        let iosurface = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[]);
        let required_hardware =
            // SAFETY: This constant is a process-lifetime Core Foundation string.
            unsafe { CFString::wrap_under_get_rule(kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder) };
        let hardware = CFDictionary::from_CFType_pairs(&[(
            required_hardware,
            CFBoolean::true_value().as_CFType(),
        )]);
        let attrs = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
            (
                CVPixelBufferKeys::PixelFormatType.into(),
                CFNumber::from(i64::from(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange))
                    .as_CFType(),
            ),
            (
                CVPixelBufferKeys::MetalCompatibility.into(),
                CFBoolean::true_value().as_CFType(),
            ),
            (
                CVPixelBufferKeys::IOSurfaceProperties.into(),
                iosurface.as_CFType(),
            ),
        ]);
        let callbacks = DecodeCallbacks {
            callback: decoded,
            context: null_mut(),
        };
        let result = Self {
            session: Owned::create("Opening the hardware video decoder", |out| {
                // SAFETY: VideoToolbox copies the callback record and retains its configuration.
                unsafe {
                    VTDecompressionSessionCreate(
                        null(),
                        description.0,
                        hardware.as_CFTypeRef().cast(),
                        attrs.as_CFTypeRef().cast(),
                        &callbacks,
                        out,
                    )
                }
            })?,
            description,
        };
        // SAFETY: The session and property value are live for the call.
        unsafe {
            VTSessionSetProperty(
                result.session.0,
                kVTDecompressionPropertyKey_RealTime,
                CFBoolean::true_value().as_CFTypeRef().cast(),
            );
        }
        Ok(result)
    }

    fn decode(&self, bytes: &[u8], timestamp: u64) -> Result<Option<VideoFrame>> {
        let length = bytes.len();
        let block = Owned::create("Allocating a compressed video sample", |out| {
            // SAFETY: A null block requests Core Media-owned storage of the given size.
            unsafe {
                CMBlockBufferCreateWithMemoryBlock(
                    null(),
                    null_mut(),
                    length,
                    null(),
                    null(),
                    0,
                    length,
                    0,
                    out,
                )
            }
        })?;
        // SAFETY: The allocated buffer is exactly large enough for the compressed bytes.
        check(
            unsafe { CMBlockBufferReplaceDataBytes(bytes.as_ptr(), block.0, 0, length) },
            "Filling the compressed video sample",
        )?;
        let timing = Timing {
            duration: Time::INVALID,
            presentation: Time::ticks(i64::try_from(timestamp)?),
            decode: Time::INVALID,
        };
        let sample = Owned::create("Creating a compressed video sample", |out| {
            // SAFETY: The sample retains its block and format. Timing and size are copied.
            unsafe {
                CMSampleBufferCreateReady(
                    null(),
                    block.0,
                    self.description.0,
                    1,
                    1,
                    &timing,
                    1,
                    &length,
                    out,
                )
            }
        })?;
        let mut output = None::<Result<CVPixelBuffer>>;
        // SAFETY: With both asynchronous and temporal processing flags clear, the
        // callback completes before this call returns. The output slot remains live.
        let status = unsafe {
            VTDecompressionSessionDecodeFrame(
                self.session.0,
                sample.0,
                0,
                std::ptr::from_mut(&mut output).cast(),
                null_mut(),
            )
        };
        check(status, "Submitting a compressed video frame")?;
        let Some(surface) = output.transpose()? else {
            return Ok(None);
        };
        let width = u32::try_from(surface.width())?;
        let height = u32::try_from(surface.height())?;
        ensure!(
            Format::supports_dimensions(width, height),
            "Unsupported decoded video dimensions."
        );
        ensure!(
            surface.pixel_format() == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            "VideoToolbox did not return a GPU-compatible NV12 surface."
        );
        Ok(Some(VideoFrame {
            surface,
            width,
            height,
        }))
    }
}

impl Drop for DecodeSession {
    fn drop(&mut self) {
        // SAFETY: The session is retained in this object and every submission is synchronous.
        unsafe { VTDecompressionSessionInvalidate(self.session.0) };
    }
}

extern "C" fn decoded(
    _: *mut c_void,
    source: *mut c_void,
    status: i32,
    _: u32,
    image: Ref,
    _: Time,
    _: Time,
) {
    if source.is_null() {
        return;
    }
    // SAFETY: The synchronous submitter supplied this slot. Retain the image before
    // returning from the callback so Core Video can keep it until the GPU is done.
    let output = unsafe { &mut *source.cast::<Option<Result<CVPixelBuffer>>>() };
    if status != 0 {
        *output = Some(Err(anyhow!(
            "Hardware video decoder returned status {status}."
        )));
    } else if let Some(image) = unsafe { CVPixelBuffer::from_raw_borrowed(image.cast_mut()) } {
        *output = Some(Ok(image));
    }
}

#[derive(Default)]
pub struct Decoder {
    session: Option<DecodeSession>,
    codec: Option<Codec>,
    parameters: Vec<Vec<u8>>,
    sample: Vec<u8>,
}

impl Decoder {
    pub fn decode(&mut self, frame: &ReceivedFrame) -> Result<Option<VideoFrame>> {
        let mut parameters = [None, None, None];
        let mut picture = false;
        let mut keyframe = false;
        self.sample.clear();
        for nal in nal_units(&frame.bytes) {
            let (index, video, idr) = match frame.codec {
                Codec::H264 => match nal[0] & 0x1f {
                    7 => (Some(0), false, false),
                    8 => (Some(1), false, false),
                    kind @ 1..=5 => (None, true, kind == 5),
                    _ => (None, false, false),
                },
                Codec::Hevc => match (nal[0] >> 1) & 0x3f {
                    kind @ 32..=34 => (Some(usize::from(kind - 32)), false, false),
                    kind @ 0..=31 => (None, true, matches!(kind, 19 | 20)),
                    _ => (None, false, false),
                },
            };
            if let Some(index) = index {
                parameters[index] = Some(nal);
            } else {
                self.sample
                    .extend_from_slice(&u32::try_from(nal.len())?.to_be_bytes());
                self.sample.extend_from_slice(nal);
            }
            picture |= video;
            keyframe |= idr;
        }
        if !picture {
            return Ok(None);
        }
        let count = match frame.codec {
            Codec::H264 => 2,
            Codec::Hevc => 3,
        };
        let complete = parameters[..count]
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>();
        if keyframe
            && let Some(complete) = complete
            && (self.codec != Some(frame.codec)
                || !self
                    .parameters
                    .iter()
                    .map(Vec::as_slice)
                    .eq(complete.iter().copied()))
        {
            let next = complete.into_iter().map(<[u8]>::to_vec).collect::<Vec<_>>();
            self.session = Some(DecodeSession::new(frame.codec, &next)?);
            self.codec = Some(frame.codec);
            self.parameters = next;
        }
        if self.codec != Some(frame.codec) {
            return Ok(None);
        }
        self.session
            .as_ref()
            .context("The hardware decoder is unavailable.")?
            .decode(&self.sample, frame.rtp_timestamp)
    }
}

// Hardware encoder, using the IOSurface supplied by ScreenCaptureKit.
type EncodeCallback = extern "C" fn(*mut c_void, *mut c_void, i32, u32, Ref);

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetDataBuffer(sample: Ref) -> Ref;
    fn CMSampleBufferGetFormatDescription(sample: Ref) -> Ref;
    fn CMBlockBufferGetDataLength(block: Ref) -> usize;
    fn CMBlockBufferCopyDataBytes(
        block: Ref,
        offset: usize,
        length: usize,
        destination: *mut u8,
    ) -> i32;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
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
    static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFStringRef;
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTProfileLevel_H264_Baseline_AutoLevel: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxFrameDelayCount: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    fn VTCompressionSessionCreate(
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
    fn VTCompressionSessionPrepareToEncodeFrames(session: Ref) -> i32;
    fn VTCompressionSessionEncodeFrame(
        session: Ref,
        image: Ref,
        presentation: Time,
        duration: Time,
        properties: Ref,
        frame_context: *mut c_void,
        info: *mut u32,
    ) -> i32;
    fn VTCompressionSessionCompleteFrames(session: Ref, until: Time) -> i32;
    fn VTCompressionSessionInvalidate(session: Ref);
}

type Encoded = std::sync::Mutex<Option<Result<Vec<u8>>>>;

pub(super) struct HardwareEncoder {
    session: Owned,
    output: Box<Encoded>,
    force_keyframe: CFDictionary<CFString, CFType>,
}

impl HardwareEncoder {
    pub(super) fn new(format: Format) -> Result<Self> {
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
        let session = Owned::create("Opening the macOS hardware encoder", |created| {
            // SAFETY: The output state is boxed and retained through session invalidation.
            unsafe {
                VTCompressionSessionCreate(
                    null(),
                    i32::from(format.width),
                    i32::from(format.height),
                    u32::from_be_bytes(*b"avc1"),
                    hardware.as_CFTypeRef().cast(),
                    null(),
                    null(),
                    encoded,
                    std::ptr::from_mut(output.as_mut()).cast(),
                    created,
                )
            }
        })?;
        let encoder = Self {
            session,
            output,
            force_keyframe,
        };
        let fps = i32::from(format.frames_per_second);
        let bitrate = super::target_bitrate(format) as i32;
        // SAFETY: Every property key and value is valid and retained through the call.
        unsafe {
            encoder.property(kVTCompressionPropertyKey_RealTime, &CFBoolean::true_value())?;
            encoder.property(
                kVTCompressionPropertyKey_AllowFrameReordering,
                &CFBoolean::false_value(),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_ProfileLevel,
                &CFString::wrap_under_get_rule(kVTProfileLevel_H264_Baseline_AutoLevel),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_AverageBitRate,
                &CFNumber::from(bitrate),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::from(fps),
            )?;
            encoder.property(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::from(fps * 300),
            )?;
            let _ = encoder.property(
                kVTCompressionPropertyKey_MaxFrameDelayCount,
                &CFNumber::from(0),
            );
            check(
                VTCompressionSessionPrepareToEncodeFrames(encoder.session.0),
                "Preparing the macOS hardware encoder",
            )?;
        }
        Ok(encoder)
    }

    unsafe fn property(&self, key: CFStringRef, value: &impl TCFType) -> Result<()> {
        // SAFETY: The caller supplies a valid property key. The value and session are live.
        check(
            unsafe { VTSessionSetProperty(self.session.0, key, value.as_CFTypeRef().cast()) },
            "Configuring the macOS hardware encoder",
        )
    }

    pub(super) fn encode(
        &self,
        surface: &CVPixelBuffer,
        timestamp: u64,
        keyframe: bool,
    ) -> Result<Option<Vec<u8>>> {
        let settings = if keyframe {
            self.force_keyframe.as_CFTypeRef().cast()
        } else {
            null()
        };
        let presentation = Time::ticks(i64::try_from(timestamp)?);
        // SAFETY: The image is alive through the encoder submission. VideoToolbox
        // retains it for any pending work, and the callback state remains live.
        unsafe {
            check(
                VTCompressionSessionEncodeFrame(
                    self.session.0,
                    surface.as_ptr().cast(),
                    presentation,
                    Time::INVALID,
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
        let mut length_field = 0;
        check(
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                description,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut length_field,
            ),
            "Reading the encoder's H.264 format",
        )?;
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
        let mut annex_b = Vec::with_capacity(size + 128);
        if nals.iter().any(|nal| nal[0] & 0x1f == 5) {
            for index in 0..2 {
                let mut data = null();
                let mut length = 0;
                check(
                    CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        description,
                        index,
                        &mut data,
                        &mut length,
                        null_mut(),
                        null_mut(),
                    ),
                    "Reading encoder parameter sets",
                )?;
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
