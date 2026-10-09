//! Per-thread CPU accounting for profiling: which module's threads are busy.

use std::collections::HashMap;
use std::time::Duration;

use windows::core::{s, w};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, HMODULE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW, GetProcAddress,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetThreadTimes, OpenThread, THREAD_QUERY_INFORMATION,
};

type NtQueryInformationThread =
    unsafe extern "system" fn(HANDLE, u32, *mut usize, u32, *mut u32) -> i32;
const THREAD_QUERY_SET_WIN32_START_ADDRESS: u32 = 9;

/// CPU time per thread id, with the module its start routine lives in.
pub fn snapshot() -> HashMap<u32, (Duration, String)> {
    let mut out = HashMap::new();
    let pid = unsafe { GetCurrentProcessId() };
    let query: Option<NtQueryInformationThread> = unsafe {
        GetModuleHandleW(w!("ntdll.dll"))
            .ok()
            .and_then(|m| GetProcAddress(m, s!("NtQueryInformationThread")))
            .map(|f| std::mem::transmute(f))
    };
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }) else {
        return out;
    };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut ok = unsafe { Thread32First(snap, &mut entry) }.is_ok();
    while ok {
        if entry.th32OwnerProcessID == pid {
            if let Ok(h) =
                unsafe { OpenThread(THREAD_QUERY_INFORMATION, false, entry.th32ThreadID) }
            {
                let (mut c, mut e, mut k, mut u) =
                    <(FILETIME, FILETIME, FILETIME, FILETIME)>::default();
                let ticks =
                    |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
                if unsafe { GetThreadTimes(h, &mut c, &mut e, &mut k, &mut u) }.is_ok() {
                    let cpu = Duration::from_nanos((ticks(k) + ticks(u)) * 100);
                    out.insert(entry.th32ThreadID, (cpu, start_module(h, query)));
                }
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
        }
        ok = unsafe { Thread32Next(snap, &mut entry) }.is_ok();
    }
    unsafe {
        let _ = CloseHandle(snap);
    }
    out
}

fn start_module(thread: HANDLE, query: Option<NtQueryInformationThread>) -> String {
    let Some(query) = query else {
        return "?".into();
    };
    let mut addr = 0usize;
    let status = unsafe {
        query(
            thread,
            THREAD_QUERY_SET_WIN32_START_ADDRESS,
            &mut addr,
            std::mem::size_of::<usize>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status != 0 || addr == 0 {
        return "?".into();
    }
    let mut module = HMODULE::default();
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(addr as *const u16),
            &mut module,
        )
    };
    if found.is_err() {
        return "?".into();
    }
    let mut buf = [0u16; 260];
    let n = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    let path = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    path.file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".into())
}

/// Prints CPU used between two snapshots, grouped by start module, busiest first.
pub fn report(
    before: &HashMap<u32, (Duration, String)>,
    after: &HashMap<u32, (Duration, String)>,
    wall: Duration,
) {
    let mut by_module: HashMap<&str, (Duration, usize)> = HashMap::new();
    for (tid, (cpu, module)) in after {
        let prev = before.get(tid).map(|(c, _)| *c).unwrap_or_default();
        let used = cpu.saturating_sub(prev);
        let slot = by_module.entry(module.as_str()).or_default();
        slot.0 += used;
        slot.1 += 1;
    }
    let mut rows: Vec<_> = by_module
        .into_iter()
        .filter(|(_, (d, _))| !d.is_zero())
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1 .0));
    println!("  Threads by module (CPU % of one core):");
    for (module, (cpu, count)) in rows {
        println!(
            "    {:>5.1}%  {module} ({count} thread{})",
            cpu.as_secs_f64() / wall.as_secs_f64() * 100.0,
            if count == 1 { "" } else { "s" }
        );
    }
}
