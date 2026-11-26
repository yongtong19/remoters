//! GPU color-space & surface conversion (D3D11 VideoProcessor + HLSL FP16 scRGB
//! shader fallback) and encoder surface pool management for NV12 SDR and P010 HDR10.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, anyhow, ensure};
use gpui::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::{
    Foundation::{RECT, TRUE},
    Graphics::{
        Direct3D::{D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_SRV_DIMENSION_TEXTURE2D, ID3DBlob},
        Direct3D11::{
            D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
            D3D11_BUFFER_DESC, D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            D3D11_RENDER_TARGET_VIEW_DESC, D3D11_RTV_DIMENSION_TEXTURE2D, D3D11_SAMPLER_DESC,
            D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_STREAM,
            D3D11_VIDEO_USAGE_PLAYBACK_NORMAL, D3D11_VIEWPORT, D3D11_VPIV_DIMENSION_TEXTURE2D,
            D3D11_VPOV_DIMENSION_TEXTURE2D, ID3D11Buffer, ID3D11DeviceContext, ID3D11PixelShader,
            ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView,
            ID3D11VertexShader, ID3D11VideoContext1, ID3D11VideoProcessor,
            ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorOutputView,
        },
        Dxgi::Common::{
            DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC,
        },
    },
};
use windows_core::{HRESULT, Interface};

use super::common::{
    BT709_FULL_RANGE, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, DXGI_CS_RGB_FULL_G10_NONE_P709,
    DXGI_CS_RGB_FULL_G22_NONE_P709, DXGI_CS_YCBCR_FULL_G22_LEFT_P709,
    DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020, DXGI_FORMAT_P010, DXGI_FORMAT_R10G10B10A2_UNORM,
    DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT, VideoDevice,
};

pub(super) struct EncodedSurfaceSlot {
    pub(super) texture: ID3D11Texture2D,
    pub(super) view: ID3D11VideoProcessorOutputView,
    pub(super) rtv_y: Option<ID3D11RenderTargetView>,
    pub(super) rtv_uv: Option<ID3D11RenderTargetView>,
}

#[link(name = "d3dcompiler")]
unsafe extern "system" {
    fn D3DCompile(
        p_src_data: *const std::ffi::c_void,
        src_data_size: usize,
        p_source_name: *const u8,
        p_defines: *const std::ffi::c_void,
        p_include: *const std::ffi::c_void,
        p_entrypoint: *const u8,
        p_target: *const u8,
        flags1: u32,
        flags2: u32,
        pp_code: *mut *mut std::ffi::c_void,
        pp_error_msgs: *mut *mut std::ffi::c_void,
    ) -> HRESULT;
}

