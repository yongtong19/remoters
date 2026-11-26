//! In-process VideoToolbox hardware video decoder (H.264, HEVC, and AV1) on macOS.

use std::{
    ffi::c_void,
    ptr::{null, null_mut},
    sync::OnceLock,
};

use anyhow::{Context, Result, anyhow, ensure};
use core_foundation::{
    base::{CFType, TCFType},
    boolean::CFBoolean,
    data::CFData,
    dictionary::CFDictionary,
    number::CFNumber,
    string::CFString,
};
use core_video::pixel_buffer::{
    CVPixelBufferKeys, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
};
use remoters_core::video::{Codec, Format, ReceivedFrame, nal_units, obu};
use screencapturekit::cv::CVPixelBuffer;

use super::ffi::{
    CMBlockBufferCreateWithMemoryBlock, CMBlockBufferReplaceDataBytes, CMSampleBufferCreateReady,
    CMVideoFormatDescriptionCreate, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, DecodeCallbacks, Owned, Ref, Time, Timing,
    VTDecompressionSessionCreate, VTDecompressionSessionDecodeFrame,
    VTDecompressionSessionInvalidate, VTIsHardwareDecodeSupported, VTSessionSetProperty, check,
    hdr_color_extension_pairs, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms,
    kVTDecompressionPropertyKey_RealTime,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};
use crate::platform::VideoFrame;

/// Returns `true` when Apple VideoToolbox reports hardware-accelerated AV1 decode support.
pub fn supports_av1_hardware_decoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        // SAFETY: VTIsHardwareDecodeSupported is thread-safe and takes a 4CC codec type.
        unsafe { VTIsHardwareDecodeSupported(u32::from_be_bytes(*b"av01")) != 0 }
    })
}

pub(super) fn create_av1_format_description(
    av1c: &[u8],
    width: u16,
    height: u16,
    hdr: bool,
) -> Result<Owned> {
    let atoms = CFDictionary::from_CFType_pairs(&[(
        CFString::new("av1C"),
        CFData::from_buffer(av1c).as_CFType(),
    )]);
    let mut pairs = vec![(
        // SAFETY: Process-lifetime CoreMedia constant.
        unsafe {
            CFString::wrap_under_get_rule(
                kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms,
            )
        },
        atoms.as_CFType(),
    )];
    if hdr {
        pairs.extend(hdr_color_extension_pairs());
    }
    let extensions = CFDictionary::from_CFType_pairs(&pairs);
    Owned::create("Creating the AV1 video format", |out| {
        // SAFETY: `extensions` is a valid CFDictionary retained through the call.
        unsafe {
            CMVideoFormatDescriptionCreate(
                null(),
                u32::from_be_bytes(*b"av01"),
                i32::from(width),
                i32::from(height),
                extensions.as_CFTypeRef().cast(),
                out,
            )
        }
    })
}

pub(super) struct DecodeSession {
    session: Owned,
    description: Owned,
}

impl DecodeSession {
    fn new(codec: Codec, parameters: &[Vec<u8>], hdr: bool) -> Result<Self> {
        let reference = parameters
            .iter()
            .map(|nal| nal.as_ptr())
            .collect::<Vec<_>>();
        let sizes = parameters.iter().map(Vec::len).collect::<Vec<_>>();
        let hdr_extensions = (hdr && codec == Codec::Hevc)
            .then(|| CFDictionary::from_CFType_pairs(&hdr_color_extension_pairs()));
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
                        hdr_extensions
                            .as_ref()
                            .map_or(null(), |dict| dict.as_CFTypeRef().cast()),
                        out,
                    ),
                    Codec::Av1 => {
                        debug_assert!(false, "AV1 uses DecodeSession::new_av1");
                        -1
                    }
                }
            }
        })?;
        Self::from_description(description, hdr)
    }

    pub(super) fn new_av1(av1c: &[u8], width: u16, height: u16, hdr: bool) -> Result<Self> {
        let description = create_av1_format_description(av1c, width, height, hdr)?;
        Self::from_description(description, hdr)
    }

    fn from_description(description: Owned, hdr: bool) -> Result<Self> {
        let iosurface = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[]);
        let required_hardware =
            // SAFETY: This constant is a process-lifetime Core Foundation string.
            unsafe { CFString::wrap_under_get_rule(kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder) };
        let hardware = CFDictionary::from_CFType_pairs(&[(
            required_hardware,
            CFBoolean::true_value().as_CFType(),
        )]);
        let pixel_format = if hdr {
            kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
        } else {
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        };
        let attrs = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
            (
                CVPixelBufferKeys::PixelFormatType.into(),
                CFNumber::from(i64::from(pixel_format)).as_CFType(),
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
        let pixel_format = surface.pixel_format();
        ensure!(
            [
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
                kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
            ]
            .contains(&pixel_format),
            "VideoToolbox did not return a GPU-compatible NV12/P010 surface ({pixel_format:#x}).",
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
    format: Option<Format>,
    parameters: Vec<Vec<u8>>,
    av1c: Vec<u8>,
    sample: Vec<u8>,
}

impl Decoder {
    /// Creates a decoder pre-configured with negotiated stream format metadata (dimensions + HDR).
    pub fn with_format(format: Format) -> Self {
        Self {
            format: Some(format),
            ..Self::default()
        }
    }

    pub fn decode(&mut self, frame: &ReceivedFrame) -> Result<Option<VideoFrame>> {
        if frame.codec == Codec::Av1 {
            return self.decode_av1(frame);
        }
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
                    kind @ 0..=31 => (None, true, matches!(kind, 19 | 20 | 21)),
                    _ => (None, false, false),
                },
                Codec::Av1 => unreachable!(),
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
            Codec::Av1 => unreachable!(),
        };
        let complete = parameters[..count]
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>();
        let hdr = self.format.is_some_and(|fmt| fmt.hdr) && frame.codec == Codec::Hevc;
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
            self.session = Some(DecodeSession::new(frame.codec, &next, hdr)?);
            self.codec = Some(frame.codec);
            self.parameters = next;
            self.av1c.clear();
        }
        if self.codec != Some(frame.codec) {
            return Ok(None);
        }
        self.session
            .as_ref()
            .context("The hardware decoder is unavailable.")?
            .decode(&self.sample, frame.rtp_timestamp)
    }

    fn decode_av1(&mut self, frame: &ReceivedFrame) -> Result<Option<VideoFrame>> {
        let is_keyframe = obu::inspect_av1(&frame.bytes)?;
        if is_keyframe {
            let seq = obu::extract_sequence_header(&frame.bytes)
                .context("AV1 keyframe is missing a sequence header OBU.")?;
            let av1c = obu::build_av1c(seq)?;
            if self.codec != Some(Codec::Av1) || self.av1c != av1c {
                let format = self
                    .format
                    .context("AV1 decoding requires stream format metadata.")?;
                self.session = Some(DecodeSession::new_av1(
                    &av1c,
                    format.width,
                    format.height,
                    format.hdr,
                )?);
                self.codec = Some(Codec::Av1);
                self.av1c = av1c;
                self.parameters.clear();
            }
        }
        if self.codec != Some(Codec::Av1) {
            return Ok(None);
        }
        self.session
            .as_ref()
            .context("The hardware decoder is unavailable.")?
            .decode(&frame.bytes, frame.rtp_timestamp)
    }
}
