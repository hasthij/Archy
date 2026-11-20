//! Archy's own notification: a small dark card with the app icon, bottom-right of the screen.
//! Replaces the Windows balloon (bulky, old icon, wrong style). One window at a time; a new
//! notification replaces the text of the one on screen. Never takes focus, so it can't
//! pull you out of a game. Drawn once into a 32-bit bitmap, so it costs nothing while shown.

use std::sync::Mutex;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_NEW: u32 = WM_APP + 40;
const TIMER: usize = 1;
const TICK_MS: u32 = 16;
const HOLD_MS: i32 = 5000;

struct Content {
    title: String,
    text: String,
    warning: bool,
}

struct State {
    hwnd: isize, // 0 = none, -1 = starting
    content: Content,
    alpha: i32,
    phase: u8, // 0 fading in, 1 holding, 2 fading out
    hold: i32,
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    dc: isize,
    bmp: isize,
}

static STATE: Mutex<State> = Mutex::new(State {
    hwnd: 0,
    content: Content { title: String::new(), text: String::new(), warning: false },
    alpha: 0,
    phase: 0,
    hold: 0,
    w: 0,
    h: 0,
    x: 0,
    y: 0,
    dc: 0,
    bmp: 0,
});

/// Shows a notification. Safe to call from any thread.
pub fn show(title: &str, text: &str, warning: bool) {
    let mut s = STATE.lock().unwrap();
    s.content = Content { title: title.to_string(), text: text.to_string(), warning };
    match s.hwnd {
        -1 => {}
        0 => {
            s.hwnd = -1;
            drop(s);
            std::thread::spawn(run);
        }
        h => unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_NEW, WPARAM(0), LPARAM(0));
        },
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn run() {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: inst.into(),
            lpszClassName: w!("ArchyToast"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("ArchyToast"),
            w!("Archy notification"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(inst.into()),
            None,
        );
        let Ok(hwnd) = hwnd else {
            STATE.lock().unwrap().hwnd = 0;
            return;
        };
        STATE.lock().unwrap().hwnd = hwnd.0 as isize;
        render(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER, TICK_MS, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NEW => {
            render(hwnd);
            LRESULT(0)
        }
        WM_TIMER => {
            let done = {
                let mut s = STATE.lock().unwrap();
                match s.phase {
                    0 => {
                        s.alpha = (s.alpha + 40).min(255);
                        if s.alpha >= 255 {
                            s.phase = 1;
                        }
                    }
                    1 => {
                        s.hold -= TICK_MS as i32;
                        if s.hold <= 0 {
                            s.phase = 2;
                        }
                    }
                    _ => s.alpha = (s.alpha - 22).max(0),
                }
                s.phase == 2 && s.alpha == 0
            };
            if done {
                let _ = DestroyWindow(hwnd);
            } else {
                present(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            STATE.lock().unwrap().phase = 2;
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), TIMER);
            {
                let mut s = STATE.lock().unwrap();
                if s.dc != 0 {
                    let _ = DeleteDC(HDC(s.dc as *mut _));
                }
                if s.bmp != 0 {
                    let _ = DeleteObject(HGDIOBJ(s.bmp as *mut _));
                }
                s.dc = 0;
                s.bmp = 0;
                s.hwnd = 0;
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Puts the finished bitmap on screen with the current fade and slide.
unsafe fn present(hwnd: HWND) {
    let s = STATE.lock().unwrap();
    if s.dc == 0 {
        return;
    }
    let lift = ((255 - s.alpha) * 12 * (GetDpiForSystem() as i32) / 96) / 255;
    let pos = POINT { x: s.x, y: s.y + lift };
    let size = SIZE { cx: s.w, cy: s.h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: s.alpha as u8, AlphaFormat: AC_SRC_ALPHA as u8 };
    let _ = UpdateLayeredWindow(hwnd, None, Some(&pos), Some(&size), Some(HDC(s.dc as *mut _)), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
}

/// 1 inside the rounded rectangle, 0 outside, soft at the edge.
fn coverage(x: f32, y: f32, w: f32, h: f32, r: f32, inset: f32) -> f32 {
    let (cx, cy) = (x - w / 2.0, y - h / 2.0);
    let (hw, hh) = (w / 2.0 - inset - r, h / 2.0 - inset - r);
    let (dx, dy) = (cx.abs() - hw, cy.abs() - hh);
    let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

fn in_poly(x: f32, y: f32, p: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = p.len() - 1;
    for i in 0..p.len() {
        let ((xi, yi), (xj, yj)) = (p[i], p[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn near(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((x - a.0 - t * dx).powi(2) + (y - a.1 - t * dy).powi(2)).sqrt()
}

/// The Archy icon (Sliced A on its dark tile) drawn at exactly `size` pixels, so it is always sharp.
/// Same shapes as assets/make_icon.py. Writes opaque pixels into `px` (row width `stride`).
fn draw_icon(px: &mut [u32], stride: i32, x0: i32, y0: i32, size: i32) {
    const OUTER: [(f32, f32); 7] = [(12.0, 3.6), (20.6, 20.4), (16.5, 20.4), (15.3, 17.8), (8.7, 17.8), (7.5, 20.4), (3.4, 20.4)];
    const HOLE: [(f32, f32); 3] = [(12.0, 10.2), (13.7, 14.6), (10.3, 14.6)];
    const SS: i32 = 4;
    let k = 24.0 / size as f32;
    for j in 0..size {
        for i in 0..size {
            let (mut rgb, mut cov) = ([0.0f32; 3], 0.0f32);
            for sj in 0..SS {
                for si in 0..SS {
                    let (u, v) = ((i as f32 + (si as f32 + 0.5) / SS as f32) * k, (j as f32 + (sj as f32 + 0.5) / SS as f32) * k);
                    if coverage(u, v, 24.0, 24.0, 5.5, 0.0) < 0.5 {
                        continue;
                    }
                    cov += 1.0;
                    let a = in_poly(u, v, &OUTER) && !in_poly(u, v, &HOLE) && near(u, v, (2.6, 14.8), (21.4, 9.4)) > 0.75;
                    let c = if a { [77.0, 141.0, 255.0] } else if coverage(u, v, 24.0, 24.0, 5.5, 0.6) < 0.5 { [34.0, 42.0, 58.0] } else { [17.0, 19.0, 22.0] };
                    for n in 0..3 {
                        rgb[n] += c[n];
                    }
                }
            }
            let a = cov / (SS * SS) as f32;
            if a > 0.0 {
                let idx = ((y0 + j) * stride + x0 + i) as usize;
                let (br, bg, bb) = ((px[idx] >> 16) & 0xFF, (px[idx] >> 8) & 0xFF, px[idx] & 0xFF);
                let mix = |s: f32, b: u32| (s / cov * a + b as f32 * (1.0 - a)) as u32;
                px[idx] = 0xFF00_0000 | (mix(rgb[0], br) << 16) | (mix(rgb[1], bg) << 8) | mix(rgb[2], bb);
            }
        }
    }
}

/// Draws the card for the current content, sizes and places the window, and starts the fade-in.
unsafe fn render(hwnd: HWND) {
    let dpi = GetDpiForSystem() as i32;
    let sc = |v: i32| v * dpi / 96;
    let (title, text, warning) = {
        let s = STATE.lock().unwrap();
        (s.content.title.clone(), s.content.text.clone(), s.content.warning)
    };
    let (w, pad, ico) = (sc(360), sc(14), sc(40));
    let tx = pad + ico + sc(12);
    let tw = w - tx - pad;

    let screen = GetDC(None);
    let dc = CreateCompatibleDC(Some(screen));
    let font = |h: i32, weight: i32| CreateFontW(-sc(h), 0, 0, 0, weight, 0, 0, 0, FONT_CHARSET(1), FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0), CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
    let (ft, fb) = (font(15, 600), font(13, 400));

    // Measure the text first (up to 3 lines) so the card fits it.
    let mut t_rect = RECT { left: 0, top: 0, right: tw, bottom: 0 };
    let mut wt = wide(&text);
    SelectObject(dc, HGDIOBJ(fb.0));
    DrawTextW(dc, &mut wt, &mut t_rect, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
    let line = sc(18);
    let text_h = if text.is_empty() { 0 } else { t_rect.bottom.min(line * 3) };
    let title_h = sc(20);
    let h = (pad + title_h + sc(2) + text_h + pad).max(pad * 2 + ico);

    // 32-bit bitmap the window will show.
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: 0, ..Default::default() },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();
    SelectObject(dc, HGDIOBJ(bmp.0));
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);

    let (bg, border) = ((20u32, 23u32, 28u32), if warning { (201u32, 137u32, 43u32) } else { (42u32, 48u32, 58u32) });
    let rad = sc(14) as f32;
    for y in 0..h {
        for x in 0..w {
            let edge = coverage(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32, rad, 0.0);
            let inner = coverage(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32, rad, 1.0);
            let c = if inner > 0.5 || edge == 0.0 { bg } else { border };
            let _ = inner;
            px[(y * w + x) as usize] = 0xFF00_0000 | (c.0 << 16) | (c.1 << 8) | c.2;
        }
    }

    // Icon and text.
    draw_icon(px, w, pad, (h - ico) / 2, ico);
    SetBkMode(dc, TRANSPARENT);
    SelectObject(dc, HGDIOBJ(ft.0));
    SetTextColor(dc, COLORREF(0x00FA_F5F2)); // #f2f5fa
    let mut tr = RECT { left: tx, top: pad, right: tx + tw, bottom: pad + title_h };
    let mut wtitle = wide(&title);
    DrawTextW(dc, &mut wtitle, &mut tr, DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
    if !text.is_empty() {
        SelectObject(dc, HGDIOBJ(fb.0));
        SetTextColor(dc, COLORREF(0x00B2_A39A)); // #9aa3b2
        let mut br = RECT { left: tx, top: pad + title_h + sc(2), right: tx + tw, bottom: pad + title_h + sc(2) + text_h };
        DrawTextW(dc, &mut wt, &mut br, DT_WORDBREAK | DT_END_ELLIPSIS | DT_NOPREFIX);
    }
    SelectObject(dc, GetStockObject(SYSTEM_FONT)); // free the fonts for deletion; the bitmap stays selected
    let _ = DeleteObject(HGDIOBJ(ft.0));
    let _ = DeleteObject(HGDIOBJ(fb.0));

    // GDI leaves alpha unset on what it drew: set it from the rounded shape, pre-multiplied.
    for y in 0..h {
        for x in 0..w {
            let a = coverage(x as f32 + 0.5, y as f32 + 0.5, w as f32, h as f32, rad, 0.0);
            let p = px[(y * w + x) as usize];
            let m = |v: u32| ((v & 0xFF) as f32 * a) as u32;
            px[(y * w + x) as usize] = (((a * 255.0) as u32) << 24) | (m(p >> 16) << 16) | (m(p >> 8) << 8) | m(p);
        }
    }

    let mut work = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    {
        let mut s = STATE.lock().unwrap();
        if s.dc != 0 {
            let _ = DeleteDC(HDC(s.dc as *mut _));
        }
        if s.bmp != 0 {
            let _ = DeleteObject(HGDIOBJ(s.bmp as *mut _));
        }
        s.dc = dc.0 as isize;
        s.bmp = bmp.0 as isize;
        s.w = w;
        s.h = h;
        s.x = work.right - w - sc(16);
        s.y = work.bottom - h - sc(16);
        s.alpha = 0;
        s.phase = 0;
        s.hold = HOLD_MS;
    }
    ReleaseDC(None, screen);
    present(hwnd);
}