const HDR_CONVERTER_HLSL: &str = r#"
cbuffer CropRect : register(b0) {
    float2 uv_scale;
    float2 _pad;
};
struct VSOut {
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};
VSOut vs_main(uint id : SV_VertexID) {
    VSOut o;
    float2 grid = float2((id << 1) & 2, id & 2);
    o.pos = float4(grid * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    o.uv = grid * uv_scale;
    return o;
}

Texture2D<float4> SrcTex : register(t0);
SamplerState LinearSamp : register(s0);

static const float3x3 REC709_TO_BT2020 = float3x3(
    0.627404, 0.329282, 0.043313,
    0.069097, 0.919546, 0.011357,
    0.016391, 0.055702, 0.927907
);

float3 linear_to_pq(float3 nits) {
    float3 y = saturate(nits / 10000.0);
    const float m1 = 0.1593017578125;
    const float m2 = 78.84375;
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    float3 ym1 = pow(y, m1);
    return pow((c1 + c2 * ym1) / (1.0 + c3 * ym1), m2);
}

float3 scrgb_to_bt2020_pq(float3 scrgb) {
    float3 lin709 = scrgb * 80.0;
    float3 lin2020 = max(0.0, mul(REC709_TO_BT2020, lin709));
    return linear_to_pq(lin2020);
}

float3 pq_to_ycbcr_bt2020_studio(float3 rgb) {
    float y  = 0.2627 * rgb.r + 0.6780 * rgb.g + 0.0593 * rgb.b;
    float cb = (rgb.b - y) / 1.8814;
    float cr = (rgb.r - y) / 1.4746;
    float y10  = round(clamp(64.0 + 876.0 * y, 64.0, 940.0));
    float cb10 = round(clamp(512.0 + 896.0 * cb, 64.0, 960.0));
    float cr10 = round(clamp(512.0 + 896.0 * cr, 64.0, 960.0));
    return float3(y10, cb10, cr10) * (64.0 / 65535.0);
}

float ps_y(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    float3 pq = scrgb_to_bt2020_pq(SrcTex.SampleLevel(LinearSamp, uv, 0).rgb);
    return pq_to_ycbcr_bt2020_studio(pq).x;
}

float2 ps_uv(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    float3 pq = scrgb_to_bt2020_pq(SrcTex.SampleLevel(LinearSamp, uv, 0).rgb);
    return pq_to_ycbcr_bt2020_studio(pq).yz;
}

float4 ps_sdr(float4 pos : SV_POSITION, float2 uv : TEXCOORD0) : SV_TARGET {
    float3 c = max(0.0, SrcTex.SampleLevel(LinearSamp, uv, 0).rgb);
    c = c / (1.0 + max(c.r, max(c.g, c.b)) * 0.15);
    c = saturate(c);
    float3 srgb = (c <= 0.0031308) ? (c * 12.92) : (1.055 * pow(c, 1.0 / 2.4) - 0.055);
    return float4(srgb, 1.0);
}
"#;

fn compile_hlsl(entry: &[u8], target: &[u8]) -> Result<Vec<u8>> {
    let mut code_raw: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut err_raw: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe {
        D3DCompile(
            HDR_CONVERTER_HLSL.as_ptr().cast(),
            HDR_CONVERTER_HLSL.len(),
            b"hdr_converter.hlsl\0".as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            entry.as_ptr(),
            target.as_ptr(),
            1 << 15, // D3DCOMPILE_OPTIMIZATION_LEVEL3
            0,
            &mut code_raw,
            &mut err_raw,
        )
    };
    let err_blob: Option<ID3DBlob> =
        (!err_raw.is_null()).then(|| unsafe { ID3DBlob::from_raw(err_raw) });
    if hr.is_err() {
        let msg = err_blob
            .as_ref()
            .map(|blob| unsafe {
                let ptr = blob.GetBufferPointer().cast::<u8>();
                let len = blob.GetBufferSize();
                String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned()
            })
            .unwrap_or_default();
        return Err(anyhow!(
            "D3DCompile failed ({hr:?}) for {}: {}",
            String::from_utf8_lossy(entry),
            msg.trim()
        ));
    }
    ensure!(!code_raw.is_null(), "D3DCompile returned null shader blob.");
    let code_blob: ID3DBlob = unsafe { ID3DBlob::from_raw(code_raw) };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            code_blob.GetBufferPointer().cast::<u8>(),
            code_blob.GetBufferSize(),
        )
    };
    Ok(bytes.to_vec())
}

/// Computes normalized `[0, 1]` sampling bounds for the FP16 shader converter,
/// defensively falling back to the full texture if `source_width`/`source_height`
/// are non-positive so a full desktop texture is never clipped or collapsed.
pub(super) fn compute_fp16_uv_scale(
    tex_width: u32,
    tex_height: u32,
    source_width: i32,
    source_height: i32,
) -> (f32, f32) {
    let tw = tex_width.max(1);
    let th = tex_height.max(1);
    let sw = if source_width <= 0 {
        tw
    } else {
        (source_width as u32).clamp(1, tw)
    };
    let sh = if source_height <= 0 {
        th
    } else {
        (source_height as u32).clamp(1, th)
    };
    (
        (sw as f32 / tw as f32).clamp(0.0, 1.0),
        (sh as f32 / th as f32).clamp(0.0, 1.0),
    )
}

