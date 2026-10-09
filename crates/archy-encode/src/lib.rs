//! Hardware video encoding. Frames go in as NV12 GPU textures and come out as
//! compressed packets, so only a few KB per frame ever cross to the CPU.
//!
//! Two backends:
//! - [`vpl::VplEncoder`]: Intel oneVPL (Quick Sync) driven directly, with the
//!   recorder's textures imported zero-copy. Much less CPU than going through
//!   Media Foundation on Intel.
//! - [`mf::MfEncoder`]: Media Foundation encoder MFTs, covering NVIDIA and AMD.
//!
//! Encoder "quality extras" that run on shader cores (lookahead, B-frame
//! analysis) stay off; quality comes from bitrate instead, which costs the
//! game nothing.

use std::sync::mpsc::Receiver;

use archy_gpu::probe::Codec;
use archy_gpu::SendPtr;
use windows::core::Result;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

pub mod mf;
pub mod vpl;

pub struct Config {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Upper bound in bits per second; quality-based modes use less on calm scenes.
    pub max_bitrate: u32,
    /// 0–100, used when the encoder supports quality-based rate control.
    pub quality: u32,
    /// Frames between keyframes. One second keeps clip starts and trims precise.
    pub gop: u32,
}

pub struct Input {
    pub texture: ID3D11Texture2D,
    /// Presentation time in 100 ns units.
    pub time: i64,
}

/// One compressed frame (Annex B for H.264/HEVC, OBUs for AV1).
pub struct Packet<'a> {
    pub data: &'a [u8],
    /// Presentation time in 100 ns units.
    pub time: i64,
    pub duration: i64,
    pub keyframe: bool,
}

#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub frames_in: u64,
    pub packets: u64,
    pub keyframes: u64,
    pub bytes: u64,
}

impl Stats {
    fn count(&mut self, p: &Packet) {
        self.packets += 1;
        self.keyframes += p.keyframe as u64;
        self.bytes += p.data.len() as u64;
    }
}

/// Which encoder implementation to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Intel oneVPL, zero-copy (Intel GPUs only).
    Vpl,
    /// Media Foundation MFT (any vendor).
    MediaFoundation,
}

pub enum Encoder {
    Vpl(Box<vpl::VplEncoder>),
    Mf(mf::MfEncoder),
}

// Both backends drive a multithread-protected device and are used from one
// encoder thread at a time.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(backend: Backend, gpu: &archy_gpu::device::Gpu, cfg: &Config) -> Result<Self> {
        Ok(match backend {
            Backend::Vpl => Self::Vpl(Box::new(vpl::VplEncoder::new(gpu, cfg)?)),
            Backend::MediaFoundation => Self::Mf(mf::MfEncoder::new(gpu, cfg)?),
        })
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Vpl(e) => &e.name,
            Self::Mf(e) => &e.name,
        }
    }

    pub fn rate_control(&self) -> &'static str {
        match self {
            Self::Vpl(e) => e.rate_control,
            Self::Mf(e) => e.rate_control,
        }
    }

    /// Encodes until `inputs` disconnects, then drains; `on_packet` gets every packet in order.
    pub fn run(
        &mut self,
        inputs: Receiver<SendPtr<Input>>,
        on_packet: impl FnMut(Packet) -> Result<()>,
    ) -> Result<Stats> {
        match self {
            Self::Vpl(e) => e.run(inputs, on_packet),
            Self::Mf(e) => e.run(inputs, on_packet),
        }
    }
}
