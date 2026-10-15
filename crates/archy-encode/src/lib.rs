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

/// Test-only fault injection (`ARCHY_FAULT=<name>`), so the failure paths a bad
/// NVIDIA/AMD driver would trigger can be exercised on any PC
/// (examples/fault_test). Never set in normal use.
pub fn fault(name: &str) -> bool {
    std::env::var("ARCHY_FAULT").is_ok_and(|v| v == name)
}

pub struct Config {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Upper bound in bits per second; quality-based modes use less on calm scenes.
    pub max_bitrate: u32,
    /// 0–100, used when the encoder supports quality-based rate control.
    pub quality: u32,
    /// `Some(bps)`: aim for this average bitrate (VBR, capped at `max_bitrate`) so
    /// file size is predictable. `None`: constant quality, size varies with the scene.
    pub target_bitrate: Option<u32>,
    /// Frames between keyframes. One second keeps clip starts and trims precise.
    pub gop: u32,
}

pub struct Input {
    pub texture: ID3D11Texture2D,
    /// Presentation time in 100 ns units.
    pub time: i64,
    /// Counts this frame as in flight until the encoder is done with it.
    pub ticket: Option<InFlight>,
    /// The same picture as the previous frame (a slot filled with a repeat). An
    /// encoder that can, writes a near-free "nothing changed" frame instead.
    pub repeat: bool,
}

/// A frame the encoder hasn't finished taking yet. The recorder converts into a
/// small rotating pool of textures; it only converts another frame while fewer
/// than the pool can spare are in flight, so a queued frame is never overwritten
/// by a newer one (which showed as old pictures after new ones under load).
pub struct InFlight(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl InFlight {
    pub fn new(count: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self(count.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
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
    /// Times the encoder needed more than a second for a frame.
    pub late: u64,
    /// Keyframes the encoder was made to insert because it ignored the keyframe interval.
    pub forced_keyframes: u64,
    /// Repeated frames written as near-free skip frames (oneVPL).
    pub skipped: u64,
}

/// Whether a compressed frame really is a safe place to start decoding, read
/// from the bitstream itself rather than trusting the encoder's label:
/// H.264 IDR (NAL 5), HEVC IRAP (NAL 16-21; with no B-frames a CRA has no
/// leading pictures). `None` for AV1, where the encoder's flag is used.
/// Cheap: stops at the first picture NAL unit, near the start of the packet.
pub fn random_access(codec: Codec, data: &[u8]) -> Option<bool> {
    if codec == Codec::Av1 {
        return None;
    }
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let header = data[i + 3];
            match codec {
                Codec::H264 => match header & 0x1f {
                    5 => return Some(true),
                    1..=4 => return Some(false),
                    _ => {}
                },
                _ => match (header >> 1) & 0x3f {
                    16..=21 => return Some(true),
                    0..=15 => return Some(false),
                    _ => {}
                },
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    Some(false)
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
    Mf(Box<mf::MfEncoder>),
}

// Both backends drive a multithread-protected device and are used from one
// encoder thread at a time.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(backend: Backend, gpu: &archy_gpu::device::Gpu, cfg: &Config) -> Result<Self> {
        Ok(match backend {
            Backend::Vpl => Self::Vpl(Box::new(vpl::VplEncoder::new(gpu, cfg)?)),
            Backend::MediaFoundation => Self::Mf(Box::new(mf::MfEncoder::new(gpu, cfg)?)),
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
