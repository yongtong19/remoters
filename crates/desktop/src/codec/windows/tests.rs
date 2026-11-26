//! Windows codec unit tests and hardware probes (MFT VBV/PTS, direct NVENC,
//! AV1 hardware encode/decode, and HDR10 P010 VideoProcessor roundtrip).

use std::{mem::ManuallyDrop, time::Duration};

use gpui::ID3D11Texture2D;
use remoters_core::video::{AccessUnit, Codec};
use windows::Win32::{
    Foundation::TRUE,
    Graphics::{
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
            D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_STREAM,
            D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VPIV_DIMENSION_TEXTURE2D,
            D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11VideoContext1,
        },
        Dxgi::Common::{
            DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
        },
    },
};
use windows_core::Interface;

use super::{
    common::{
        DXGI_CS_RGB_FULL_G10_NONE_P709, DXGI_CS_RGB_FULL_G22_NONE_P709,
        DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020, DXGI_FORMAT_P010, DXGI_FORMAT_R16G16B16A16_FLOAT,
        VideoDevice, supports_av1_hardware_decoder, supports_av1_hardware_encoder,
    },
    d3d_convert::{
        Fp16ShaderConverter, SurfaceConverter, compute_fp16_uv_scale,
        compute_video_processor_rects, create_p010_rtvs, video_processor_supports_input,
    },
    mft_encoder::{mft_max_bitrate, mft_vbv_buffer_bytes},
    nvenc::NvencEncoder,
    pipeline::{STATIC_KEEPALIVE_INTERVAL, next_clamped_sample_time},
};

#[test]
fn mft_vbv_and_max_bitrate_stay_tight() {
    // 20 Mbps @ 60 fps => (20M / 60) * 2 / 8 = 83,333 bytes (vs old 625 KB 250ms VBV)
    assert_eq!(mft_vbv_buffer_bytes(20_000_000, 60), 83_333);
    // 50 Mbps @ 60 fps => 208,333 bytes
    assert_eq!(mft_vbv_buffer_bytes(50_000_000, 60), 208_333);
    // Floor clamp at 48 KiB
    assert_eq!(mft_vbv_buffer_bytes(5_000_000, 120), 48 * 1024);
    // Ceiling clamp at 384 KiB
    assert_eq!(mft_vbv_buffer_bytes(150_000_000, 60), 384 * 1024);
    // Peak ratio tightened from 1.5x to 1.2x
    assert_eq!(mft_max_bitrate(20_000_000), 24_000_000);
    assert_eq!(mft_max_bitrate(50_000_000), 60_000_000);
    assert_eq!(STATIC_KEEPALIVE_INTERVAL, Duration::from_millis(200));
}

#[test]
fn clamped_sample_time_prevents_static_gap_bursts() {
    let dur = 10_000_000 / 60; // 166,666 (16.67ms in 100ns)
    let t0 = next_clamped_sample_time(1_000_000, None, -1, dur);
    assert_eq!(t0, 0);
    let t1 = next_clamped_sample_time(1_000_000 + dur, Some(1_000_000), t0, dur);
    assert_eq!(t1, dur);
    let after_silence = 1_000_000 + dur + 50_000_000;
    let t2 = next_clamped_sample_time(after_silence, Some(1_000_000 + dur), t1, dur);
    assert_eq!(t2, t1 + 2 * dur);
    let t3 = next_clamped_sample_time(after_silence + dur, Some(after_silence), t2, dur);
    assert_eq!(t3, t2 + dur);
    let t4 = next_clamped_sample_time(after_silence, Some(after_silence + dur), t3, dur);
    assert_eq!(t4, t3 + dur);
}

