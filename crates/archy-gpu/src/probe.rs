use std::fmt;

use windows::core::{Result, GUID, PWSTR};
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_ERROR_NOT_FOUND,
};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFAttributes, MFCreateAttributes, MFMediaType_Video, MFShutdown, MFStartup,
    MFTEnum2, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_AV1, MFVideoFormat_H264,
    MFVideoFormat_HEVC, MFSTARTUP_FULL, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_ADAPTER_LUID,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

const VENDOR_NVIDIA: u32 = 0x10DE;
const VENDOR_AMD: u32 = 0x1002;
const VENDOR_INTEL: u32 = 0x8086;

/// Below this much dedicated VRAM an Intel/AMD adapter is treated as integrated.
const IGPU_VRAM_LIMIT: usize = 512 * 1024 * 1024;

/// AMD APUs reserve up to 2 GB of system RAM as "dedicated" memory, so a
/// discrete AMD card is only assumed from more than that (smallest is 2 GB+).
const AMD_APU_VRAM_LIMIT: usize = 1024 * 1024 * 1024;

fn is_integrated(vendor_id: u32, dedicated_vram: usize) -> bool {
    match vendor_id {
        VENDOR_INTEL => dedicated_vram < IGPU_VRAM_LIMIT,
        VENDOR_AMD => dedicated_vram <= AMD_APU_VRAM_LIMIT,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::Av1, Codec::Hevc, Codec::H264];

    pub fn subtype(self) -> GUID {
        match self {
            Codec::H264 => MFVideoFormat_H264,
            Codec::Hevc => MFVideoFormat_HEVC,
            Codec::Av1 => MFVideoFormat_AV1,
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Encoder {
    pub codec: Codec,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Display {
    pub device_name: String,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone)]
pub struct Adapter {
    pub index: u32,
    pub name: String,
    pub vendor_id: u32,
    /// "vvvv:dddd" (PCI vendor and device id): stable across reboots, unlike the LUID.
    pub id: String,
    pub luid: LUID,
    pub dedicated_vram: usize,
    pub shared_memory: usize,
    pub integrated: bool,
    pub displays: Vec<Display>,
    pub encoders: Vec<Encoder>,
}

impl Adapter {
    pub fn vendor(&self) -> &'static str {
        match self.vendor_id {
            VENDOR_NVIDIA => "NVIDIA",
            VENDOR_AMD => "AMD",
            VENDOR_INTEL => "Intel",
            _ => "Other",
        }
    }

    pub fn best_codec(&self) -> Option<Codec> {
        Codec::ALL
            .into_iter()
            .find(|c| self.encoders.iter().any(|e| e.codec == *c))
    }
}

/// Lists hardware adapters (software rasterizers skipped) with their displays and encoders.
pub fn probe() -> Result<Vec<Adapter>> {
    unsafe {
        // Without this, display sizes are scaled by Windows DPI (e.g. 1920x1080 at 125% reads as 1536x864).
        // Fails harmlessly if awareness was already set (e.g. by a manifest).
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // S_FALSE (already initialized) is fine; only hard failures matter.
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    let result = probe_adapters();
    unsafe {
        let _ = MFShutdown();
    }
    result
}

fn probe_adapters() -> Result<Vec<Adapter>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut adapters = Vec::new();
    for index in 0.. {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e),
        };
        let desc = unsafe { adapter.GetDesc1()? };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let integrated = is_integrated(desc.VendorId, desc.DedicatedVideoMemory);
        adapters.push(Adapter {
            index,
            name: wide_to_string(&desc.Description),
            vendor_id: desc.VendorId,
            id: format!("{:04x}:{:04x}", desc.VendorId, desc.DeviceId),
            luid: desc.AdapterLuid,
            dedicated_vram: desc.DedicatedVideoMemory,
            shared_memory: desc.SharedSystemMemory,
            integrated,
            displays: displays(&adapter)?,
            encoders: encoders(desc.AdapterLuid)?,
        });
    }
    Ok(adapters)
}

fn displays(adapter: &IDXGIAdapter1) -> Result<Vec<Display>> {
    let mut out = Vec::new();
    for i in 0.. {
        let output = match unsafe { adapter.EnumOutputs(i) } {
            Ok(o) => o,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e),
        };
        let desc = unsafe { output.GetDesc()? };
        if !desc.AttachedToDesktop.as_bool() {
            continue;
        }
        let r = desc.DesktopCoordinates;
        out.push(Display {
            device_name: wide_to_string(&desc.DeviceName),
            width: r.right - r.left,
            height: r.bottom - r.top,
        });
    }
    Ok(out)
}

