//! Finding an app's root process (e.g. Discord's main process, whose tree
//! includes the child process that plays voice chat).

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

/// The first running process named `exe` whose parent has a different name.
pub fn find_root_process(exe: &str) -> Option<u32> {
    let mut procs: Vec<(u32, u32, String)> = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(e.szExeFile.len());
            procs.push((
                e.th32ProcessID,
                e.th32ParentProcessID,
                String::from_utf16_lossy(&e.szExeFile[..len]),
            ));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let named = |pid: u32| procs.iter().find(|p| p.0 == pid).map(|p| p.2.as_str());
    procs
        .iter()
        .filter(|p| p.2.eq_ignore_ascii_case(exe))
        .find(|p| !named(p.1).is_some_and(|n| n.eq_ignore_ascii_case(exe)))
        .map(|p| p.0)
}

/// Apps playing sound right now on the default output: (root pid, exe name).
/// One cheap COM query (no capture, no polling threads); ~1 ms.
/// `idle`: also list apps that have an audio session but are silent right now
/// (Spotify paused), so they can be ticked before they play.
pub fn playing_apps(idle: bool) -> Vec<(u32, String)> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    let mut pids: Vec<u32> = Vec::new();
    let _ = (|| -> windows::core::Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            // Every active output, not just the default: a WhatsApp call plays on the
            // "communications" device, which can differ.
            let devs = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for d in 0..devs.GetCount()? {
                let Ok(dev) = devs.Item(d) else { continue };
                let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let Ok(list) = mgr.GetSessionEnumerator() else { continue };
                for i in 0..list.GetCount().unwrap_or(0) {
                    let Ok(c) = list.GetSession(i) else { continue };
                    match c.GetState().ok() {
                        Some(s) if s == AudioSessionStateActive || (idle && s == AudioSessionStateInactive) => {}
                        _ => continue,
                    }
                    if let Ok(c2) = c.cast::<IAudioSessionControl2>() {
                        if let Ok(pid) = c2.GetProcessId() {
                            if pid != 0 {
                                pids.push(pid);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    let procs = snapshot();
    let name = |pid: u32| procs.iter().find(|p| p.0 == pid).map(|p| (p.1, p.2.clone()));
    let mut out: Vec<(u32, String)> = Vec::new();
    for pid in pids {
        // Walk up to the root of the same-named chain (Brave's audio child -> brave.exe).
        let Some((mut parent, exe)) = name(pid) else { continue };
        let mut root = pid;
        while let Some((pp, pn)) = name(parent) {
            if !pn.eq_ignore_ascii_case(&exe) {
                break;
            }
            root = parent;
            parent = pp;
        }
        if !out.iter().any(|o| o.1.eq_ignore_ascii_case(&exe)) {
            out.push((root, exe));
        }
    }
    out
}

fn snapshot() -> Vec<(u32, u32, String)> {
    let mut procs = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return procs };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            procs.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    procs
}

/// The app's icon as a PNG (32x32), from the exe of process `pid`. None if it has none.
pub fn app_icon_png(pid: u32) -> Option<Vec<u8>> {
    use windows::core::PWSTR;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        ok.ok()?;
        buf[len as usize] = 0;
        let mut info = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            windows::core::PCWSTR(buf.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if r == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let mut ii = ICONINFO::default();
        let got = GetIconInfo(info.hIcon, &mut ii).is_ok();
        let mut png_bytes = None;
        if got && !ii.hbmColor.is_invalid() {
            let mut bm = BITMAP::default();
            GetObjectW(ii.hbmColor.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _));
            let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
            if w > 0 && h > 0 && w <= 256 && h <= 256 {
                let mut bi = BITMAPINFO::default();
                bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = w as i32;
                bi.bmiHeader.biHeight = -(h as i32);
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                let mut px = vec![0u8; (w * h * 4) as usize];
                let dc = GetDC(None);
                let lines = GetDIBits(dc, ii.hbmColor, 0, h, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
                ReleaseDC(None, dc);
                if lines > 0 {
                    let opaque = px.chunks(4).all(|p| p[3] == 0);
                    for p in px.chunks_mut(4) {
                        p.swap(0, 2); // BGRA -> RGBA
                        if opaque {
                            p[3] = 255;
                        }
                    }
                    png_bytes = encode_rgba(&px, w, h);
                }
            }
        }
        if !ii.hbmColor.is_invalid() { let _ = DeleteObject(ii.hbmColor.into()); }
        if !ii.hbmMask.is_invalid() { let _ = DeleteObject(ii.hbmMask.into()); }
        let _ = DestroyIcon(info.hIcon);
        png_bytes
    }
}

fn encode_rgba(px: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(px).ok()?;
    }
    Some(out)
}

fn icon_file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let safe: String = name.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    dir.join(format!("{safe}.png"))
}

/// Saves the icon of `pid` as `<dir>/<name>.png` (once; later calls reuse it) and returns the PNG.
pub fn cache_icon(dir: &std::path::Path, name: &str, pid: u32) -> Option<Vec<u8>> {
    let file = icon_file(dir, name);
    if let Ok(b) = std::fs::read(&file) {
        return Some(b);
    }
    let png = app_icon_png(pid)?;
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&file, &png);
    Some(png)
}

/// An icon saved earlier by `cache_icon`.
pub fn cached_icon(dir: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    std::fs::read(icon_file(dir, name)).ok()
}