#[test]
fn nvenc_direct_hardware_encode_probe() {
    let Ok(gpu) = VideoDevice::new(None) else {
        println!("Skipping NVENC probe: no hardware D3D11 video device.");
        return;
    };
    let (width, height, fps) = (1280u32, 720u32, 60u32);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
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
    let mut tex = None;
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }.unwrap();
    let tex = tex.unwrap();

    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        match NvencEncoder::new(
            &gpu.device,
            codec,
            false,
            width,
            height,
            fps,
            20_000_000,
            true,
        ) {
            Ok(mut enc) => {
                let mut got_idr = false;
                enc.encode_texture(&tex, DXGI_FORMAT_NV12.0, 0, 166_666, true, |bytes, _| {
                    let unit = AccessUnit::from_annex_b(codec, 0, bytes.to_vec().into())
                        .expect("NVENC IDR access unit should parse cleanly");
                    assert!(
                        unit.is_random_access(),
                        "forced IDR should be random-access"
                    );
                    got_idr = true;
                })
                .expect("NVENC encode_texture(IDR) should succeed");
                assert!(got_idr, "expected IDR output bytes from NVENC {codec:?}");
                enc.set_bitrate(30_000_000)
                    .expect("NVENC live set_bitrate reconfigure should succeed");
                let mut got_p = false;
                enc.encode_texture(
                    &tex,
                    DXGI_FORMAT_NV12.0,
                    166_666,
                    166_666,
                    false,
                    |bytes, _| {
                        let _ = AccessUnit::from_annex_b(codec, 1, bytes.to_vec().into())
                            .expect("NVENC P-frame access unit should parse cleanly");
                        got_p = true;
                    },
                )
                .expect("NVENC encode_texture(P) should succeed");
                assert!(got_p, "expected P-frame output bytes from NVENC {codec:?}");
                println!("NVENC direct hardware encode probe succeeded for {codec:?}.");
            }
            Err(err) => {
                println!("NVENC probe skipped/unavailable for {codec:?}: {err:#}");
            }
        }
    }
}

#[test]
fn av1_hardware_encoder_probe_on_windows() {
    let enc = supports_av1_hardware_encoder();
    let dec = supports_av1_hardware_decoder();
    println!("Windows AV1 probe: encode={enc} decode={dec}");
}

#[test]
fn hdr_p010_video_processor_probe() {
    let Ok(gpu) = VideoDevice::new(None) else {
        println!("Skipping D3D11 VideoProcessor probe: no hardware D3D11 video device.");
        return;
    };
    let (width, height) = (1280u32, 720u32);
    let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputWidth: width,
        InputHeight: height,
        OutputWidth: width,
        OutputHeight: height,
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        ..Default::default()
    };
    let enumerator = unsafe { gpu.video.CreateVideoProcessorEnumerator(&content) }
        .expect("CreateVideoProcessorEnumerator should succeed");
    let processor = unsafe { gpu.video.CreateVideoProcessor(&enumerator, 0) }
        .expect("CreateVideoProcessor should succeed");
    let ctx1: ID3D11VideoContext1 = gpu
        .video_context
        .cast()
        .expect("ID3D11VideoContext1 should be supported on Windows");

    let make_tex = |format: DXGI_FORMAT, bind: u32| -> ID3D11Texture2D {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BindFlags: bind,
            ..Default::default()
        };
        let mut tex = None;
        unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }.unwrap();
        tex.unwrap()
    };

    let fp16 = make_tex(
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
    );
    let p010 = make_tex(DXGI_FORMAT_P010, D3D11_BIND_RENDER_TARGET.0 as u32);
    let bgra = make_tex(
        DXGI_FORMAT_B8G8R8A8_UNORM,
        (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
    );

    let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        ..Default::default()
    };
    let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        ..Default::default()
    };

    let fp16_vp_supported =
        video_processor_supports_input(&enumerator, DXGI_FORMAT_R16G16B16A16_FLOAT);
    if fp16_vp_supported {
        let mut fp16_in = None;
        unsafe {
            gpu.video.CreateVideoProcessorInputView(
                &fp16,
                &enumerator,
                &in_desc,
                Some(&mut fp16_in),
            )
        }
        .unwrap();
        let mut p010_out = None;
        unsafe {
            gpu.video.CreateVideoProcessorOutputView(
                &p010,
                &enumerator,
                &out_desc,
                Some(&mut p010_out),
            )
        }
        .unwrap();
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(fp16_in),
            ..Default::default()
        }];
        unsafe {
            ctx1.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_CS_RGB_FULL_G10_NONE_P709);
            ctx1.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020,
            );
            let blt = gpu.video_context.VideoProcessorBlt(
                &processor,
                p010_out.as_ref().unwrap(),
                0,
                &streams,
            );
            let _ = ManuallyDrop::take(&mut streams[0].pInputSurface);
            blt.expect("FP16 scRGB -> P010 HDR10 VideoProcessorBlt should succeed");
        }
    } else {
        let mut source_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { fp16.GetDesc(&mut source_desc) };
        let (rtv_y, rtv_uv) =
            create_p010_rtvs(&gpu.device, &p010).expect("P010 RTVs should be supported");
        let mut converter = Fp16ShaderConverter::new(&gpu.device)
            .expect("Fp16ShaderConverter should compile HLSL shaders");
        converter
            .convert_to_p010(
                &gpu.device,
                &gpu.context,
                &fp16,
                &source_desc,
                width as i32,
                height as i32,
                &rtv_y,
                &rtv_uv,
                width,
                height,
            )
            .expect("FP16 scRGB -> P010 HDR10 pixel shader pass should succeed");
    }

    let mut p010_in = None;
    unsafe {
        gpu.video
            .CreateVideoProcessorInputView(&p010, &enumerator, &in_desc, Some(&mut p010_in))
    }
    .unwrap();
    let mut bgra_out = None;
    unsafe {
        gpu.video
            .CreateVideoProcessorOutputView(&bgra, &enumerator, &out_desc, Some(&mut bgra_out))
    }
    .unwrap();
    let mut tonemap_streams = [D3D11_VIDEO_PROCESSOR_STREAM {
        Enable: TRUE,
        pInputSurface: ManuallyDrop::new(p010_in),
        ..Default::default()
    }];
    unsafe {
        ctx1.VideoProcessorSetStreamColorSpace1(
            &processor,
            0,
            DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020,
        );
        ctx1.VideoProcessorSetOutputColorSpace1(&processor, DXGI_CS_RGB_FULL_G22_NONE_P709);
        let blt = gpu.video_context.VideoProcessorBlt(
            &processor,
            bgra_out.as_ref().unwrap(),
            0,
            &tonemap_streams,
        );
        let _ = ManuallyDrop::take(&mut tonemap_streams[0].pInputSurface);
        blt.expect("P010 HDR10 -> BGRA8 SDR VideoProcessorBlt should succeed");
        gpu.context.Flush();
    }
    println!("HDR P010 VideoProcessor roundtrip probe succeeded ({width}x{height}).");
}

