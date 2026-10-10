//! Keeping the recorder off the game's CPU cores.
//!
//! Many games (Roblox among them) are limited by one main thread on a
//! performance core. On hybrid CPUs (Intel 12th gen+), the recorder and every
//! thread its libraries start (Intel's encoder runtime has a dozen) are pinned
//! to the efficiency cores, run with EcoQoS and below-normal priority, so the
//! game keeps its performance cores to itself.

use windows::Win32::System::SystemInformation::{
    CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, ProcessPowerThrottling, SetPriorityClass, SetProcessDefaultCpuSets,
    SetProcessInformation, BELOW_NORMAL_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
};

/// Lifts efficiency mode for the calling thread only (still on the efficiency
/// cores, so the game's cores stay untouched): normal speed and priority. For
/// saving a clip, which used to take seconds while throttled.
pub fn boost_current_thread() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadInformation, SetThreadPriority, ThreadPowerThrottling,
        THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        THREAD_POWER_THROTTLING_STATE, THREAD_PRIORITY_ABOVE_NORMAL,
    };
    unsafe {
        let thread = GetCurrentThread();
        let state = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        let _ = SetThreadInformation(
            thread,
            ThreadPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        );
        let _ = SetThreadPriority(thread, THREAD_PRIORITY_ABOVE_NORMAL);
    }
}

/// What was applied, for logging.
pub struct Applied {
    pub efficiency_cores: usize,
    pub total_cores: usize,
}

/// Applies to the whole process, including threads created later by libraries.
pub fn apply() -> Applied {
    let process = unsafe { GetCurrentProcess() };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    unsafe {
        let _ = SetProcessInformation(
            process,
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
        let _ = SetPriorityClass(process, BELOW_NORMAL_PRIORITY_CLASS);
    }

    let sets = cpu_sets();
    let total_cores = sets.len();
    let lowest = sets.iter().map(|&(_, class)| class).min();
    let highest = sets.iter().map(|&(_, class)| class).max();
    let mut efficiency_cores = 0;
    if let (Some(lo), Some(hi)) = (lowest, highest) {
        // Only hybrid CPUs have more than one efficiency class; elsewhere leave scheduling alone.
        if lo != hi {
            let ids: Vec<u32> = sets
                .iter()
                .filter(|&&(_, c)| c == lo)
                .map(|&(id, _)| id)
                .collect();
            efficiency_cores = ids.len();
            unsafe {
                let _ = SetProcessDefaultCpuSets(process, Some(&ids));
            }
        }
    }
    Applied {
        efficiency_cores,
        total_cores,
    }
}

/// (CPU set id, efficiency class) for every logical processor.
fn cpu_sets() -> Vec<(u32, u8)> {
    let mut len = 0u32;
    unsafe {
        let _ = GetSystemCpuSetInformation(None, 0, &mut len, None, None);
    }
    if len == 0 {
        return Vec::new();
    }
    // u64 storage keeps the entries suitably aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let ok = unsafe {
        GetSystemCpuSetInformation(
            Some(buf.as_mut_ptr() as *mut SYSTEM_CPU_SET_INFORMATION),
            len,
            &mut len,
            None,
            None,
        )
    };
    if !ok.as_bool() {
        return Vec::new();
    }
    let bytes = buf.as_ptr() as *const u8;
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < len as usize {
        let entry = unsafe { &*(bytes.add(off) as *const SYSTEM_CPU_SET_INFORMATION) };
        if entry.Size == 0 {
            break;
        }
        if entry.Type == CpuSetInformation {
            let cpu = unsafe { entry.Anonymous.CpuSet };
            out.push((cpu.Id, cpu.EfficiencyClass));
        }
        off += entry.Size as usize;
    }
    out
}
