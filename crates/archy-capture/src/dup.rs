//! DXGI Desktop Duplication of one monitor, with the mouse cursor drawn in.
//!
//! Used for exclusive-fullscreen games: Windows Graphics Capture can hide the
//! cursor there (a Windows 11 24H2+ bug), duplication never does. Nothing is
//! injected into the game. Each wanted frame is copied once on the GPU into a
//! small pool of our own textures, so the desktop image is released at once.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::core::{Interface, Result};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_GDI_COMPATIBLE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, IDXGISurface1,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{DeleteObject, HGDIOBJ, HMONITOR};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HICON, ICONINFO,
};

use crate::Frame;

/// Texture ring: one being filled, the rest possibly still read by the GPU.
const POOL: usize = 3;
/// Give up (the recording ends and restarts) after the duplication stays broken this long.
const GIVE_UP: Duration = Duration::from_secs(10);

struct Open {
    dupl: IDXGIOutputDuplication,
    size: (u32, u32),
    /// Monitor's top-left on the virtual desktop, and its size there (for the cursor).
    origin: (i32, i32),
    desktop: (i32, i32),
    pool: Vec<ID3D11Texture2D>,
}

struct State {
    open: Option<Open>,
    next: usize,
    last_time: i64,
    /// Slot grid, the same rule the recorder's pacer uses: first frame's time, then one slot per interval.
    origin: i64,
    next_slot: i64,
    broken_since: Option<Instant>,
    /// Cursor handle and its hotspot, so GetIconInfo runs once per cursor shape.
    hotspot: (isize, i32, i32),
}

pub struct Dup {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    monitor: HMONITOR,
    cursor: bool,
    /// Minimum gap between delivered frames, in 100 ns.
    min_interval: i64,
    freq: i64,
    state: RefCell<State>,
    closed: Cell<bool>,
    pub width: u32,
    pub height: u32,
}

fn open(device: &ID3D11Device, monitor: HMONITOR) -> Result<Open> {
    let dxgi: IDXGIDevice = device.cast()?;
    let adapter = unsafe { dxgi.GetAdapter()? };
    let mut i = 0;
    loop {
        // Runs out with DXGI_ERROR_NOT_FOUND when the monitor isn't on this GPU.
        let out = unsafe { adapter.EnumOutputs(i)? };
        let desc = unsafe { out.GetDesc()? };
        i += 1;
        if desc.Monitor != monitor {
            continue;
        }
        let out1: IDXGIOutput1 = out.cast()?;
        let dupl = unsafe { out1.DuplicateOutput(device)? };
        let d = unsafe { dupl.GetDesc() };
        if d.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(E_FAIL.into()); // HDR / 10-bit desktop: let the caller use WGC
        }
        let (w, h) = (d.ModeDesc.Width, d.ModeDesc.Height);
        let r = desc.DesktopCoordinates;
        let tex = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32,
        };
        let mut pool = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            let mut t = None;
            unsafe { device.CreateTexture2D(&tex, None, Some(&mut t))? };
            pool.push(t.ok_or_else(|| windows::core::Error::from(E_FAIL))?);
        }
        return Ok(Open {
            dupl,
            size: (w, h),
            origin: (r.left, r.top),
            desktop: ((r.right - r.left).max(1), (r.bottom - r.top).max(1)),
            pool,
        });
    }
}

/// QPC ticks to 100 ns units without overflowing.
fn to_100ns(ticks: i64, freq: i64) -> i64 {
    (ticks / freq) * 10_000_000 + (ticks % freq) * 10_000_000 / freq
}