#[test]
fn video_processor_rects_scale_full_frame_without_cropping() {
    // 4K capture -> 1080p encode must map full 3840x2160 source to full 1920x1080 dest/target.
    let (src, dst, tgt) = compute_video_processor_rects(3840, 2160, 3840, 2160, 1920, 1080);
    assert_eq!(
        (src.left, src.top, src.right, src.bottom),
        (0, 0, 3840, 2160)
    );
    assert_eq!(
        (dst.left, dst.top, dst.right, dst.bottom),
        (0, 0, 1920, 1080)
    );
    assert_eq!(
        (tgt.left, tgt.top, tgt.right, tgt.bottom),
        (0, 0, 1920, 1080)
    );

    // Non-16:9 native mobile resolutions map 1:1 with zero cropping or distortion.
    for (w, h) in [(2400u32, 1080u32), (2640, 1200), (2772, 1240), (2560, 1152)] {
        let (src, dst, tgt) = compute_video_processor_rects(w, h, w as i32, h as i32, w, h);
        assert_eq!((src.right, src.bottom), (w as i32, h as i32));
        assert_eq!((dst.right, dst.bottom), (w as i32, h as i32));
        assert_eq!((tgt.right, tgt.bottom), (w as i32, h as i32));
    }

    // Defensive clamping: non-positive or oversized source dims fall back to full texture bounds.
    let (src_zero, dst_zero, _) = compute_video_processor_rects(3840, 2160, 0, -1, 1920, 1080);
    assert_eq!((src_zero.right, src_zero.bottom), (3840, 2160));
    assert_eq!((dst_zero.right, dst_zero.bottom), (1920, 1080));

    let (src_over, _, _) = compute_video_processor_rects(2400, 1080, 9999, 9999, 2400, 1080);
    assert_eq!((src_over.right, src_over.bottom), (2400, 1080));

    assert_eq!(compute_fp16_uv_scale(3840, 2160, 3840, 2160), (1.0, 1.0));
    assert_eq!(compute_fp16_uv_scale(3840, 2160, 0, 0), (1.0, 1.0));
    assert_eq!(compute_fp16_uv_scale(2400, 1080, 1200, 540), (0.5, 0.5));
}

