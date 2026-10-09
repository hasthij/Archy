//! Intel oneVPL (Quick Sync) encoder, driven directly.
//!
//! The dispatcher `libvpl.dll` ships with Intel's graphics driver (System32) and
//! is loaded at runtime, so non-Intel PCs simply don't offer this backend.
//! The recorder's NV12 textures are imported zero-copy with
//! `mfxMemoryInterface::ImportFrameSurface`, and the encoder runs on the
//! fixed-function low-power path (VDEnc) when the GPU has one.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use archy_gpu::device::Gpu;
use archy_gpu::probe::Codec;
use archy_gpu::SendPtr;
use windows::core::{s, w, Error, Interface, Result};
use windows::Win32::Foundation::{FreeLibrary, E_FAIL, HMODULE};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
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
    pub const MFX_WRN_DEVICE_BUSY: mfxStatus = 2;

    pub const MFX_VARIANT_TYPE_U32: u32 = 5;
    pub const MFX_IMPL_TYPE_HARDWARE: u32 = 0x0002;
    pub const MFX_ACCEL_MODE_VIA_D3D11: u32 = 0x0300;
    pub const MFX_HANDLE_D3D11_DEVICE: u32 = 3;
    pub const MFX_HANDLE_MEMORY_INTERFACE: u32 = 1001;

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

    pub const MFX_SURFACE_TYPE_D3D11_TEX2D: u32 = 2;
    pub const MFX_SURFACE_FLAG_IMPORT_SHARED: u32 = 0x0010;
    pub const MFX_SURFACE_FLAG_IMPORT_COPY: u32 = 0x0020;
    pub const MFX_SURFACE_COMPONENT_ENCODE: u32 = 1;

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
        // GetRefCounter, Map, Unmap, GetNativeHandle, GetDeviceHandle,
        // Synchronize, OnComplete, QueryInterface, reserved2[2]: unused.
        pub _rest: [*mut c_void; 10],
    }

    #[repr(C)]
    pub struct mfxSurfaceHeader {
        pub SurfaceType: u32,
        pub SurfaceFlags: u32,
        pub StructSize: u32,
        pub NumExtParam: u16,
        pub ExtParam: *mut c_void,
        pub reserved: [u32; 6],
    }

    #[repr(C)]
    pub struct mfxSurfaceInterface {
        pub Header: mfxSurfaceHeader,
        pub Version: u16,
        pub Context: mfxHDL,
        pub AddRef: *mut c_void,
        pub Release: *mut c_void,
        pub GetRefCounter: *mut c_void,
        pub Synchronize: *mut c_void,
        pub reserved: [*mut c_void; 11],
    }

    #[repr(C)]
    pub struct mfxSurfaceD3D11Tex2D {
        pub SurfaceInterface: mfxSurfaceInterface,
        pub texture2D: mfxHDL,
        pub reserved: [mfxHDL; 7],
    }

    #[repr(C)]
    pub struct mfxMemoryInterface {
        pub Context: mfxHDL,
        pub Version: u16,
        pub ImportFrameSurface: unsafe extern "C" fn(
            *mut mfxMemoryInterface,
            u32,
            *mut mfxSurfaceHeader,
            *mut *mut mfxFrameSurface1,
        ) -> mfxStatus,
        pub GetBitstreamBuffer: *mut c_void,
        pub reserved: [mfxHDL; 15],
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
        assert!(
            size_of::<mfxSurfaceHeader>() == 48 && offset_of!(mfxSurfaceHeader, ExtParam) == 16
        );
        assert!(
            size_of::<mfxSurfaceInterface>() == 184
                && offset_of!(mfxSurfaceInterface, Context) == 56
        );
        assert!(
            size_of::<mfxSurfaceD3D11Tex2D>() == 248
                && offset_of!(mfxSurfaceD3D11Tex2D, texture2D) == 184
        );
        assert!(offset_of!(mfxMemoryInterface, ImportFrameSurface) == 16);
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
type FnGetHandle = unsafe extern "C" fn(mfxSession, u32, *mut mfxHDL) -> mfxStatus;
type FnParam = unsafe extern "C" fn(mfxSession, *mut mfxVideoParam) -> mfxStatus;
type FnEncode = unsafe extern "C" fn(
    mfxSession,
    *mut c_void,
    *mut mfxFrameSurface1,
    *mut mfxBitstream,
    *mut mfxSyncPoint,
) -> mfxStatus;
type FnEncodeClose = unsafe extern "C" fn(mfxSession) -> mfxStatus;
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
    get_handle: FnGetHandle,
    init: FnParam,
    get_video_param: FnParam,
    encode: FnEncode,
    encode_close: FnEncodeClose,
    sync: FnSync,
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
            get_handle: sym!("MFXVideoCORE_GetHandle"),
            init: sym!("MFXVideoENCODE_Init"),
            get_video_param: sym!("MFXVideoENCODE_GetVideoParam"),
            encode: sym!("MFXVideoENCODE_EncodeFrameAsync"),
            encode_close: sym!("MFXVideoENCODE_Close"),
            sync: sym!("MFXVideoCORE_SyncOperation"),
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
        ..Default::default()
    };
    if icq {
        mfx.RateControlMethod = MFX_RATECONTROL_ICQ;
        mfx.TargetKbps = (51 - cfg.quality.min(100) * 2 / 5) as u16;
        mfx.MaxKbps = 0;
    } else {
        mfx.RateControlMethod = MFX_RATECONTROL_VBR;
        mfx.TargetKbps = (max_kbps * 7 / 10 / mult) as u16;
        mfx.MaxKbps = (max_kbps / mult) as u16;
    }
    mfxVideoParam {
        AllocId: 0,
        reserved: [0; 2],
        reserved3: 0,
        // One frame in flight: lowest latency and memory, and the recorder
        // submits frames one at a time anyway.
        AsyncDepth: 1,
        mfx,
        _vpp_tail: [0; 32],
        Protected: 0,
        IOPattern: MFX_IOPATTERN_IN_VIDEO_MEMORY,
        ExtParam: null_mut(),
        NumExtParam: 0,
        reserved2: 0,
    }
}

