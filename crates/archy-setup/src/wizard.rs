//! The installer window: a dark, owner-drawn wizard (Welcome, Installing, Done, Failed).
//! Used for fresh installs, upgrades and the in-app updater (`--update` starts at once).
//! One window procedure, no child controls: everything is painted and hit-tested here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{default_dir, log, perform, registered_dir, Note, Opts};

const BG: u32 = rgb(0x0D, 0x0E, 0x10);
const PANEL: u32 = rgb(0x1A, 0x1C, 0x21);
const LINE: u32 = rgb(0x2A, 0x2D, 0x34);
const TEXT: u32 = rgb(0xE8, 0xEA, 0xEE);
const DIM: u32 = rgb(0x8E, 0x93, 0x9E);
const ACCENT: u32 = rgb(0x4D, 0x8D, 0xFF);
const ACCENT_HI: u32 = rgb(0x6F, 0xA3, 0xFF);
const ERR: u32 = rgb(0xFF, 0x7A, 0x7A);

const W: i32 = 540;
const H: i32 = 380;

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

type R = (i32, i32, i32, i32);

static DPI: AtomicI32 = AtomicI32::new(96);
fn sc(v: i32) -> i32 {
    v * DPI.load(Ordering::Relaxed) / 96
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Welcome,
    Installing,
    Done,
    Failed,
}

struct Ui {
    page: Page,
    dir: PathBuf,
    upgrading: bool,
    desktop: bool,
    startup: bool,
    target: f32,
    shown: f32,
    status: String,
    notes: Vec<Note>,
    err: String,
    hotkey: String,
    hover: u32,
    /// title, body, small, button fonts (HFONT as isize so the state is Send)
    fonts: [isize; 4],
    icon: isize,
}

static UI: Mutex<Ui> = Mutex::new(Ui {
    page: Page::Welcome,
    dir: PathBuf::new(),
    upgrading: false,
    desktop: true,
    startup: true,
    target: 0.0,
    shown: 0.0,
    status: String::new(),
    notes: Vec::new(),
    err: String::new(),
    hotkey: String::new(),
    hover: 0,
    fonts: [0; 4],
    icon: 0,
});

fn ui() -> MutexGuard<'static, Ui> {
    UI.lock().unwrap_or_else(|e| e.into_inner())
}

const PRIMARY: u32 = 1;
const SECOND: u32 = 2;
const CHANGE: u32 = 3;
const CK_DESKTOP: u32 = 4;
const CK_STARTUP: u32 = 5;
const FIX: u32 = 10;

struct Item {
    r: R,
    id: u32,
    label: String,
    /// 0 primary button, 1 ghost button, 2 checkbox, 3 small button
    kind: u8,
    checked: bool,
}

fn items(u: &Ui) -> Vec<Item> {
    let mut v = Vec::new();
    let btn = |r: R, id, label: &str, kind| Item { r, id, label: label.into(), kind, checked: false };
    match u.page {
        Page::Welcome => {
            v.push(btn((378, 326, 508, 362), PRIMARY, if u.upgrading { "Update" } else { "Install" }, 0));
            v.push(btn((266, 326, 366, 362), SECOND, "Cancel", 1));
            if !u.upgrading {
                v.push(btn((418, 190, 508, 222), CHANGE, "Change…", 3));
                v.push(Item { r: (32, 240, 508, 264), id: CK_DESKTOP, label: "Add a desktop shortcut".into(), kind: 2, checked: u.desktop });
                v.push(Item {
                    r: (32, 270, 508, 294),
                    id: CK_STARTUP,
                    label: "Start Archy when I sign in to Windows (recommended)".into(),
                    kind: 2,
                    checked: u.startup,
                });
            }
        }
        Page::Installing => {}
        Page::Done => {
            v.push(btn((378, 326, 508, 362), PRIMARY, "Finish", 0));
            for (i, n) in u.notes.iter().take(3).enumerate() {
                if n.fix.is_some() {
                    let y = 150 + i as i32 * 54;
                    v.push(btn((428, y + 4, 508, y + 32), FIX + i as u32, "Fix", 3));
                }
            }
        }
        Page::Failed => {
            v.push(btn((378, 326, 508, 362), PRIMARY, "Try again", 0));
            v.push(btn((266, 326, 366, 362), SECOND, "Close", 1));
        }
    }
    v
}