impl Dup {
    pub fn start(device: &ID3D11Device, monitor: HMONITOR, cursor: bool, min_interval: Duration) -> Result<Self> {
        let o = open(device, monitor)?;
        let (width, height) = o.size;
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq)? };
        let context = unsafe { device.GetImmediateContext()? };
        Ok(Self {
            device: device.clone(),
            context,
            monitor,
            cursor,
            min_interval: (min_interval.as_nanos() / 100) as i64,
            freq: freq.max(1),
            state: RefCell::new(State {
                open: Some(o),
                next: 0,
                last_time: 0,
                origin: 0,
                next_slot: 0,
                broken_since: None,
                hotspot: (0, 0, 0),
            }),
            closed: Cell::new(false),
            width,
            height,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub fn latest(&self) -> Result<Option<Frame>> {
        self.next(Duration::ZERO)
    }

    /// Waits up to `timeout` for a wanted frame. `None`: nothing new in that time
    /// (or the duplication is being rebuilt after a mode change / alt-tab).
    pub fn next(&self, timeout: Duration) -> Result<Option<Frame>> {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.borrow_mut();
        loop {
            if st.open.is_none() {
                match open(&self.device, self.monitor) {
                    Ok(o) => {
                        st.open = Some(o);
                        st.broken_since = None;
                    }
                    Err(_) => {
                        let since = *st.broken_since.get_or_insert_with(Instant::now);
                        if since.elapsed() > GIVE_UP {
                            self.closed.set(true);
                        }
                        std::thread::sleep(timeout.min(Duration::from_millis(200)));
                        return Ok(None);
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let ms = left.as_micros().div_ceil(1000).max(1) as u32;
            let dupl = st.open.as_ref().expect("opened above").dupl.clone();
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match unsafe { dupl.AcquireNextFrame(ms, &mut info, &mut res) } {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(_) => {
                    // Access lost (mode change, alt-tab, another app took it): rebuild.
                    st.open = None;
                    st.broken_since.get_or_insert_with(Instant::now);
                    return Ok(None);
                }
            }
            st.broken_since = None;
            let t = if info.LastPresentTime != 0 {
                to_100ns(info.LastPresentTime, self.freq)
            } else {
                let mut now = 0i64;
                unsafe { let _ = QueryPerformanceCounter(&mut now); }
                to_100ns(now, self.freq)
            };
            // Cursor-only updates are useless without the cursor; frames closer than
            // the wanted interval would only be thrown away, so skip their copy.
            let cursor_only = info.LastPresentTime == 0;
            // On a fixed grid, not "gap since the last kept frame": that aliased a 144 Hz
            // game down to ~48 fps (every 3rd present) and made the clip judder.
            let slot = if st.last_time == 0 { 0 } else { (t - st.origin + self.min_interval / 2).div_euclid(self.min_interval) };
            let too_soon = st.last_time != 0 && slot < st.next_slot;
            if (cursor_only && !self.cursor) || too_soon {
                unsafe { let _ = dupl.ReleaseFrame(); }
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                continue;
            }
            let copied = (|| -> Result<(ID3D11Texture2D, u32, u32)> {
                let tex: ID3D11Texture2D = res.ok_or_else(|| windows::core::Error::from(E_FAIL))?.cast()?;
                let idx = st.next;
                let o = st.open.as_ref().expect("opened above");
                let dst = o.pool[idx].clone();
                let (w, h) = o.size;
                unsafe { self.context.CopyResource(&dst, &tex) };
                Ok((dst, w, h))
            })();
            unsafe { let _ = dupl.ReleaseFrame(); }
            let (dst, w, h) = copied?;
            st.next = (st.next + 1) % POOL;
            if st.last_time == 0 { st.origin = t; }
            st.next_slot = slot + 1;
            st.last_time = t;
            if self.cursor {
                self.draw_cursor(&mut st, &dst);
            }
            return Ok(Some(Frame { _frame: None, texture: dst, time: t, content_width: w, content_height: h }));
        }
    }

    /// Draws the cursor (when Windows says it's showing) onto `dst`.
    fn draw_cursor(&self, st: &mut State, dst: &ID3D11Texture2D) {
        unsafe {
            let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
            if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
                return;
            }
            let Some(o) = st.open.as_ref() else { return };
            let key = ci.hCursor.0 as isize;
            if st.hotspot.0 != key {
                let mut ii = ICONINFO::default();
                let hot = if GetIconInfo(HICON(ci.hCursor.0), &mut ii).is_ok() {
                    for b in [ii.hbmMask, ii.hbmColor] {
                        if !b.is_invalid() {
                            let _ = DeleteObject(HGDIOBJ(b.0));
                        }
                    }
                    (ii.xHotspot as i32, ii.yHotspot as i32)
                } else {
                    (0, 0)
                };
                st.hotspot = (key, hot.0, hot.1);
            }
            let (hx, hy) = (st.hotspot.1, st.hotspot.2);
            // The desktop rectangle can be DPI-virtualised; scale to the real pixels.
            let sx = o.size.0 as f32 / o.desktop.0 as f32;
            let sy = o.size.1 as f32 / o.desktop.1 as f32;
            let x = ((ci.ptScreenPos.x - o.origin.0) as f32 * sx) as i32 - hx;
            let y = ((ci.ptScreenPos.y - o.origin.1) as f32 * sy) as i32 - hy;
            let Ok(surface) = dst.cast::<IDXGISurface1>() else { return };
            if let Ok(hdc) = surface.GetDC(false) {
                let _ = DrawIconEx(hdc, x, y, HICON(ci.hCursor.0), 0, 0, 0, None, DI_NORMAL);
                let _ = surface.ReleaseDC(None);
            }
        }
    }
}