/// Computes `(source_rect, dest_rect, output_target_rect)` for `VideoProcessorBlt`.
///
/// Always maps the full valid source frame (`0..eff_src_w, 0..eff_src_h`) to the
/// full encoder output surface (`0..output_width, 0..output_height`) so that when
/// capture dimensions differ from negotiated encode dimensions (e.g. a 3840x2160
/// display streamed at 1920x1080 or a non-16:9 native phone resolution), D3D11
/// scales the entire frame rather than cropping the top-left quadrant.
pub(super) fn compute_video_processor_rects(
    tex_width: u32,
    tex_height: u32,
    source_width: i32,
    source_height: i32,
    output_width: u32,
    output_height: u32,
) -> (RECT, RECT, RECT) {
    let tw = tex_width.max(1);
    let th = tex_height.max(1);
    let sw = if source_width <= 0 {
        tw
    } else {
        (source_width as u32).clamp(1, tw)
    };
    let sh = if source_height <= 0 {
        th
    } else {
        (source_height as u32).clamp(1, th)
    };
    let ow = output_width.max(1);
    let oh = output_height.max(1);
    let source_rect = RECT {
        left: 0,
        top: 0,
        right: sw as i32,
        bottom: sh as i32,
    };
    let dest_rect = RECT {
        left: 0,
        top: 0,
        right: ow as i32,
        bottom: oh as i32,
    };
    (source_rect, dest_rect, dest_rect)
}

pub(super) struct Fp16ShaderConverter {
    vs: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
    ps_sdr: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    cbuf: ID3D11Buffer,
    sdr_bgra: Option<(ID3D11Texture2D, ID3D11RenderTargetView, u32, u32)>,
    staging_srv_tex: Option<(ID3D11Texture2D, u32, u32, DXGI_FORMAT)>,
}

impl Fp16ShaderConverter {
    pub(super) fn new(device: &ID3D11Device) -> Result<Self> {
        let vs_bytes = compile_hlsl(b"vs_main\0", b"vs_5_0\0")?;
        let ps_y_bytes = compile_hlsl(b"ps_y\0", b"ps_5_0\0")?;
        let ps_uv_bytes = compile_hlsl(b"ps_uv\0", b"ps_5_0\0")?;
        let ps_sdr_bytes = compile_hlsl(b"ps_sdr\0", b"ps_5_0\0")?;

        let mut vs = None;
        let mut ps_y = None;
        let mut ps_uv = None;
        let mut ps_sdr = None;
        unsafe {
            device.CreateVertexShader(&vs_bytes, None, Some(&mut vs))?;
            device.CreatePixelShader(&ps_y_bytes, None, Some(&mut ps_y))?;
            device.CreatePixelShader(&ps_uv_bytes, None, Some(&mut ps_uv))?;
            device.CreatePixelShader(&ps_sdr_bytes, None, Some(&mut ps_sdr))?;
        }
        let samp_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler = None;
        unsafe { device.CreateSamplerState(&samp_desc, Some(&mut sampler)) }?;

        let cbuf_desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let mut cbuf = None;
        unsafe { device.CreateBuffer(&cbuf_desc, None, Some(&mut cbuf)) }?;

        Ok(Self {
            vs: vs.context("Missing HDR converter vertex shader.")?,
            ps_y: ps_y.context("Missing HDR converter Y pixel shader.")?,
            ps_uv: ps_uv.context("Missing HDR converter UV pixel shader.")?,
            ps_sdr: ps_sdr.context("Missing HDR converter SDR pixel shader.")?,
            sampler: sampler.context("Missing HDR converter sampler.")?,
            cbuf: cbuf.context("Missing HDR converter constant buffer.")?,
            sdr_bgra: None,
            staging_srv_tex: None,
        })
    }