fn hit(u: &Ui, x: i32, y: i32) -> u32 {
    for it in items(u) {
        let (l, t, r, b) = it.r;
        if x >= sc(l) && x < sc(r) && y >= sc(t) && y < sc(b) {
            return it.id;
        }
    }
    0
}

fn rect(r: R) -> RECT {
    RECT { left: sc(r.0), top: sc(r.1), right: sc(r.2), bottom: sc(r.3) }
}

unsafe fn text(hdc: HDC, s: &str, r: R, font: isize, col: u32, fmt: DRAW_TEXT_FORMAT) {
    SelectObject(hdc, HGDIOBJ(font as *mut _));
    SetTextColor(hdc, COLORREF(col));
    let mut v: Vec<u16> = s.encode_utf16().collect();
    let mut rc = rect(r);
    DrawTextW(hdc, &mut v, &mut rc, fmt | DT_NOPREFIX);
}

unsafe fn rrect(hdc: HDC, r: R, fill: u32, border: u32, rad: i32) {
    let b = CreateSolidBrush(COLORREF(fill));
    let p = CreatePen(PS_SOLID, 1, COLORREF(border));
    let ob = SelectObject(hdc, b.into());
    let op = SelectObject(hdc, p.into());
    let _ = RoundRect(hdc, sc(r.0), sc(r.1), sc(r.2), sc(r.3), sc(rad) * 2, sc(rad) * 2);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(b.into());
    let _ = DeleteObject(p.into());
}

