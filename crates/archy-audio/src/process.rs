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
