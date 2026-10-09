//! Windows.Graphics.Capture of a game window or a whole monitor.
//!
//! Anti-cheat safe: nothing is injected into the game. Frames arrive as GPU
//! textures; the recorder pulls only the newest one at its own pace, so a game
//! running at 240 fps costs the same as one at 60.

use std::time::Duration;

use windows::core::{factory, Interface, Result, BOOL};
use windows::Foundation::TimeSpan;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, IsWindowVisible,
};

#[derive(Clone, Copy, Debug)]
pub enum Target {
    Window(HWND),
    Monitor(HMONITOR),
}

/// First visible top-level window whose title contains `needle` (case-insensitive).
pub fn find_window(needle: &str) -> Option<(HWND, String)> {
    struct Search {
        needle: String,
        found: Option<(HWND, String)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = &mut *(lparam.0 as *mut Search);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return true.into();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        if title.to_lowercase().contains(&search.needle) {
            search.found = Some((hwnd, title));
            return false.into();
        }
        true.into()
    }
    let mut search = Search {
        needle: needle.to_lowercase(),
        found: None,
    };
    unsafe {
        // Returns an "error" when the callback stops early; that is the found case.
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

pub fn primary_monitor() -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

pub fn monitor_of(hwnd: HWND) -> HMONITOR {
    unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
}

pub struct Capture {
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    pub width: u32,
    pub height: u32,
}

/// A captured frame. Holding it keeps its buffer out of the pool; drop it right
/// after the GPU work that reads `texture` has been queued.
pub struct Frame {
    _frame: Direct3D11CaptureFrame,
    pub texture: ID3D11Texture2D,
}

impl Capture {
    /// `min_interval`: ask the compositor for at most one frame per interval
    /// (Windows 11 24H2+; ignored on older builds, where the recorder paces itself).
    pub fn start(device: &ID3D11Device, target: Target, min_interval: Duration) -> Result<Self> {
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe {
            match target {
                Target::Window(hwnd) => interop.CreateForWindow(hwnd)?,
                Target::Monitor(mon) => interop.CreateForMonitor(mon)?,
            }
        };
        let size = item.Size()?;

        let dxgi: IDXGIDevice = device.cast()?;
        let winrt_device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()? };

        // Two buffers: one we may be reading, one the compositor can fill. Never more,
        // so a stalled recorder can never make the game wait on capture.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Each of these exists only on newer Windows builds; older ones just skip it.
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetMinUpdateInterval(TimeSpan {
            Duration: (min_interval.as_nanos() / 100) as i64,
        });
        session.StartCapture()?;

        Ok(Self {
            _item: item,
            pool,
            session,
            width: size.Width as u32,
            height: size.Height as u32,
        })
    }

    /// Newest frame since the last call, discarding any older ones; `None` if nothing new.
    pub fn latest(&self) -> Result<Option<Frame>> {
        let mut newest = None;
        // TryGetNextFrame reports "no frame" as an error-shaped null.
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            newest = Some(frame);
        }
        let Some(frame) = newest else { return Ok(None) };
        let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };
        Ok(Some(Frame {
            _frame: frame,
            texture,
        }))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