    fn source_srv(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        picture: &ID3D11Texture2D,
        source: &D3D11_TEXTURE2D_DESC,
    ) -> Result<ID3D11ShaderResourceView> {
        let srv_tex = if (source.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32) != 0 {
            picture.clone()
        } else {
            let need_new = !matches!(
                self.staging_srv_tex,
                Some((_, w, h, fmt)) if w == source.Width && h == source.Height && fmt == source.Format
            );
            if need_new {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: source.Width,
                    Height: source.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: source.Format,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    ..Default::default()
                };
                let mut tex = None;
                unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }?;
                let tex = tex.context("Could not allocate staging SRV texture.")?;
                self.staging_srv_tex = Some((tex, source.Width, source.Height, source.Format));
            }
            let copy_tex = self.staging_srv_tex.as_ref().unwrap().0.clone();
            unsafe { context.CopyResource(&copy_tex, picture) };
            copy_tex
        };
        let mut srv_desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
            Format: source.Format,
            ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        srv_desc.Anonymous.Texture2D.MostDetailedMip = 0;
        srv_desc.Anonymous.Texture2D.MipLevels = 1;
        let mut srv = None;
        unsafe { device.CreateShaderResourceView(&srv_tex, Some(&srv_desc), Some(&mut srv)) }?;
        srv.context("Could not create HDR source SRV.")
    }

    fn bind_common(
        &self,
        context: &ID3D11DeviceContext,
        srv: &ID3D11ShaderResourceView,
        source: &D3D11_TEXTURE2D_DESC,
        source_width: i32,
        source_height: i32,
    ) {
        let (uv_x, uv_y) =
            compute_fp16_uv_scale(source.Width, source.Height, source_width, source_height);
        let constants = [uv_x, uv_y, 0.0f32, 0.0f32];
        unsafe {
            context.UpdateSubresource(&self.cbuf, 0, None, constants.as_ptr().cast(), 0, 0);
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vs, None);
            context.VSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            context.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
        }
    }

    fn draw_pass(
        context: &ID3D11DeviceContext,
        ps: &ID3D11PixelShader,
        rtv: &ID3D11RenderTargetView,
        width: u32,
        height: u32,
    ) {
        let vp = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: width as f32,
            Height: height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        unsafe {
            context.RSSetViewports(Some(&[vp]));
            context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            context.PSSetShader(ps, None);
            context.Draw(3, 0);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn convert_to_p010(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        picture: &ID3D11Texture2D,
        source: &D3D11_TEXTURE2D_DESC,
        source_width: i32,
        source_height: i32,
        rtv_y: &ID3D11RenderTargetView,
        rtv_uv: &ID3D11RenderTargetView,
        out_width: u32,
        out_height: u32,
    ) -> Result<()> {
        let srv = self.source_srv(device, context, picture, source)?;
        self.bind_common(context, &srv, source, source_width, source_height);
        Self::draw_pass(context, &self.ps_y, rtv_y, out_width, out_height);
        Self::draw_pass(
            context,
            &self.ps_uv,
            rtv_uv,
            (out_width / 2).max(1),
            (out_height / 2).max(1),
        );
        unsafe {
            context.OMSetRenderTargets(None, None);
            context.PSSetShaderResources(0, Some(&[None]));
            context.Flush();
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn convert_to_bgra(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        picture: &ID3D11Texture2D,
        source: &D3D11_TEXTURE2D_DESC,
        source_width: i32,
        source_height: i32,
        out_width: u32,
        out_height: u32,
    ) -> Result<ID3D11Texture2D> {
        let need_new = !matches!(
            self.sdr_bgra,
            Some((_, _, w, h)) if w == out_width && h == out_height
        );
        if need_new {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: out_width,
                Height: out_height,
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
            let mut tex = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }?;
            let tex = tex.context("Could not allocate SDR tone-map BGRA texture.")?;
            let mut rtv = None;
            unsafe { device.CreateRenderTargetView(&tex, None, Some(&mut rtv)) }?;
            let rtv = rtv.context("Could not create SDR tone-map RTV.")?;
            self.sdr_bgra = Some((tex, rtv, out_width, out_height));
        }
        let srv = self.source_srv(device, context, picture, source)?;
        let (tex, rtv, _, _) = self.sdr_bgra.as_ref().unwrap();
        let tex = tex.clone();
        let rtv = rtv.clone();
        self.bind_common(context, &srv, source, source_width, source_height);
        Self::draw_pass(context, &self.ps_sdr, &rtv, out_width, out_height);
        unsafe {
            context.OMSetRenderTargets(None, None);
            context.PSSetShaderResources(0, Some(&[None]));
            context.Flush();
        }
        Ok(tex)
    }
}

pub(super) fn create_p010_rtvs(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
) -> Result<(ID3D11RenderTargetView, ID3D11RenderTargetView)> {
    let mut y_desc = D3D11_RENDER_TARGET_VIEW_DESC {
        Format: DXGI_FORMAT_R16_UNORM,
        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
        ..Default::default()
    };
    y_desc.Anonymous.Texture2D.MipSlice = 0;
    let mut rtv_y = None;
    unsafe { device.CreateRenderTargetView(texture, Some(&y_desc), Some(&mut rtv_y)) }?;

    let mut uv_desc = D3D11_RENDER_TARGET_VIEW_DESC {
        Format: DXGI_FORMAT_R16G16_UNORM,
        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
        ..Default::default()
    };
    uv_desc.Anonymous.Texture2D.MipSlice = 0;
    let mut rtv_uv = None;
    unsafe { device.CreateRenderTargetView(texture, Some(&uv_desc), Some(&mut rtv_uv)) }?;

    Ok((
        rtv_y.context("Missing P010 Y render target view.")?,
        rtv_uv.context("Missing P010 UV render target view.")?,
    ))
}

pub(super) fn video_processor_supports_input(
    enumerator: &ID3D11VideoProcessorEnumerator,
    format: DXGI_FORMAT,
) -> bool {
    unsafe { enumerator.CheckVideoProcessorFormat(format) }
        .is_ok_and(|flags| (flags & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT) != 0)
}

/// Manages the D3D11 VideoProcessor, FP16 HDR shader converter, and 4-slot
/// NV12/P010 encoder surface pool.
pub(super) struct SurfaceConverter {
    pub(super) surface_pool: Vec<EncodedSurfaceSlot>,
    surface_index: usize,
    pub(super) last_surface_idx: Option<usize>,
    processor: Option<ID3D11VideoProcessor>,
    enumerator: Option<ID3D11VideoProcessorEnumerator>,
    fp16_converter: Option<Fp16ShaderConverter>,
    fp16_uses_shader: bool,
    input_width: u32,
    input_height: u32,
    input_format: DXGI_FORMAT,
    output_width: u32,
    output_height: u32,
    use_p010: bool,
}

impl SurfaceConverter {
    pub(super) fn new(output_width: u32, output_height: u32, use_p010: bool) -> Self {
        Self {
            surface_pool: Vec::new(),
            surface_index: 0,
            last_surface_idx: None,
            processor: None,
            enumerator: None,
            fp16_converter: None,
            fp16_uses_shader: false,
            input_width: 0,
            input_height: 0,
            input_format: DXGI_FORMAT::default(),
            output_width,
            output_height,
            use_p010,
        }
    }

    pub(super) fn clear(&mut self) {
        self.surface_pool.clear();
        self.last_surface_idx = None;
        self.fp16_converter = None;
        self.fp16_uses_shader = false;
        self.processor = None;
        self.enumerator = None;
    }

    pub(super) fn texture(&self, idx: usize) -> ID3D11Texture2D {
        self.surface_pool[idx].texture.clone()
    }

    fn ensure_surface_pool(&mut self, gpu: &VideoDevice) -> Result<()> {
        if !self.surface_pool.is_empty() {
            return Ok(());
        }
        let enumerator = self.enumerator.as_ref().unwrap();
        let format = if self.use_p010 {
            DXGI_FORMAT_P010
        } else {
            DXGI_FORMAT_NV12
        };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: self.output_width,
            Height: self.output_height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
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
            unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
            let texture = texture.context("Could not allocate encoder surface texture.")?;
            let mut view = None;
            unsafe {
                gpu.video.CreateVideoProcessorOutputView(
                    &texture,
                    enumerator,
                    &target,
                    Some(&mut view),
                )
            }?;
            let view = view.context("Could not create encoder surface output view.")?;
            let (rtv_y, rtv_uv) = if self.use_p010 {
                match create_p010_rtvs(&gpu.device, &texture) {
                    Ok((y, uv)) => (Some(y), Some(uv)),
                    Err(_) => (None, None),
                }
            } else {
                (None, None)
            };
            self.surface_pool.push(EncodedSurfaceSlot {
                texture,
                view,
                rtv_y,
                rtv_uv,
            });
        }
        Ok(())
    }

    pub(super) fn convert<F>(
        &mut self,
        gpu: &VideoDevice,
        picture: &ID3D11Texture2D,
        source_width: i32,
        source_height: i32,
        on_pool_reset: F,
    ) -> Result<usize>
    where
        F: FnOnce(),
    {
        let mut source = D3D11_TEXTURE2D_DESC::default();
        unsafe { picture.GetDesc(&mut source) };
        if self.processor.is_none()
            || self.input_width != source.Width
            || self.input_height != source.Height
            || self.input_format != source.Format
        {
            on_pool_reset();
            self.surface_pool.clear();
            self.last_surface_idx = None;
            self.enumerator = None;
            self.processor = None;
            self.fp16_uses_shader = false;
            let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputWidth: source.Width,
                InputHeight: source.Height,
                OutputWidth: self.output_width,
                OutputHeight: self.output_height,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                ..Default::default()
            };
            let mut enumerator = unsafe { gpu.video.CreateVideoProcessorEnumerator(&content) }?;
            let vp_supports_source = video_processor_supports_input(&enumerator, source.Format);
            if !vp_supports_source && source.Format == DXGI_FORMAT_R16G16B16A16_FLOAT {
                self.fp16_uses_shader = true;
                // When tone-mapping FP16 scRGB -> SDR BGRA at output dimensions before
                // NV12 conversion, match the VideoProcessor's InputWidth/InputHeight to
                // the intermediate BGRA texture so D3D11 never sees a size mismatch.
                if !self.use_p010
                    && (source.Width != self.output_width || source.Height != self.output_height)
                {
                    let sdr_content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                        InputWidth: self.output_width,
                        InputHeight: self.output_height,
                        OutputWidth: self.output_width,
                        OutputHeight: self.output_height,
                        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
                        ..Default::default()
                    };
                    enumerator = unsafe { gpu.video.CreateVideoProcessorEnumerator(&sdr_content) }?;
                }
            }
            let processor = unsafe { gpu.video.CreateVideoProcessor(&enumerator, 0) }?;
            self.enumerator = Some(enumerator);
            self.processor = Some(processor);
            self.input_width = source.Width;
            self.input_height = source.Height;
            self.input_format = source.Format;
        }
        self.ensure_surface_pool(gpu)?;

        let idx = self.surface_index;
        self.surface_index = (self.surface_index + 1) % self.surface_pool.len();

        if self.fp16_uses_shader {
            if self.fp16_converter.is_none() {
                self.fp16_converter = Some(Fp16ShaderConverter::new(&gpu.device)?);
            }
            if self.use_p010 {
                let slot = &self.surface_pool[idx];
                let rtv_y = slot
                    .rtv_y
                    .as_ref()
                    .context("Missing P010 Y RTV for FP16 HDR shader conversion.")?;
                let rtv_uv = slot
                    .rtv_uv
                    .as_ref()
                    .context("Missing P010 UV RTV for FP16 HDR shader conversion.")?;
                self.fp16_converter.as_mut().unwrap().convert_to_p010(
                    &gpu.device,
                    &gpu.context,
                    picture,
                    &source,
                    source_width,
                    source_height,
                    rtv_y,
                    rtv_uv,
                    self.output_width,
                    self.output_height,
                )?;
                return Ok(idx);
            }

            let bgra = self.fp16_converter.as_mut().unwrap().convert_to_bgra(
                &gpu.device,
                &gpu.context,
                picture,
                &source,
                source_width,
                source_height,
                self.output_width,
                self.output_height,
            )?;
            return self.convert_with_video_processor(
                gpu,
                idx,
                &bgra,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                self.output_width,
                self.output_height,
                self.output_width as i32,
                self.output_height as i32,
            );
        }

        self.convert_with_video_processor(
            gpu,
            idx,
            picture,
            source.Format,
            source.Width,
            source.Height,
            source_width,
            source_height,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn convert_with_video_processor(
        &self,
        gpu: &VideoDevice,
        idx: usize,
        picture: &ID3D11Texture2D,
        source_format: DXGI_FORMAT,
        tex_width: u32,
        tex_height: u32,
        source_width: i32,
        source_height: i32,
    ) -> Result<usize> {
        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let mut input_view = None;
        unsafe {
            gpu.video.CreateVideoProcessorInputView(
                picture,
                self.enumerator.as_ref().unwrap(),
                &in_desc,
                Some(&mut input_view),
            )
        }?;
        let input_view = input_view.context("Could not create capture input view.")?;

        let processor = self.processor.as_ref().unwrap();
        let slot = &self.surface_pool[idx];

        let (source_rect, dest_rect, target_rect) = compute_video_processor_rects(
            tex_width,
            tex_height,
            source_width,
            source_height,
            self.output_width,
            self.output_height,
        );
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(input_view)),
            ..Default::default()
        }];

        let input_cs = match source_format {
            f if f == DXGI_FORMAT_R16G16B16A16_FLOAT => DXGI_CS_RGB_FULL_G10_NONE_P709,
            f if f == DXGI_FORMAT_R10G10B10A2_UNORM => DXGI_CS_RGB_FULL_G22_NONE_P709,
            _ => DXGI_CS_RGB_FULL_G22_NONE_P709,
        };
        let use_hdr_cs = self.use_p010 || source_format == DXGI_FORMAT_R16G16B16A16_FLOAT;
        let video_ctx1 = use_hdr_cs
            .then(|| gpu.video_context.cast::<ID3D11VideoContext1>().ok())
            .flatten();

        unsafe {
            gpu.video_context.VideoProcessorSetStreamSourceRect(
                processor,
                0,
                true,
                Some(&source_rect),
            );
            gpu.video_context
                .VideoProcessorSetStreamDestRect(processor, 0, true, Some(&dest_rect));
            gpu.video_context.VideoProcessorSetOutputTargetRect(
                processor,
                true,
                Some(&target_rect),
            );
            gpu.video_context
                .VideoProcessorSetStreamAutoProcessingMode(processor, 0, false);
            if let Some(ctx1) = &video_ctx1 {
                ctx1.VideoProcessorSetStreamColorSpace1(processor, 0, input_cs);
                let out_cs = if self.use_p010 {
                    DXGI_CS_YCBCR_STUDIO_G2084_LEFT_P2020
                } else {
                    DXGI_CS_YCBCR_FULL_G22_LEFT_P709
                };
                ctx1.VideoProcessorSetOutputColorSpace1(processor, out_cs);
            } else {
                gpu.video_context
                    .VideoProcessorSetOutputColorSpace(processor, &BT709_FULL_RANGE);
            }
            let blt = gpu
                .video_context
                .VideoProcessorBlt(processor, &slot.view, 0, &streams);
            let _ = ManuallyDrop::take(&mut streams[0].pInputSurface);
            blt?;
            gpu.context.Flush();
        }
        Ok(idx)
    }
}
