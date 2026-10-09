//! Scale + BGRA→NV12 (BT.709 limited range) with two tiny pixel-shader passes:
//! one into the Y plane, one into the half-resolution UV plane.
//!
//! The trade-off against the video-processor path: queuing this costs the CPU
//! microseconds instead of milliseconds (Intel's video-processor driver is
//! CPU-heavy), at the price of a sub-millisecond slice of shader time.

use windows::core::{s, Interface, Result, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11PixelShader,
    ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11VertexShader, D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_LINEAR,
    D3D11_RENDER_TARGET_VIEW_DESC, D3D11_RENDER_TARGET_VIEW_DESC_0, D3D11_RTV_DIMENSION_TEXTURE2D,
    D3D11_SAMPLER_DESC, D3D11_TEX2D_RTV, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM};

use crate::convert::create_nv12;
use crate::device::Gpu;

const HLSL: &str = r#"
Texture2D src : register(t0);
SamplerState samp : register(s0);

struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };

// Fullscreen triangle, no vertex buffer.
V vs(uint id : SV_VertexID) {
    V o;
    o.uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

// Downscaling more than ~1.5x with one bilinear tap skips source pixels and makes
// edges shimmer. When shrinking, average four taps spread over the output pixel's
// footprint (each tap already blends 2x2), so 1080p->360p stays smooth. 1:1 stays one tap.
float3 fetch(float2 uv) {
    float sw, sh;
    src.GetDimensions(sw, sh);
    float2 step = float2(ddx(uv).x, ddy(uv).y);
    if (step.x * sw < 1.5 && step.y * sh < 1.5)
        return src.Sample(samp, uv).rgb;
    float2 o = step * 0.25;
    return 0.25 * (src.Sample(samp, uv + float2(-o.x, -o.y)).rgb + src.Sample(samp, uv + float2(o.x, -o.y)).rgb
                 + src.Sample(samp, uv + float2(-o.x, o.y)).rgb + src.Sample(samp, uv + float2(o.x, o.y)).rgb);
}

// BT.709, limited (studio) range, applied to the sRGB-encoded values as video expects.
float ps_y(V i) : SV_Target {
    float3 c = fetch(i.uv);
    return dot(c, float3(0.2126, 0.7152, 0.0722)) * (219.0 / 255.0) + (16.0 / 255.0);
}

// Rendered at half size: bilinear sampling at the 2x2 block centre averages it.
float2 ps_uv(V i) : SV_Target {
    float3 c = fetch(i.uv);
    float cb = dot(c, float3(-0.1146, -0.3854, 0.5));
    float cr = dot(c, float3(0.5, -0.4542, -0.0458));
    return float2(cb, cr) * (224.0 / 255.0) + (128.0 / 255.0);
}
"#;

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let r = unsafe {
        D3DCompile(
            HLSL.as_ptr() as *const _,
            HLSL.len(),
            s!("archy_convert"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .map(|b| unsafe {
                let bytes = std::slice::from_raw_parts(
                    b.GetBufferPointer() as *const u8,
                    b.GetBufferSize(),
                );
                String::from_utf8_lossy(bytes).into_owned()
            })
            .unwrap_or_default();
        return Err(windows::core::Error::new(e.code(), msg));
    }
    Ok(code.expect("D3DCompile returned no bytecode"))
}

fn bytecode(blob: &ID3DBlob) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
}

struct Target {
    texture: ID3D11Texture2D,
    y: ID3D11RenderTargetView,
    uv: ID3D11RenderTargetView,
}

pub struct ShaderConverter {
    context: ID3D11DeviceContext,
    lock: ID3D11Multithread,
    device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    outputs: Vec<Target>,
    next: usize,
    sources: Vec<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    out_size: (u32, u32),
}

impl ShaderConverter {
    pub fn new(gpu: &Gpu, out_size: (u32, u32), pool_len: usize) -> Result<Self> {
        let device = &gpu.device;
        let vs_code = compile(s!("vs"), s!("vs_4_0"))?;
        let y_code = compile(s!("ps_y"), s!("ps_4_0"))?;
        let uv_code = compile(s!("ps_uv"), s!("ps_4_0"))?;
        let (mut vs, mut ps_y, mut ps_uv, mut sampler) = (None, None, None, None);
        unsafe {
            device.CreateVertexShader(bytecode(&vs_code), None, Some(&mut vs))?;
            device.CreatePixelShader(bytecode(&y_code), None, Some(&mut ps_y))?;
            device.CreatePixelShader(bytecode(&uv_code), None, Some(&mut ps_uv))?;
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            device.CreateSamplerState(&desc, Some(&mut sampler))?;
        }

        let mut outputs = Vec::with_capacity(pool_len);
        for _ in 0..pool_len {
            let texture = create_nv12(device, out_size.0, out_size.1)?;
            // On NV12, an R8 view addresses the Y plane and an R8G8 view the UV plane.
            let y = plane_view(device, &texture, DXGI_FORMAT_R8_UNORM)?;
            let uv = plane_view(device, &texture, DXGI_FORMAT_R8G8_UNORM)?;
            outputs.push(Target { texture, y, uv });
        }

        Ok(Self {
            context: gpu.context.clone(),
            lock: gpu.device.cast()?,
            device: device.clone(),
            vs: vs.expect("no vertex shader"),
            ps_y: ps_y.expect("no Y shader"),
            ps_uv: ps_uv.expect("no UV shader"),
            sampler: sampler.expect("no sampler"),
            outputs,
            next: 0,
            sources: Vec::new(),
            out_size,
        })
    }

    pub fn convert(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let srv = self.source_view(src)?;
        let target = &self.outputs[self.next];
        self.next = (self.next + 1) % self.outputs.len();
        let (w, h) = (self.out_size.0 as f32, self.out_size.1 as f32);
        let ctx = &self.context;
        unsafe {
            // The encoder shares this device; keep our state changes atomic.
            self.lock.Enter();
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

            ctx.PSSetShader(&self.ps_y, None);
            ctx.OMSetRenderTargets(Some(&[Some(target.y.clone())]), None);
            ctx.RSSetViewports(Some(&[viewport(w, h)]));
            ctx.Draw(3, 0);

            ctx.PSSetShader(&self.ps_uv, None);
            ctx.OMSetRenderTargets(Some(&[Some(target.uv.clone())]), None);
            ctx.RSSetViewports(Some(&[viewport(w / 2.0, h / 2.0)]));
            ctx.Draw(3, 0);

            // Unbind so the capture texture and NV12 target are free for others.
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            self.lock.Leave();
        }
        Ok(target.texture.clone())
    }

    fn source_view(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView> {
        if let Some((_, v)) = self.sources.iter().find(|(t, _)| t == src) {
            return Ok(v.clone());
        }
        let mut view = None;
        unsafe {
            self.device
                .CreateShaderResourceView(src, None, Some(&mut view))?
        };
        let view = view.expect("CreateShaderResourceView returned no view");
        if self.sources.len() >= 4 {
            self.sources.remove(0);
        }
        self.sources.push((src.clone(), view.clone()));
        Ok(view)
    }
}

fn plane_view(
    device: &ID3D11Device,
    tex: &ID3D11Texture2D,
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
) -> Result<ID3D11RenderTargetView> {
    let desc = D3D11_RENDER_TARGET_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
        },
    };
    let mut view = None;
    unsafe { device.CreateRenderTargetView(tex, Some(&desc), Some(&mut view))? };
    Ok(view.expect("CreateRenderTargetView returned no view"))
}

fn viewport(w: f32, h: f32) -> D3D11_VIEWPORT {
    D3D11_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: w,
        Height: h,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    }
}