pub struct VplEncoder {
    api: Api,
    loader: mfxLoader,
    session: mfxSession,
    memory: *mut mfxMemoryInterface,
    bitstream: mfxBitstream,
    buffer: Vec<u8>,
    import_flags: u32,
    frame_duration: i64,
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
            memory: null_mut(),
            bitstream: unsafe { std::mem::zeroed() },
            buffer: Vec::new(),
            import_flags: MFX_SURFACE_FLAG_IMPORT_SHARED,
            frame_duration: 10_000_000 / cfg.fps as i64,
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
            let mut memory: mfxHDL = null_mut();
            check(
                (api.get_handle)(self.session, MFX_HANDLE_MEMORY_INTERFACE, &mut memory),
                "MFXGetMemoryInterface (runtime too old for zero-copy import)",
            )?;
            self.memory = memory as *mut mfxMemoryInterface;
        }
        Ok(())
    }

    /// Tries the cheapest configuration first: fixed-function low-power encode with
    /// constant quality, then falls back step by step.
    fn init(&mut self, cfg: &Config) -> Result<()> {
        let attempts = [
            (true, true, "constant quality, low-power"),
            (true, false, "VBR, low-power"),
            (false, true, "constant quality"),
            (false, false, "VBR"),
        ];
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
                self.buffer = vec![0u8; size];
                self.bitstream.Data = self.buffer.as_mut_ptr();
                self.bitstream.MaxLength = size as u32;
                return Ok(());
            }
            last = status;
            unsafe { (self.api.encode_close)(self.session) };
        }
        check(last, "MFXVideoENCODE_Init").map(|_| ())
    }

    /// Wraps the texture as an encoder surface without copying it.
    fn import(&mut self, texture: &ID3D11Texture2D) -> Result<*mut mfxFrameSurface1> {
        loop {
            let mut desc: mfxSurfaceD3D11Tex2D = unsafe { std::mem::zeroed() };
            desc.SurfaceInterface.Header.SurfaceType = MFX_SURFACE_TYPE_D3D11_TEX2D;
            desc.SurfaceInterface.Header.SurfaceFlags = self.import_flags;
            desc.SurfaceInterface.Header.StructSize =
                std::mem::size_of::<mfxSurfaceD3D11Tex2D>() as u32;
            desc.SurfaceInterface.Version = 1 << 8; // 1.0
            desc.texture2D = texture.as_raw();
            let mut surface = null_mut();
            let status = unsafe {
                ((*self.memory).ImportFrameSurface)(
                    self.memory,
                    MFX_SURFACE_COMPONENT_ENCODE,
                    &mut desc.SurfaceInterface.Header,
                    &mut surface,
                )
            };
            if status >= MFX_ERR_NONE {
                return Ok(surface);
            }
            if self.import_flags == MFX_SURFACE_FLAG_IMPORT_SHARED {
                // Driver can't map our texture directly: fall back to a GPU-side copy.
                self.import_flags = MFX_SURFACE_FLAG_IMPORT_COPY;
                continue;
            }
            return check(status, "ImportFrameSurface").map(|_| null_mut());
        }
    }

    /// Submits one frame (or `None` to drain) and emits a packet if one is ready.
    fn encode(
        &mut self,
        surface: *mut mfxFrameSurface1,
        on_packet: &mut impl FnMut(Packet) -> Result<()>,
        stats: &mut Stats,
    ) -> Result<bool> {
        let mut sync: mfxSyncPoint = null_mut();
        let status = loop {
            let s = unsafe {
                (self.api.encode)(
                    self.session,
                    null_mut(),
                    surface,
                    &mut self.bitstream,
                    &mut sync,
                )
            };
            if s == MFX_WRN_DEVICE_BUSY {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            break s;
        };
        if status == MFX_ERR_MORE_DATA {
            return Ok(false);
        }
        check(status, "EncodeFrameAsync")?;
        if sync.is_null() {
            return Ok(false);
        }
        check(
            unsafe { (self.api.sync)(self.session, sync, 1000) },
            "SyncOperation",
        )?;

        let bs = &mut self.bitstream;
        let data = &self.buffer[bs.DataOffset as usize..(bs.DataOffset + bs.DataLength) as usize];
        let packet = Packet {
            data,
            // oneVPL timestamps are 90 kHz.
            time: (bs.TimeStamp as i64) * 1000 / 9,
            duration: self.frame_duration,
            keyframe: bs.FrameType & (MFX_FRAMETYPE_IDR | MFX_FRAMETYPE_I) != 0,
        };
        stats.count(&packet);
        on_packet(packet)?;
        bs.DataOffset = 0;
        bs.DataLength = 0;
        Ok(true)
    }

    pub fn run(
        &mut self,
        inputs: Receiver<SendPtr<Input>>,
        mut on_packet: impl FnMut(Packet) -> Result<()>,
    ) -> Result<Stats> {
        let mut stats = Stats::default();
        while let Ok(SendPtr(input)) = inputs.recv() {
            let surface = self.import(&input.texture)?;
            unsafe { (*surface).Data.TimeStamp = (input.time * 9 / 1000) as u64 };
            let result = self.encode(surface, &mut on_packet, &mut stats);
            // The encoder holds its own reference while it needs the frame.
            unsafe { ((*(*surface).FrameInterface).Release)(surface) };
            result?;
            stats.frames_in += 1;
        }
        // Drain anything still buffered.
        while self.encode(null_mut(), &mut on_packet, &mut stats)? {}
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
