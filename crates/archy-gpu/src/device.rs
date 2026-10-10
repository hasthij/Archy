use windows::core::{Interface, Result};
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory4,
};

/// GPU scheduling priority for the recorder's device. Negative = the game wins
/// every contest for the GPU; the recorder only uses leftover time.
pub const RECORDER_GPU_PRIORITY: i32 = -4;

/// Hardware GPUs to try for recording, after Windows' default: Intel first
/// (Quick Sync is on every Intel chip, and on laptops the Intel GPU drives the
/// screen), then the rest. Laptop GPUs such as NVIDIA's MX series have no video
/// encoder, so when Windows hands Archy one of those it must look further.
pub fn other_adapters(except: LUID) -> Vec<LUID> {
    use windows::Win32::Graphics::Dxgi::{DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_SOFTWARE};
    let mut found: Vec<(bool, LUID)> = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory4>() else {
            return Vec::new();
        };
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else { continue };
            let software = DXGI_ADAPTER_FLAG(desc.Flags as i32) & DXGI_ADAPTER_FLAG_SOFTWARE
                == DXGI_ADAPTER_FLAG_SOFTWARE;
            let same = desc.AdapterLuid.LowPart == except.LowPart
                && desc.AdapterLuid.HighPart == except.HighPart;
            if !software && !same {
                found.push((desc.VendorId == 0x8086, desc.AdapterLuid));
            }
        }
    }
    found.sort_by_key(|&(intel, _)| !intel);
    found.into_iter().map(|(_, luid)| luid).collect()
}

pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub luid: LUID,
}

impl Gpu {
    /// Creates the recorder's own device on the given adapter (or the default one).
    /// `priority`: GPU thread priority (-7..=7); use [`RECORDER_GPU_PRIORITY`] by default.
    pub fn new(adapter_luid: Option<LUID>, priority: i32) -> Result<Self> {
        let adapter: Option<IDXGIAdapter> = match adapter_luid {
            Some(luid) => unsafe {
                let factory: IDXGIFactory4 = CreateDXGIFactory1()?;
                Some(factory.EnumAdapterByLuid::<IDXGIAdapter1>(luid)?.cast()?)
            },
            None => None,
        };
        let levels: [D3D_FEATURE_LEVEL; 2] = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                adapter.as_ref(),
                // An explicit adapter requires UNKNOWN; no adapter means "default hardware".
                if adapter.is_some() {
                    D3D_DRIVER_TYPE_UNKNOWN
                } else {
                    D3D_DRIVER_TYPE_HARDWARE
                },
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device: ID3D11Device = device.expect("D3D11CreateDevice returned no device");
        let context = context.expect("D3D11CreateDevice returned no context");

        // Media Foundation's encoder uses this device from its own threads.
        unsafe {
            let _ = device
                .cast::<ID3D11Multithread>()?
                .SetMultithreadProtected(true);
        }

        let dxgi: IDXGIDevice = device.cast()?;
        unsafe {
            // Best effort: some drivers ignore or refuse priority changes.
            let _ = dxgi.SetGPUThreadPriority(priority);
        }
        let luid = unsafe { dxgi.GetAdapter()?.GetDesc()?.AdapterLuid };
        Ok(Self {
            device,
            context,
            luid,
        })
    }
}
