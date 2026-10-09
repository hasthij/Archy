//! Scale + BGRA→NV12 (BT.709 limited range) in one ID3D11VideoProcessor pass.
//!
//! The video processor runs on the GPU's video/media engine, so this step does
//! not compete with the game for shader cores, and it reads the captured frame
//! in place: one full-resolution read, one smaller NV12 write.

use std::mem::ManuallyDrop;

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, ID3D11VideoContext, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET, D3D11_TEX2D_VPIV, D3D11_TEX2D_VPOV,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0, D3D11_VIDEO_PROCESSOR_STREAM,
    D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
    DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};

use crate::convert_shader::ShaderConverter;
use crate::device::Gpu;

/// Output size for a source, capped at `max_height`, aspect kept, both sides even (NV12).
pub fn fit_size(src_w: u32, src_h: u32, max_height: u32) -> (u32, u32) {
    let (w, h) = if src_h > max_height {
        let w = (src_w as u64 * max_height as u64 / src_h as u64) as u32;
        (w, max_height)
    } else {
        (src_w, src_h)
    };
    (w & !1, h & !1)
}

/// Which GPU block does the scale + color conversion. Both stay selectable:
/// which one costs a game less differs per GPU and driver, so benchmarks decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvertMode {
    /// ID3D11VideoProcessor on the media engine: no shader time, but some drivers
    /// (Intel) spend ~1.5 ms of CPU per frame queuing it.
    VideoEngine,
    /// Two pixel-shader passes: microseconds of CPU, a sliver of shader time.
    Shader,
}

pub enum AnyConverter {
    VideoEngine(Converter),
    Shader(ShaderConverter),
}

impl AnyConverter {
    pub fn new(
        gpu: &Gpu,
        mode: ConvertMode,
        in_size: (u32, u32),
        out_size: (u32, u32),
        fps: u32,
        pool_len: usize,
    ) -> Result<Self> {
        Ok(match mode {
            ConvertMode::VideoEngine => {
                Self::VideoEngine(Converter::new(gpu, in_size, out_size, fps, pool_len)?)
            }
            ConvertMode::Shader => Self::Shader(ShaderConverter::new(gpu, out_size, pool_len)?),
        })
    }

    pub fn convert(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        match self {
            Self::VideoEngine(c) => c.convert(src),
            Self::Shader(c) => c.convert(src),
        }
    }
}

/// Hardware encoders work on 16-pixel macroblocks/CTUs; NV12 targets are
/// allocated at the aligned size and the encoder crops to the visible size.
pub fn align16(v: u32) -> u32 {
    (v + 15) & !15
}

pub struct Converter {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    /// Input views for capture textures seen so far. The capture pool recycles a
    /// couple of textures, so views are created once instead of every frame.
    inputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    pub out_width: u32,
    pub out_height: u32,
}

impl Converter {
    /// `pool_len` NV12 targets rotate so the encoder can still be reading older ones.
    pub fn new(
        gpu: &Gpu,
        in_size: (u32, u32),
        out_size: (u32, u32),
        fps: u32,
        pool_len: usize,
    ) -> Result<Self> {
        let video_device: ID3D11VideoDevice = gpu.device.cast()?;
        let video_context: ID3D11VideoContext = gpu.context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: fps,
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_size.0,
            InputHeight: in_size.1,
            OutputFrameRate: rate,
            OutputWidth: out_size.0,
            OutputHeight: out_size.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content)? };
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0)? };

        unsafe {
            // No driver "enhancements" (denoise, edge, skin tone...): pure scale + convert.
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            // Only the visible area; alignment padding below/right is cropped by the encoder.
            let visible = RECT {
                left: 0,
                top: 0,
                right: out_size.0 as i32,
                bottom: out_size.1 as i32,
            };
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&visible));
            // Correct color metadata is free quality: sRGB in, BT.709 limited range out.
            if let Ok(vc1) = video_context.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }
        }

        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: align16(out_size.0),
            Height: align16(out_size.1),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut outputs = Vec::with_capacity(pool_len);
        for _ in 0..pool_len {
            let mut tex = None;
            unsafe {
                gpu.device
                    .CreateTexture2D(&tex_desc, None, Some(&mut tex))?
            };
            let tex = tex.expect("CreateTexture2D returned no texture");
            let mut view = None;
            unsafe {
                video_device.CreateVideoProcessorOutputView(
                    &tex,
                    &enumerator,
                    &view_desc,
                    Some(&mut view),
                )?
            };
            outputs.push((tex, view.expect("no output view")));
        }

        Ok(Self {
            video_device,
            video_context,
            enumerator,
            processor,
            outputs,
            next: 0,
            inputs: Vec::new(),
            out_width: out_size.0,
            out_height: out_size.1,
        })
    }

    /// Converts `src` (BGRA) into the next pooled NV12 texture and returns it.
    pub fn convert(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
        let input = self.input_view(src)?;
        let (tex, view) = &self.outputs[self.next];
        self.next = (self.next + 1) % self.outputs.len();

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(input)),
            ..Default::default()
        };
        let result = unsafe {
            self.video_context.VideoProcessorBlt(
                &self.processor,
                view,
                0,
                std::slice::from_ref(&stream),
            )
        };
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        result?;
        Ok(tex.clone())
    }

    fn input_view(&mut self, src: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView> {
        if let Some((_, v)) = self.inputs.iter().find(|(t, _)| t == src) {
            return Ok(v.clone());
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut view = None;
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                src,
                &self.enumerator,
                &desc,
                Some(&mut view),
            )?
        };
        let view = view.expect("CreateVideoProcessorInputView returned no view");
        // Bounded: if the capture source is recreated, old textures age out.
        if self.inputs.len() >= 4 {
            self.inputs.remove(0);
        }
        self.inputs.push((src.clone(), view.clone()));
        Ok(view)
    }
}
