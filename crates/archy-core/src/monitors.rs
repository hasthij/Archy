//! The screens connected to this PC, for picking which one to record.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};

#[derive(Clone, Debug, serde::Serialize)]
pub struct Monitor {
    /// Windows' name for it (e.g. `\\.\DISPLAY1`); stable while the PC is set up the same.
    pub id: String,
    /// Shown in the picker, e.g. "Screen 1 · 1920×1080 · main".
    pub label: String,
    #[serde(skip)]
    pub handle: isize,
}

const MONITORINFOF_PRIMARY: u32 = 1;

pub fn list() -> Vec<Monitor> {
    unsafe extern "system" fn visit(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFOEXW {
            monitorInfo: MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(0);
            let id = String::from_utf16_lossy(&info.szDevice[..len]);
            let r = info.monitorInfo.rcMonitor;
            let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
            let label = format!(
                "Screen {} · {}×{}{}",
                out.len() + 1,
                r.right - r.left,
                r.bottom - r.top,
                if primary { " · main" } else { "" }
            );
            out.push(Monitor { id, label, handle: monitor.0 as isize });
        }
        true.into()
    }
    let mut out: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(visit), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

/// Whether HDR is on for the screen `monitor` is on. Capture is 8-bit SDR, so
/// Windows tone-maps an HDR desktop for it and clips can look washed out.
pub fn hdr_on(monitor: HMONITOR) -> bool {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return false;
    };
    for a in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(a) }) else { break };
        for o in 0.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(o) }) else { break };
            let Ok(output) = output.cast::<IDXGIOutput6>() else { continue };
            let Ok(desc) = (unsafe { output.GetDesc1() }) else { continue };
            if desc.Monitor == monitor {
                return desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
            }
        }
    }
    false
}

/// The screen with Windows' name `id`, if it's still connected.
pub fn find(id: &str) -> Option<HMONITOR> {
    if id.is_empty() {
        return None;
    }
    list()
        .into_iter()
        .find(|m| m.id.eq_ignore_ascii_case(id))
        .map(|m| HMONITOR(m.handle as *mut _))
}