#[test]
fn surface_converter_4k_to_1080p_scales_all_four_quadrants_without_crop() {
    let Ok(gpu) = VideoDevice::new(None) else {
        println!("Skipping 4K->1080p GPU scaling test: no hardware D3D11 video device.");
        return;
    };

    // Build a 3840x2160 BGRA8 test frame with 4 distinct luminance quadrants:
    //   TL = 250 (bright white), TR = 60 (dark gray),
    //   BL = 175 (light gray),   BR = 10 (near black).
    // If VideoProcessorBlt cropped instead of scaling, all 4 quadrants of the
    // 1920x1080 output would read ~250 (only the top-left 4K quadrant).
    let (src_w, src_h) = (3840u32, 2160u32);
    let (out_w, out_h) = (1920u32, 1080u32);
    let mut pixels = vec![0u8; (src_w * src_h * 4) as usize];
    for y in 0..src_h {
        let bottom = y >= src_h / 2;
        let row_offset = (y * src_w * 4) as usize;
        for x in 0..src_w {
            let right = x >= src_w / 2;
            let v = match (bottom, right) {
                (false, false) => 250u8,
                (false, true) => 60u8,
                (true, false) => 175u8,
                (true, true) => 10u8,
            };
            let idx = row_offset + (x * 4) as usize;
            pixels[idx] = v;
            pixels[idx + 1] = v;
            pixels[idx + 2] = v;
            pixels[idx + 3] = 255;
        }
    }

    let src_desc = D3D11_TEXTURE2D_DESC {
        Width: src_w,
        Height: src_h,
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
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr().cast(),
        SysMemPitch: src_w * 4,
        SysMemSlicePitch: 0,
    };
    let mut src_tex = None;
    unsafe {
        gpu.device
            .CreateTexture2D(&src_desc, Some(&init), Some(&mut src_tex))
    }
    .unwrap();
    let src_tex = src_tex.unwrap();

    let mut converter = SurfaceConverter::new(out_w, out_h, false);
    let slot_idx = converter
        .convert(&gpu, &src_tex, src_w as i32, src_h as i32, || {})
        .expect("4K->1080p SurfaceConverter::convert should succeed");
    let nv12_tex = converter.texture(slot_idx);

    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: out_w,
        Height: out_h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        ..Default::default()
    };
    let mut staging = None;
    unsafe {
        gpu.device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
    }
    .unwrap();
    let staging = staging.unwrap();

    unsafe {
        gpu.context.CopyResource(&staging, &nv12_tex);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .expect("Map NV12 staging texture should succeed");
        let pitch = mapped.RowPitch as usize;
        let data = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * out_h as usize);
        let sample_y = |x: u32, y: u32| data[y as usize * pitch + x as usize];
        let tl = sample_y(out_w / 4, out_h / 4);
        let tr = sample_y(out_w * 3 / 4, out_h / 4);
        let bl = sample_y(out_w / 4, out_h * 3 / 4);
        let br = sample_y(out_w * 3 / 4, out_h * 3 / 4);
        gpu.context.Unmap(&staging, 0);

        assert!(tl >= 210, "expected bright TL quadrant, got Y={tl}");
        assert!(
            (30..=105).contains(&tr),
            "expected mid-dark TR quadrant (no 4K crop!), got Y={tr}"
        );
        assert!(
            (135..=210).contains(&bl),
            "expected mid-bright BL quadrant (no 4K crop!), got Y={bl}"
        );
        assert!(
            br <= 45,
            "expected dark BR quadrant (no 4K crop!), got Y={br}"
        );
        println!(
            "4K->1080p full-frame scaling verified (Y quadrants: TL={tl} TR={tr} BL={bl} BR={br})."
        );
    }
}

#[test]
fn nvenc_direct_hardware_encode_supports_non_16_9_mobile_resolutions() {
    let Ok(gpu) = VideoDevice::new(None) else {
        println!("Skipping non-16:9 NVENC probe: no hardware D3D11 video device.");
        return;
    };
    for (width, height) in [(2400u32, 1080u32), (2640, 1200), (2772, 1240)] {
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
        let mut src_tex = None;
        unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut src_tex)) }.unwrap();
        let src_tex = src_tex.unwrap();

        let mut converter = SurfaceConverter::new(width, height, false);
        let idx = converter
            .convert(&gpu, &src_tex, width as i32, height as i32, || {})
            .expect("non-16:9 SurfaceConverter should succeed");
        let nv12 = converter.texture(idx);

        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            match NvencEncoder::new(
                &gpu.device,
                codec,
                false,
                width,
                height,
                60,
                25_000_000,
                false,
            ) {
                Ok(mut enc) => {
                    let mut got_idr = false;
                    enc.encode_texture(&nv12, DXGI_FORMAT_NV12.0, 0, 166_666, true, |bytes, _| {
                        let unit = AccessUnit::from_annex_b(codec, 0, bytes.to_vec().into())
                            .expect("non-16:9 NVENC IDR access unit should parse cleanly");
                        assert!(unit.is_random_access());
                        got_idr = true;
                    })
                    .expect("non-16:9 NVENC encode should succeed");
                    assert!(got_idr, "expected IDR bytes for {codec:?} {width}x{height}");
                }
                Err(err) => {
                    println!("Non-16:9 NVENC {codec:?} {width}x{height} skipped: {err:#}");
                }
            }
        }
    }
}
