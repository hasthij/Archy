//! Intel oneVPL (Quick Sync) encoder, driven directly.
//!
//! The dispatcher `libvpl.dll` ships with Intel's graphics driver (System32) and
//! is loaded at runtime, so non-Intel PCs simply don't offer this backend.
//! Each frame is copied GPU-side into one of the runtime's own input surfaces
//! (one CopySubresourceRegion: microseconds of CPU), which works on every Intel
//! driver; texture import (`ImportFrameSurface`) was refused by current drivers
//! and cost ~1.5 ms of CPU per frame in its copy fallback. The encoder runs on
//! the fixed-function low-power path (VDEnc) when the GPU has one.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use archy_gpu::device::Gpu;
use archy_gpu::probe::Codec;
use archy_gpu::SendPtr;
use windows::core::{s, w, Error, Interface, Result};
use windows::Win32::Foundation::{FreeLibrary, E_FAIL, HMODULE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX, D3D11_TEXTURE2D_DESC,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use crate::{Config, Input, Packet, Stats};

#[allow(non_snake_case, non_camel_case_types, dead_code)]
mod ffi {
    //! Hand-written subset of the oneVPL 2.x C API. Every layout below is checked
    //! against MSVC's sizeof/offsetof of the official headers (see the asserts).
    use std::ffi::c_void;
    use std::mem::{offset_of, size_of};

    pub type mfxStatus = i32;
    pub type mfxHDL = *mut c_void;
    pub type mfxLoader = *mut c_void;
    pub type mfxConfig = *mut c_void;
    pub type mfxSession = *mut c_void;
    pub type mfxSyncPoint = *mut c_void;

    pub const MFX_ERR_NONE: mfxStatus = 0;
    pub const MFX_ERR_MORE_DATA: mfxStatus = -10;
    pub const MFX_WRN_IN_EXECUTION: mfxStatus = 1;
    pub const MFX_WRN_DEVICE_BUSY: mfxStatus = 2;

    pub const MFX_VARIANT_TYPE_U32: u32 = 5;
    pub const MFX_IMPL_TYPE_HARDWARE: u32 = 0x0002;
    pub const MFX_ACCEL_MODE_VIA_D3D11: u32 = 0x0300;
    pub const MFX_HANDLE_D3D11_DEVICE: u32 = 3;

    pub const MFX_IOPATTERN_IN_VIDEO_MEMORY: u16 = 0x01;
    pub const MFX_PICSTRUCT_PROGRESSIVE: u16 = 0x01;
    pub const MFX_CHROMAFORMAT_YUV420: u16 = 1;
    pub const MFX_GOP_CLOSED: u16 = 1;
    pub const MFX_TARGETUSAGE_BALANCED: u16 = 4;
    pub const MFX_RATECONTROL_VBR: u16 = 2;
    pub const MFX_RATECONTROL_ICQ: u16 = 9;
    pub const MFX_CODINGOPTION_ON: u16 = 0x10;
    pub const MFX_FRAMETYPE_I: u16 = 0x0001;
    pub const MFX_FRAMETYPE_IDR: u16 = 0x0080;

    pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
        (a as u32) | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
    }
    pub const MFX_FOURCC_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');
    pub const MFX_CODEC_AVC: u32 = fourcc(b'A', b'V', b'C', b' ');
    pub const MFX_CODEC_HEVC: u32 = fourcc(b'H', b'E', b'V', b'C');
    pub const MFX_CODEC_AV1: u32 = fourcc(b'A', b'V', b'1', b' ');

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct mfxVariant {
        pub Version: u16,
        pub Type: u32,
        pub Data: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct mfxFrameInfo {
        pub reserved: [u32; 4],
        pub ChannelId: u16,
        pub BitDepthLuma: u16,
        pub BitDepthChroma: u16,
        pub Shift: u16,
        pub FrameId: [u16; 4],
        pub FourCC: u32,
        pub Width: u16,
        pub Height: u16,
        pub CropX: u16,
        pub CropY: u16,
        pub CropW: u16,
        pub CropH: u16,
        pub FrameRateExtN: u32,
        pub FrameRateExtD: u32,
        pub reserved3: u16,
        pub AspectRatioW: u16,
        pub AspectRatioH: u16,
        pub PicStruct: u16,
        pub ChromaFormat: u16,
        pub reserved2: u16,
    }

    /// Encoder view of `mfxInfoMFX` (the decode/JPEG union arms have the same size).
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct mfxInfoMFX {
        pub reserved: [u32; 7],
        pub LowPower: u16,
        pub BRCParamMultiplier: u16,
        pub FrameInfo: mfxFrameInfo,
        pub CodecId: u32,
        pub CodecProfile: u16,
        pub CodecLevel: u16,
        pub NumThread: u16,
        pub TargetUsage: u16,
        pub GopPicSize: u16,
        pub GopRefDist: u16,
        pub GopOptFlag: u16,
        pub IdrInterval: u16,
        pub RateControlMethod: u16,
        pub InitialDelayInKB: u16,
        pub BufferSizeInKB: u16,
        /// Also `ICQQuality` in ICQ mode.
        pub TargetKbps: u16,
        pub MaxKbps: u16,
        pub NumSlice: u16,
        pub NumRefFrame: u16,
        pub EncodedOrder: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct mfxVideoParam {
        pub AllocId: u32,
        pub reserved: [u32; 2],
        pub reserved3: u16,
        pub AsyncDepth: u16,
        pub mfx: mfxInfoMFX,
        /// Rest of the union with `mfxInfoVPP`, which is larger.
        pub _vpp_tail: [u8; 32],
        pub Protected: u16,
        pub IOPattern: u16,
        pub ExtParam: *mut c_void,
        pub NumExtParam: u16,
        pub reserved2: u16,
    }

    #[repr(C)]
    pub struct mfxBitstream {
        pub reserved: [u32; 6],
        pub DecodeTimeStamp: i64,
        pub TimeStamp: u64,
        pub Data: *mut u8,
        pub DataOffset: u32,
        pub DataLength: u32,
        pub MaxLength: u32,
        pub PicStruct: u16,
        pub FrameType: u16,
        pub DataFlag: u16,
        pub reserved2: u16,
    }

    #[repr(C)]
    pub struct mfxFrameData {
        pub ExtParam: u64,
        pub NumExtParam: u16,
        pub reserved: [u16; 9],
        pub MemType: u16,
        pub PitchHigh: u16,
        pub TimeStamp: u64,
        pub FrameOrder: u32,
        pub Locked: u16,
        pub Pitch: u16,
        pub planes: [*mut u8; 4],
        pub MemId: *mut c_void,
        pub Corrupted: u16,
        pub DataFlag: u16,
    }

    #[repr(C)]
    pub struct mfxFrameSurface1 {
        pub FrameInterface: *mut mfxFrameSurfaceInterface,
        pub Version: u16,
        pub reserved1: [u16; 3],
        pub Info: mfxFrameInfo,
        pub Data: mfxFrameData,
    }

    #[repr(C)]
    pub struct mfxFrameSurfaceInterface {
        pub Context: mfxHDL,
        pub Version: u16,
        pub reserved1: [u16; 3],
        pub AddRef: unsafe extern "C" fn(*mut mfxFrameSurface1) -> mfxStatus,
        pub Release: unsafe extern "C" fn(*mut mfxFrameSurface1) -> mfxStatus,
        pub GetRefCounter: *mut c_void,
        pub Map: *mut c_void,
        pub Unmap: *mut c_void,
        /// D3D11: `resource` is the surface's `ID3D11Texture2D*`.
        pub GetNativeHandle:
            unsafe extern "C" fn(*mut mfxFrameSurface1, *mut mfxHDL, *mut u32) -> mfxStatus,
        // GetDeviceHandle, Synchronize, OnComplete, QueryInterface, reserved2[2]: unused.
        pub _rest: [*mut c_void; 6],
    }

    // Layouts verified against MSVC + oneVPL 2.17 headers (x64).
    const _: () = {
        assert!(size_of::<mfxVariant>() == 16 && offset_of!(mfxVariant, Data) == 8);
        assert!(size_of::<mfxFrameInfo>() == 68);
        assert!(offset_of!(mfxFrameInfo, FourCC) == 32 && offset_of!(mfxFrameInfo, Width) == 36);
        assert!(
            offset_of!(mfxFrameInfo, FrameRateExtN) == 48
                && offset_of!(mfxFrameInfo, ChromaFormat) == 64
        );
        assert!(size_of::<mfxInfoMFX>() == 136 && offset_of!(mfxInfoMFX, FrameInfo) == 32);
        assert!(
            offset_of!(mfxInfoMFX, CodecId) == 100 && offset_of!(mfxInfoMFX, TargetUsage) == 110
        );
        assert!(
            offset_of!(mfxInfoMFX, RateControlMethod) == 120
                && offset_of!(mfxInfoMFX, TargetKbps) == 126
        );
        assert!(offset_of!(mfxInfoMFX, EncodedOrder) == 134);
        assert!(size_of::<mfxVideoParam>() == 208 && offset_of!(mfxVideoParam, mfx) == 16);
        assert!(
            offset_of!(mfxVideoParam, IOPattern) == 186
                && offset_of!(mfxVideoParam, ExtParam) == 192
        );
        assert!(size_of::<mfxBitstream>() == 72 && offset_of!(mfxBitstream, Data) == 40);
        assert!(
            offset_of!(mfxBitstream, MaxLength) == 56 && offset_of!(mfxBitstream, FrameType) == 62
        );
        assert!(size_of::<mfxFrameData>() == 96 && offset_of!(mfxFrameData, TimeStamp) == 32);
        assert!(offset_of!(mfxFrameData, MemId) == 80);
        assert!(size_of::<mfxFrameSurface1>() == 184 && offset_of!(mfxFrameSurface1, Data) == 88);
        assert!(size_of::<mfxFrameSurfaceInterface>() == 112);
        assert!(offset_of!(mfxFrameSurfaceInterface, Release) == 24);
        assert!(offset_of!(mfxFrameSurfaceInterface, GetNativeHandle) == 56);
    };
}

use ffi::*;

type FnLoad = unsafe extern "C" fn() -> mfxLoader;
type FnUnload = unsafe extern "C" fn(mfxLoader);
type FnCreateConfig = unsafe extern "C" fn(mfxLoader) -> mfxConfig;
type FnSetFilter = unsafe extern "C" fn(mfxConfig, *const u8, mfxVariant) -> mfxStatus;
type FnCreateSession = unsafe extern "C" fn(mfxLoader, u32, *mut mfxSession) -> mfxStatus;
type FnClose = unsafe extern "C" fn(mfxSession) -> mfxStatus;
type FnSetHandle = unsafe extern "C" fn(mfxSession, u32, mfxHDL) -> mfxStatus;
type FnGetSurface = unsafe extern "C" fn(mfxSession, *mut *mut mfxFrameSurface1) -> mfxStatus;
type FnParam = unsafe extern "C" fn(mfxSession, *mut mfxVideoParam) -> mfxStatus;
type FnEncode = unsafe extern "C" fn(
    mfxSession,
    *mut c_void,
    *mut mfxFrameSurface1,
    *mut mfxBitstream,
    *mut mfxSyncPoint,
) -> mfxStatus;
type FnEncodeClose = unsafe extern "C" fn(mfxSession) -> mfxStatus;
type FnQueryVersion = unsafe extern "C" fn(mfxSession, *mut u32) -> mfxStatus;
type FnSync = unsafe extern "C" fn(mfxSession, mfxSyncPoint, u32) -> mfxStatus;

struct Api {
    lib: HMODULE,
    load: FnLoad,
    unload: FnUnload,
    create_config: FnCreateConfig,
    set_filter: FnSetFilter,
    create_session: FnCreateSession,
    close: FnClose,
    set_handle: FnSetHandle,
    get_surface: FnGetSurface,
    init: FnParam,
    get_video_param: FnParam,
    encode: FnEncode,
    encode_close: FnEncodeClose,
    sync: FnSync,
    query_version: FnQueryVersion,
}

impl Api {
    // Each transmute's target type is the struct field it initializes.
    #[allow(clippy::missing_transmute_annotations)]
    fn load() -> Result<Self> {
        // System32 only: never pick up a planted libvpl.dll from the app directory.
        let lib = unsafe { LoadLibraryExW(w!("libvpl.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)? };
        macro_rules! sym {
            ($name:literal) => {
                match unsafe { GetProcAddress(lib, s!($name)) } {
                    Some(f) => unsafe { std::mem::transmute(f) },
                    None => {
                        unsafe {
                            let _ = FreeLibrary(lib);
                        }
                        return Err(Error::new(E_FAIL, concat!("libvpl.dll lacks ", $name)));
                    }
                }
            };
        }
        Ok(Self {
            lib,
            load: sym!("MFXLoad"),
            unload: sym!("MFXUnload"),
            create_config: sym!("MFXCreateConfig"),
            set_filter: sym!("MFXSetConfigFilterProperty"),
            create_session: sym!("MFXCreateSession"),
            close: sym!("MFXClose"),
            set_handle: sym!("MFXVideoCORE_SetHandle"),
            get_surface: sym!("MFXMemory_GetSurfaceForEncode"),
            init: sym!("MFXVideoENCODE_Init"),
            get_video_param: sym!("MFXVideoENCODE_GetVideoParam"),
            encode: sym!("MFXVideoENCODE_EncodeFrameAsync"),
            encode_close: sym!("MFXVideoENCODE_Close"),
            sync: sym!("MFXVideoCORE_SyncOperation"),
            query_version: sym!("MFXQueryVersion"),
        })
    }
}

fn check(status: mfxStatus, what: &str) -> Result<mfxStatus> {
    if status < MFX_ERR_NONE {
        Err(Error::new(
            E_FAIL,
            format!("{what} failed (oneVPL status {status})"),
        ))
    } else {
        Ok(status)
    }
}

fn codec_id(codec: Codec) -> u32 {
    match codec {
        Codec::H264 => MFX_CODEC_AVC,
        Codec::Hevc => MFX_CODEC_HEVC,
        Codec::Av1 => MFX_CODEC_AV1,
    }
}

fn align16(v: u32) -> u16 {
    ((v + 15) & !15) as u16
}

/// Encoder parameters. `quality` 0–100 maps linearly onto ICQ 51 (worst) … 11;
/// the default 70 lands on ICQ 23, a common "looks clean" setting.
fn video_param(cfg: &Config, low_power: bool, icq: bool) -> mfxVideoParam {
    let max_kbps = cfg.max_bitrate / 1000;
    // Kbps fields are 16-bit; BRCParamMultiplier scales them for high bitrates.
    let mult = max_kbps.div_ceil(u16::MAX as u32).max(1);
    let mut mfx = mfxInfoMFX {
        LowPower: if low_power { MFX_CODINGOPTION_ON } else { 0 },
        BRCParamMultiplier: mult as u16,
        FrameInfo: mfxFrameInfo {
            FourCC: MFX_FOURCC_NV12,
            ChromaFormat: MFX_CHROMAFORMAT_YUV420,
            PicStruct: MFX_PICSTRUCT_PROGRESSIVE,
            Width: align16(cfg.width),
            Height: align16(cfg.height),
            CropW: cfg.width as u16,
            CropH: cfg.height as u16,
            FrameRateExtN: cfg.fps,
            FrameRateExtD: 1,
            AspectRatioW: 1,
            AspectRatioH: 1,
            ..Default::default()
        },
        CodecId: codec_id(cfg.codec),
        TargetUsage: MFX_TARGETUSAGE_BALANCED,
        GopPicSize: cfg.gop as u16,
        // No B-frames: no reordering delay, less encoder work.
        GopRefDist: 1,
        GopOptFlag: MFX_GOP_CLOSED,
        // Every keyframe must be a full reset (IDR) so any clip can start on it.
        // H.264: 0 = every I-frame is IDR. HEVC: 1 = every I-frame is IDR (0 there
        // would mean only the very first one is).
        IdrInterval: if cfg.codec == Codec::Hevc { 1 } else { 0 },
        ..Default::default()
    };
    if icq {
        mfx.RateControlMethod = MFX_RATECONTROL_ICQ;
        mfx.TargetKbps = (51 - cfg.quality.min(100) * 2 / 5) as u16;
        mfx.MaxKbps = 0;
    } else {
        mfx.RateControlMethod = MFX_RATECONTROL_VBR;
        let target_kbps = cfg
            .target_bitrate
            .map(|b| b / 1000)
            .unwrap_or(max_kbps * 7 / 10)
            .min(max_kbps);
        mfx.TargetKbps = (target_kbps / mult) as u16;
        mfx.MaxKbps = (max_kbps / mult) as u16;
        // A 2-second rate-control buffer. With the driver's tiny default, keyframes
        // were squeezed to ~4x a normal frame and clips went blocky once a second;
        // now a keyframe can take what it needs and the following frames pay it back.
        // The average (and so the file size) is unchanged.
        let buffer_kb = target_kbps * 2 / 8;
        mfx.BufferSizeInKB = (buffer_kb / mult).min(u16::MAX as u32) as u16;
        mfx.InitialDelayInKB = (buffer_kb / 2 / mult).min(u16::MAX as u32) as u16;
    }
    mfxVideoParam {
        AllocId: 0,
        reserved: [0; 2],
        reserved3: 0,
        // Frames the runtime may have in flight; matches the encoder's pipeline.
        AsyncDepth: PIPELINE as u16,
        mfx,
        _vpp_tail: [0; 32],
        Protected: 0,
        IOPattern: MFX_IOPATTERN_IN_VIDEO_MEMORY,
        ExtParam: null_mut(),
        NumExtParam: 0,
        reserved2: 0,
    }
}

/// Frames encoding at once. Waiting for each frame (~12 ms on an iGPU) before
/// submitting the next made bursts back up and drop; with a few in flight the
/// GPU stays fed and packets are collected oldest-first, in order.
const PIPELINE: usize = 3;

/// One output buffer; each in-flight frame needs its own.
struct Slot {
    bitstream: mfxBitstream,
    _buffer: Vec<u8>,
}

impl Slot {
    fn new(size: usize) -> Self {
        let mut buffer = vec![0u8; size];
        let mut bitstream: mfxBitstream = unsafe { std::mem::zeroed() };
        // The Vec's heap storage never moves, so this pointer stays valid.
        bitstream.Data = buffer.as_mut_ptr();
        bitstream.MaxLength = size as u32;
        Self {
            bitstream,
            _buffer: buffer,
        }
    }
}

pub struct VplEncoder {
    api: Api,
    loader: mfxLoader,
    session: mfxSession,
    /// The recorder's immediate context, for the per-frame GPU copy.
    context: ID3D11DeviceContext,
    slots: Vec<Slot>,
    free: Vec<usize>,
    in_flight: VecDeque<(mfxSyncPoint, usize)>,
    frame_duration: i64,
    /// AV1 reports key frames as I (no IDR concept).
    av1: bool,
    pub name: String,
    pub rate_control: &'static str,
}

unsafe impl Send for VplEncoder {}

impl VplEncoder {
    pub fn new(gpu: &Gpu, cfg: &Config) -> Result<Self> {
        let api = Api::load()?;
        let loader = unsafe { (api.load)() };
        if loader.is_null() {
            return Err(Error::new(E_FAIL, "MFXLoad failed"));
        }
        // From here on, Drop cleans up whatever has been created.
        let mut enc = Self {
            api,
            loader,
            session: null_mut(),
            context: gpu.context.clone(),
            slots: Vec::new(),
            free: Vec::new(),
            in_flight: VecDeque::new(),
            frame_duration: 10_000_000 / cfg.fps as i64,
            av1: cfg.codec == Codec::Av1,
            name: String::new(),
            rate_control: "",
        };
        enc.open_session(gpu)?;
        enc.init(cfg)?;
        Ok(enc)
    }

    fn open_session(&mut self, gpu: &Gpu) -> Result<()> {
        let api = &self.api;
        unsafe {
            let config = (api.create_config)(self.loader);
            let filter = |name: &[u8], value: u32| {
                let v = mfxVariant {
                    Version: 0,
                    Type: MFX_VARIANT_TYPE_U32,
                    Data: value as u64,
                };
                check(
                    (api.set_filter)(config, name.as_ptr(), v),
                    "MFXSetConfigFilterProperty",
                )
            };
            filter(b"mfxImplDescription.Impl\0", MFX_IMPL_TYPE_HARDWARE)?;
            filter(
                b"mfxImplDescription.AccelerationMode\0",
                MFX_ACCEL_MODE_VIA_D3D11,
            )?;
            check(
                (api.create_session)(self.loader, 0, &mut self.session),
                "MFXCreateSession",
            )?;
            // The encoder must work on the recorder's own device so textures are shared, not copied.
            check(
                (api.set_handle)(self.session, MFX_HANDLE_D3D11_DEVICE, gpu.device.as_raw()),
                "MFXVideoCORE_SetHandle",
            )?;
            if std::env::var_os("ARCHY_VPL_TRACE").is_some() {
                let mut v = 0u32;
                (api.query_version)(self.session, &mut v);
                eprintln!("  vpl runtime API {}.{}", v >> 16, v & 0xffff);
            }
        }
        Ok(())
    }

    /// Tries the cheapest configuration first: fixed-function low-power encode with
    /// constant quality, then falls back step by step.
    fn init(&mut self, cfg: &Config) -> Result<()> {
        let all = [
            (true, true, "constant quality, low-power"),
            (true, false, "VBR, low-power"),
            (false, true, "constant quality"),
            (false, false, "VBR"),
        ];
        // A bitrate target means VBR only; constant quality is the fallback-free default otherwise.
        let attempts: Vec<_> = all
            .into_iter()
            .filter(|(_, icq, _)| cfg.target_bitrate.is_none() || !icq)
            .collect();
        let mut last = MFX_ERR_NONE;
        for (low_power, icq, label) in attempts {
            let mut par = video_param(cfg, low_power, icq);
            let status = unsafe { (self.api.init)(self.session, &mut par) };
            if status >= MFX_ERR_NONE {
                self.rate_control = label;
                self.name = format!(
                    "Intel Quick Sync via oneVPL ({})",
                    if low_power { "VDEnc" } else { "VME" }
                );
                let mut actual: mfxVideoParam = unsafe { std::mem::zeroed() };
                unsafe { (self.api.get_video_param)(self.session, &mut actual) };
                let mult = actual.mfx.BRCParamMultiplier.max(1) as usize;
                let size = (actual.mfx.BufferSizeInKB as usize * 1000 * mult)
                    .max(cfg.width as usize * cfg.height as usize * 3 / 2);
                self.slots = (0..PIPELINE).map(|_| Slot::new(size)).collect();
                self.free = (0..PIPELINE).collect();
                return Ok(());
            }
            last = status;
            unsafe { (self.api.encode_close)(self.session) };
        }
        check(last, "MFXVideoENCODE_Init").map(|_| ())
    }

    /// Takes an input surface from the runtime's pool and GPU-copies the frame into it.
    fn input_surface(&mut self, src: &ID3D11Texture2D) -> Result<*mut mfxFrameSurface1> {
        let mut surface = null_mut();
        check(
            unsafe { (self.api.get_surface)(self.session, &mut surface) },
            "GetSurfaceForEncode",
        )?;
        let mut resource: mfxHDL = null_mut();
        let mut kind = 0u32;
        let status = unsafe {
            ((*(*surface).FrameInterface).GetNativeHandle)(surface, &mut resource, &mut kind)
        };
        if let Err(e) = check(status, "GetNativeHandle") {
            unsafe { ((*(*surface).FrameInterface).Release)(surface) };
            return Err(e);
        }
        unsafe {
            let dst = ID3D11Texture2D::from_raw_borrowed(&resource).expect("null surface texture");
            let (mut d, mut s) = (
                D3D11_TEXTURE2D_DESC::default(),
                D3D11_TEXTURE2D_DESC::default(),
            );
            dst.GetDesc(&mut d);
            src.GetDesc(&mut s);
            let region = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: d.Width.min(s.Width),
                bottom: d.Height.min(s.Height),
                back: 1,
            };
            self.context
                .CopySubresourceRegion(dst, 0, 0, 0, 0, src, 0, Some(&region));
        }
        Ok(surface)
    }

    /// Submits one frame, or `null` to flush the encoder. Returns whether the
    /// runtime produced an output task (always true for a frame it accepted).
    fn submit(
        &mut self,
        surface: *mut mfxFrameSurface1,
        on_packet: &mut impl FnMut(Packet) -> Result<()>,
        stats: &mut Stats,
    ) -> Result<bool> {
        if self.free.is_empty() {
            self.finish_oldest(on_packet, stats)?;
        }
        let slot = self.free.pop().expect("a slot was just freed");
        let mut sync: mfxSyncPoint = null_mut();
        let t_submit = trace::now();
        let status = loop {
            let bs = &mut self.slots[slot].bitstream;
            let s = unsafe { (self.api.encode)(self.session, null_mut(), surface, bs, &mut sync) };
            if s == MFX_WRN_DEVICE_BUSY {
                // GPU queue full: collect a finished frame instead of sleeping, if there is one.
                if !self.in_flight.is_empty() {
                    self.finish_oldest(on_packet, stats)?;
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
                continue;
            }
            break s;
        };
        trace::add(0, t_submit);
        if status == MFX_ERR_MORE_DATA || (status >= MFX_ERR_NONE && sync.is_null()) {
            self.free.push(slot);
            return Ok(false);
        }
        if let Err(e) = check(status, "EncodeFrameAsync") {
            self.free.push(slot);
            return Err(e);
        }
        self.in_flight.push_back((sync, slot));
        Ok(true)
    }

    /// Waits for the oldest in-flight frame and hands its packet to `on_packet`.
    fn finish_oldest(
        &mut self,
        on_packet: &mut impl FnMut(Packet) -> Result<()>,
        stats: &mut Stats,
    ) -> Result<()> {
        let Some((sync, slot)) = self.in_flight.pop_front() else {
            return Ok(());
        };
        let t_sync = trace::now();
        // "Still encoding" is not "done": reading the slot now would store a
        // half-written frame, and reusing it would let the GPU overwrite the next
        // one (both corrupt every frame until the next keyframe). Keep waiting;
        // only a frame stuck for 10 s is an error.
        let mut status = MFX_WRN_IN_EXECUTION;
        for _ in 0..10 {
            status = unsafe { (self.api.sync)(self.session, sync, 1000) };
            if status != MFX_WRN_IN_EXECUTION {
                break;
            }
            stats.late += 1;
        }
        trace::add(1, t_sync);
        self.free.push(slot);
        if status == MFX_WRN_IN_EXECUTION {
            return Err(Error::new(E_FAIL, "the GPU encoder stopped responding (a frame took over 10 s)"));
        }
        check(status, "SyncOperation")?;

        let bs = &mut self.slots[slot].bitstream;
        let data = unsafe {
            std::slice::from_raw_parts(bs.Data.add(bs.DataOffset as usize), bs.DataLength as usize)
        };
        let packet = Packet {
            data,
            // oneVPL timestamps are 90 kHz.
            time: (bs.TimeStamp as i64) * 1000 / 9,
            duration: self.frame_duration,
            // IDR only: a plain I-frame (e.g. one inserted on a scene change) can be
            // followed by frames that reference pictures before it.
            keyframe: if self.av1 {
                bs.FrameType & (MFX_FRAMETYPE_IDR | MFX_FRAMETYPE_I) != 0
            } else {
                bs.FrameType & MFX_FRAMETYPE_IDR != 0
            },
        };
        stats.count(&packet);
        let result = on_packet(packet);
        bs.DataOffset = 0;
        bs.DataLength = 0;
        result
    }

    pub fn run(
        &mut self,
        inputs: Receiver<SendPtr<Input>>,
        mut on_packet: impl FnMut(Packet) -> Result<()>,
    ) -> Result<Stats> {
        let mut stats = Stats::default();
        while let Ok(SendPtr(input)) = inputs.recv() {
            let t_copy = trace::now();
            let surface = self.input_surface(&input.texture)?;
            trace::add(2, t_copy);
            unsafe { (*surface).Data.TimeStamp = (input.time * 9 / 1000) as u64 };
            let result = self.submit(surface, &mut on_packet, &mut stats);
            // The encoder holds its own reference while it needs the frame.
            unsafe { ((*(*surface).FrameInterface).Release)(surface) };
            result?;
            stats.frames_in += 1;
        }
        // Flush frames the runtime still holds, then collect everything in flight.
        while self.submit(null_mut(), &mut on_packet, &mut stats)? {}
        while !self.in_flight.is_empty() {
            self.finish_oldest(&mut on_packet, &mut stats)?;
        }
        trace::report(stats.frames_in);
        Ok(stats)
    }
}

impl Drop for VplEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.session.is_null() {
                (self.api.encode_close)(self.session);
                (self.api.close)(self.session);
            }
            (self.api.unload)(self.loader);
            let _ = FreeLibrary(self.api.lib);
        }
    }
}

/// Profiling of the encode loop (set ARCHY_VPL_TRACE=1): wall and thread-CPU
/// time spent in the input copy, submit and sync.
mod trace {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};

    thread_local! {
        static ACC: RefCell<[(Duration, Duration); 3]> = const { RefCell::new([(Duration::ZERO, Duration::ZERO); 3]) };
    }

    fn cpu() -> Duration {
        let (mut c, mut e, mut k, mut u) = <(FILETIME, FILETIME, FILETIME, FILETIME)>::default();
        unsafe {
            let _ = GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u);
        }
        let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        Duration::from_nanos((t(k) + t(u)) * 100)
    }

    pub fn now() -> (Instant, Duration) {
        (Instant::now(), cpu())
    }

    pub fn add(slot: usize, start: (Instant, Duration)) {
        let (wall, cpu_used) = (start.0.elapsed(), cpu().saturating_sub(start.1));
        ACC.with(|a| {
            let mut a = a.borrow_mut();
            a[slot].0 += wall;
            a[slot].1 += cpu_used;
        });
    }

    pub fn report(frames: u64) {
        if std::env::var_os("ARCHY_VPL_TRACE").is_none() {
            return;
        }
        let n = frames.max(1) as f64;
        ACC.with(|a| {
            for (name, (wall, cpu_used)) in ["submit", "sync", "copy"].iter().zip(a.borrow().iter())
            {
                eprintln!(
                    "  vpl {name:<6} wall {:.2} ms/frame, thread CPU {:.2} ms/frame",
                    wall.as_secs_f64() * 1000.0 / n,
                    cpu_used.as_secs_f64() * 1000.0 / n
                );
            }
        });
    }
}