/// Hardware encoder MFTs bound to one adapter, for every codec Archy can record in.
fn encoders(luid: LUID) -> Result<Vec<Encoder>> {
    let mut found: Vec<Encoder> = Vec::new();
    for codec in Codec::ALL {
        for activate in encoder_activates(luid, codec)? {
            let name = friendly_name(&activate);
            // Drivers can register the same MFT more than once.
            if !found.iter().any(|e| e.codec == codec && e.name == name) {
                found.push(Encoder { codec, name });
            }
        }
    }
    Ok(found)
}

/// Hardware encoder MFTs on the adapter with `luid` that output `codec`, best first.
/// Media Foundation must be started.
pub fn encoder_activates(luid: LUID, codec: Codec) -> Result<Vec<IMFActivate>> {
    let mut attrs: Option<IMFAttributes> = None;
    unsafe { MFCreateAttributes(&mut attrs, 1)? };
    let attrs = attrs.expect("MFCreateAttributes succeeded without attributes");
    let luid_bytes = unsafe {
        std::slice::from_raw_parts(
            &luid as *const LUID as *const u8,
            std::mem::size_of::<LUID>(),
        )
    };
    unsafe { attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, luid_bytes)? };

    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: codec.subtype(),
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output),
            &attrs,
            &mut list,
            &mut count,
        )?;
    }
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // Take ownership so each activate is released when dropped.
        if let Some(a) = unsafe { std::ptr::read(list.add(i)) } {
            out.push(a);
        }
    }
    if !list.is_null() {
        unsafe { CoTaskMemFree(Some(list as *const _)) };
    }
    Ok(out)
}

pub fn friendly_name(activate: &IMFActivate) -> String {
    let mut name = PWSTR::null();
    let mut len = 0u32;
    unsafe {
        if activate
            .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name, &mut len)
            .is_err()
        {
            return "(unnamed)".into();
        }
        let s = name.to_string().unwrap_or_default();
        CoTaskMemFree(Some(name.0 as *const _));
        s
    }
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;

    fn sim(vendor_id: u32, vram_mb: usize, codecs: &[Codec]) -> Adapter {
        Adapter {
            index: 0,
            name: "sim".into(),
            vendor_id,
            id: format!("{vendor_id:04x}:0000"),
            luid: LUID::default(),
            dedicated_vram: vram_mb * MB,
            shared_memory: 0,
            integrated: is_integrated(vendor_id, vram_mb * MB),
            displays: Vec::new(),
            encoders: codecs.iter().map(|&codec| Encoder { codec, name: "sim".into() }).collect(),
        }
    }

    #[test]
    fn intel_igpu() {
        let a = sim(VENDOR_INTEL, 128, &[Codec::H264, Codec::Hevc]);
        assert!(a.integrated && a.vendor() == "Intel");
        assert_eq!(a.best_codec(), Some(Codec::Hevc));
        assert!(!sim(VENDOR_INTEL, 8192, &[Codec::Av1]).integrated); // Arc dGPU
    }

    #[test]
    fn nvidia_with_codecs() {
        let a = sim(VENDOR_NVIDIA, 8192, &[Codec::H264, Codec::Hevc, Codec::Av1]);
        assert!(!a.integrated && a.vendor() == "NVIDIA");
        assert_eq!(a.best_codec(), Some(Codec::Av1));
        assert!(!sim(VENDOR_NVIDIA, 0, &[]).integrated); // never "integrated"
    }

    #[test]
    fn nvidia_without_codec() {
        let a = sim(VENDOR_NVIDIA, 2048, &[]);
        assert_eq!(a.best_codec(), None);
    }

    #[test]
    fn amd_apu_and_dgpu() {
        for mb in [64, 512, 1024] {
            assert!(sim(VENDOR_AMD, mb, &[Codec::H264]).integrated, "{mb} MB APU");
        }
        assert!(!sim(VENDOR_AMD, 2048, &[Codec::H264]).integrated);
        assert!(!sim(VENDOR_AMD, 16384, &[Codec::H264, Codec::Av1]).integrated);
        assert_eq!(sim(VENDOR_AMD, 512, &[Codec::H264]).best_codec(), Some(Codec::H264));
    }
}