unsafe fn mkfont(px: i32, weight: i32) -> isize {
    CreateFontW(
        -sc(px),
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        FONT_OUTPUT_PRECISION(0),
        FONT_CLIP_PRECISION(0),
        CLEARTYPE_QUALITY,
        0,
        w!("Segoe UI"),
    )
    .0 as isize
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let screen = BeginPaint(hwnd, &mut ps);
        let (cw, ch) = (sc(W), sc(H));
        let hdc = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, cw, ch);
        let old = SelectObject(hdc, bmp.into());
        SetBkMode(hdc, TRANSPARENT);
        {
            let u = ui();
            draw(hdc, &u);
        }
        let _ = BitBlt(screen, 0, 0, cw, ch, Some(hdc), 0, 0, SRCCOPY);
        SelectObject(hdc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(hdc);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe fn draw(hdc: HDC, u: &Ui) {
    let bg = CreateSolidBrush(COLORREF(BG));
    FillRect(hdc, &rect((0, 0, W, H)), bg);
    let _ = DeleteObject(bg.into());
    // footer strip
    let fb = CreateSolidBrush(COLORREF(PANEL));
    FillRect(hdc, &rect((0, 312, W, H)), fb);
    let _ = DeleteObject(fb.into());

    let [f_title, f_body, f_small, f_btn] = u.fonts;
    if u.icon != 0 {
        let _ = DrawIconEx(hdc, sc(32), sc(26), HICON(u.icon as *mut _), sc(52), sc(52), 0, None, DI_NORMAL);
    }
    let left = DT_LEFT | DT_TOP | DT_WORDBREAK;
    let (title, sub) = match u.page {
        Page::Welcome if u.upgrading => ("Update Archy", "A newer version is ready to install"),
        Page::Welcome => ("Install Archy", "Game clips, saved the moment they happen"),
        Page::Installing if u.upgrading => ("Updating Archy", "Please wait, this takes a moment"),
        Page::Installing => ("Installing Archy", "Please wait, this takes a moment"),
        Page::Done if u.upgrading => ("Archy is updated", "Everything is in place"),
        Page::Done => ("Archy is ready", "Everything is in place"),
        Page::Failed => ("Setup couldn't finish", "Nothing was lost"),
    };
    text(hdc, title, (100, 28, W - 32, 62), f_title, TEXT, DT_LEFT | DT_TOP | DT_SINGLELINE);
    text(hdc, sub, (100, 62, W - 32, 84), f_body, DIM, DT_LEFT | DT_TOP | DT_SINGLELINE);
    rrect(hdc, (32, 100, W - 32, 102), LINE, LINE, 0);

    match u.page {
        Page::Welcome => {
            let body = if u.upgrading {
                "Your clips and settings are kept. Archy closes while it updates, then starts again by itself."
            } else {
                "Archy keeps the last moments of your games and saves them with a hotkey or your voice. Everything stays on this PC. It installs just for you; no admin needed."
            };
            text(hdc, body, (32, 114, W - 32, 168), f_body, TEXT, left);
            text(hdc, "Install location", (32, 170, W - 32, 188), f_small, DIM, DT_LEFT | DT_TOP | DT_SINGLELINE);
            let box_r = (32, 190, if u.upgrading { W - 32 } else { 408 }, 222);
            rrect(hdc, box_r, PANEL, LINE, 6);
            text(
                hdc,
                &u.dir.display().to_string(),
                (44, 190, box_r.2 - 10, 222),
                f_body,
                TEXT,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_PATH_ELLIPSIS,
            );
        }
        Page::Installing => {
            text(hdc, &u.status, (32, 140, W - 32, 170), f_body, TEXT, DT_LEFT | DT_TOP | DT_SINGLELINE);
            rrect(hdc, (32, 180, W - 32, 192), PANEL, LINE, 6);
            let fill = 32 + ((W - 64) as f32 * u.shown.clamp(0.0, 1.0)) as i32;
            if fill > 44 {
                rrect(hdc, (32, 180, fill, 192), ACCENT, ACCENT, 6);
            }
            text(
                hdc,
                "Voice clipping and the clip editor download once (about 120 MB), so the first install can take a few minutes. Keep this window open.",
                (32, 210, W - 32, 270),
                f_small,
                DIM,
                left,
            );
        }
        Page::Done => {
            let body = if u.upgrading {
                "Archy is running in your system tray (bottom right, next to the clock). Your clips and settings are untouched.".to_string()
            } else {
                format!("Archy is running in your system tray (bottom right, next to the clock). In a game, press {} to save the last seconds.", u.hotkey)
            };
            let top = if u.notes.is_empty() { 120 } else { 114 };
            text(hdc, &body, (32, top, W - 32, top + 36), f_body, TEXT, left);
            for (i, n) in u.notes.iter().take(3).enumerate() {
                let y = 150 + i as i32 * 54;
                let r = if n.fix.is_some() { 420 } else { W - 32 };
                text(hdc, &n.text, (32, y, r, y + 52), f_small, ERR, left);
            }
        }
        Page::Failed => {
            text(hdc, &u.err, (32, 114, W - 32, 220), f_body, ERR, left);
            text(
                hdc,
                "Details are in %TEMP%\\archy-setup.log. If antivirus blocked a file, allow it and try again.",
                (32, 240, W - 32, 290),
                f_small,
                DIM,
                left,
            );
        }
    }

    for it in items(u) {
        let hot = u.hover == it.id;
        match it.kind {
            0 => {
                let c = if hot { ACCENT_HI } else { ACCENT };
                rrect(hdc, it.r, c, c, 8);
                text(hdc, &it.label, it.r, f_btn, rgb(0x08, 0x10, 0x20), DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            }
            1 | 3 => {
                let border = if hot { ACCENT } else { LINE };
                rrect(hdc, it.r, PANEL, border, 8);
                text(hdc, &it.label, it.r, f_btn, TEXT, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
            }
            _ => {
                let bx = (it.r.0, it.r.1 + 3, it.r.0 + 18, it.r.1 + 21);
                if it.checked {
                    rrect(hdc, bx, ACCENT, ACCENT, 4);
                    text(hdc, "✓", bx, f_small, rgb(0x08, 0x10, 0x20), DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                } else {
                    rrect(hdc, bx, PANEL, if hot { ACCENT } else { DIM }, 4);
                }
                text(hdc, &it.label, (it.r.0 + 28, it.r.1, it.r.2, it.r.3), f_body, TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
            }
        }
    }
}

fn start_install(hwnd: HWND) {
    let o = {
        let mut u = ui();
        u.page = Page::Installing;
        u.target = 0.02;
        u.shown = 0.0;
        u.status = "Starting…".into();
        u.notes.clear();
        Opts { dir: u.dir.clone(), upgrading: u.upgrading, desktop: u.desktop, startup: u.startup }
    };
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
    std::thread::spawn(move || {
        let step = |end: f32, msg: &str| {
            let mut u = ui();
            u.target = end;
            u.status = msg.to_string();
        };
        let r = perform(&o, &step);
        let mut u = ui();
        match r {
            Ok(notes) => {
                u.notes = notes;
                u.hotkey = archy_core::settings::Settings::load().0.hotkey_save;
                u.shown = 1.0;
                u.page = Page::Done;
            }
            Err(e) => {
                log(&format!("failed: {e}"));
                u.err = e;
                u.page = Page::Failed;
            }
        }
    });
}

fn pick_folder(hwnd: HWND) {
    unsafe {
        let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else {
            return;
        };
        if let Ok(opts) = dlg.GetOptions() {
            let _ = dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
        }
        if dlg.Show(Some(hwnd)).is_err() {
            return;
        }
        let Ok(item) = dlg.GetResult() else { return };
        let Ok(p) = item.GetDisplayName(SIGDN_FILESYSPATH) else { return };
        let chosen = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        if chosen.is_empty() {
            return;
        }
        // Always a folder of our own, so uninstalling never touches other files.
        let mut dir = PathBuf::from(chosen);
        if !dir.file_name().map(|n| n.eq_ignore_ascii_case("Archy")).unwrap_or(false) {
            dir.push("Archy");
        }
        ui().dir = dir;
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn click(hwnd: HWND, id: u32) {
    let (page, notes_fix) = {
        let u = ui();
        let fix = if id >= FIX { u.notes.get((id - FIX) as usize).and_then(|n| n.fix) } else { None };
        (u.page, fix)
    };
    if let Some(f) = notes_fix {
        f();
        return;
    }
    match (id, page) {
        (PRIMARY, Page::Welcome) | (PRIMARY, Page::Failed) => start_install(hwnd),
        (PRIMARY, Page::Done) | (SECOND, Page::Welcome) | (SECOND, Page::Failed) => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        (CHANGE, Page::Welcome) => pick_folder(hwnd),
        (CK_DESKTOP, _) => {
            let mut u = ui();
            u.desktop = !u.desktop;
        }
        (CK_STARTUP, _) => {
            let mut u = ui();
            u.startup = !u.startup;
        }
        _ => {}
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_TIMER => {
                let busy = {
                    let mut u = ui();
                    if u.page == Page::Installing {
                        let d = u.target - u.shown;
                        if d > 0.0 {
                            u.shown += (d * 0.03).max(0.0008).min(d);
                        }
                    }
                    u.page == Page::Installing || u.page == Page::Done || u.page == Page::Failed
                };
                if busy {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
                let changed = {
                    let mut u = ui();
                    let h = hit(&u, x, y);
                    let c = h != u.hover;
                    u.hover = h;
                    c
                };
                if changed {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let over = ui().hover != 0;
                if (lp.0 & 0xffff) == 1 && over {
                    if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(Some(c));
                        return LRESULT(1);
                    }
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_LBUTTONUP => {
                let (x, y) = ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
                let id = hit(&ui(), x, y);
                if id != 0 {
                    click(hwnd, id);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                if ui().page == Page::Installing {
                    return LRESULT(0);
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub fn run(auto: bool) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // One wizard at a time: a second launch just brings the first to the front.
        let _guard = CreateMutexW(None, true, w!("Local\\ArchySetupWizard"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(w) = FindWindowW(w!("ArchySetup"), None) {
                let _ = ShowWindow(w, SW_RESTORE);
                let _ = SetForegroundWindow(w);
            }
            return;
        }
        DPI.store(GetDpiForSystem() as i32, Ordering::Relaxed);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        // The exe's icon is resource 1 (the Clip A icon set in build.rs).
        let icon = |px: i32| {
            LoadImageW(Some(hinst.into()), PCWSTR(1usize as *const u16), IMAGE_ICON, px, px, LR_DEFAULTCOLOR)
                .map(|h| h.0 as isize)
                .unwrap_or(0)
        };
        {
            let mut u = ui();
            match registered_dir() {
                Some(d) => {
                    u.dir = d;
                    u.upgrading = true;
                }
                None => u.dir = default_dir(),
            }
            u.fonts = [mkfont(26, 600), mkfont(15, 400), mkfont(13, 400), mkfont(15, 600)];
            u.icon = icon(sc(52));
        }
        let class = w!("ArchySetup");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hIcon: HICON(icon(sc(32)) as *mut _),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rc = RECT { left: 0, top: 0, right: sc(W), bottom: sc(H) };
        let _ = AdjustWindowRectEx(&mut rc, style, false, WS_EX_APPWINDOW);
        let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - ww) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - wh) / 2;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_APPWINDOW,
            class,
            w!("Archy Setup"),
            style,
            x,
            y,
            ww,
            wh,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            log("couldn't create the setup window");
            return;
        };
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let _ = ShowWindow(hwnd, SW_SHOW);
        // Come to the front even when started by another program (the updater).
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(Some(hwnd), 1, 30, None);
        if auto {
            start_install(hwnd);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
