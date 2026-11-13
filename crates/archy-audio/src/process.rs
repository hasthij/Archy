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
pub fn playing_apps() -> Vec<(u32, String)> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    let mut pids: Vec<u32> = Vec::new();
    let _ = (|| -> windows::core::Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let mgr: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None)?;
            let list = mgr.GetSessionEnumerator()?;
            for i in 0..list.GetCount()? {
                let c = list.GetSession(i)?;
                if c.GetState()? != AudioSessionStateActive {
                    continue;
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
