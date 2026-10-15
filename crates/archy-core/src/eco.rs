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
/// saving a clip, which used to take seconds while throttled, and minutes when
/// the CPU was full (HIGHEST in a below-normal process is a normal priority,
/// level with the game, for the second or so a save takes).
pub fn boost_current_thread() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadInformation, SetThreadPriority, ThreadPowerThrottling,
        THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        THREAD_POWER_THROTTLING_STATE, THREAD_PRIORITY_HIGHEST,
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
        let _ = SetThreadPriority(thread, THREAD_PRIORITY_HIGHEST);
    }
}

/// For the video capture and encoder threads: Windows' "Capture" scheduling
/// class (MMCSS) and no power throttling. The process stays low-priority and
/// throttled; these two threads do little work, but it has to happen on time.
/// At 144 fps a slot is 6.9 ms, and a throttled thread waiting behind the game
/// for 35-55 ms lost 5-8 frames at a time (gaps in the clip, and "the GPU
/// couldn't keep up" step-downs that were really the CPU scheduler). MMCSS
/// limits itself so it can't take the game's CPU; audio threads do the same.
/// Keep the returned guard for the thread's life: dropping it gives the
/// scheduling class back.
#[must_use]
pub fn video_thread() -> Mmcss {
    use windows::core::w;
    use windows::Win32::System::Threading::{
        AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadInformation, ThreadPowerThrottling,
        THREAD_POWER_THROTTLING_CURRENT_VERSION, THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        THREAD_POWER_THROTTLING_STATE,
    };
    unsafe {
        let mut index = 0u32;
        let handle = AvSetMmThreadCharacteristicsW(w!("Capture"), &mut index).ok();
        let state = THREAD_POWER_THROTTLING_STATE {
            Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        let _ = SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        );
        Mmcss(handle)
    }
}

/// A thread's MMCSS registration; reverted when dropped.
pub struct Mmcss(Option<windows::Win32::Foundation::HANDLE>);

impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(h) = self.0 {
            unsafe {
                let _ = windows::Win32::System::Threading::AvRevertMmThreadCharacteristics(h);
            }
        }
    }
}

/// While alive, the whole process runs at full speed (still below the game's
/// priority); then back to efficiency mode. For saving a clip: Media Foundation writes the MP4 on
/// its own worker threads, which inherit the process's throttling, so a 30 s
/// clip took 4-5 s to save during a game (121 ms with the CPU idle). Still on the
/// efficiency cores, so the game's cores stay untouched.
/// Does nothing unless `apply()` put the process in efficiency mode: a user who
/// turned efficiency mode off (or a tool like the self-test) must not be
/// throttled after their first save.
pub struct Unthrottled {
    lifted: bool,
}

/// Whether `apply()` has put this process in efficiency mode.
static APPLIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Saves running right now; efficiency mode comes back when the last one ends.
static SAVING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Unthrottled {
    pub fn new() -> Self {
        use std::sync::atomic::Ordering;
        if !APPLIED.load(Ordering::Acquire) {
            return Self { lifted: false };
        }
        if SAVING.fetch_add(1, Ordering::AcqRel) == 0 {
            set_throttled(false);
        }
        Self { lifted: true }
    }
}

impl Default for Unthrottled {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Unthrottled {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        if self.lifted && SAVING.fetch_sub(1, Ordering::AcqRel) == 1 && APPLIED.load(Ordering::Acquire) {
            set_throttled(true);
        }
    }
}

/// Power throttling (EcoQoS) only: the priority stays below normal, so a save
/// never takes CPU time from the game (lifting both made the game hitch for the
/// ~1.4 s of each save). Unthrottled, Media Foundation's threads run at full clock
/// speed, which was what made saves take 4-5 s in-game.
fn set_throttled(on: bool) {
    let process = unsafe { GetCurrentProcess() };
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
    };
    unsafe {
        let _ = SetProcessInformation(
            process,
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

/// What was applied, for logging.
pub struct Applied {
    pub efficiency_cores: usize,
    pub total_cores: usize,
}

/// Applies to the whole process, including threads created later by libraries.
pub fn apply() -> Applied {
    APPLIED.store(true, std::sync::atomic::Ordering::Release);
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{GetPriorityClass, NORMAL_PRIORITY_CLASS};

    #[test]
    fn a_save_never_throttles_a_process_that_was_not_in_efficiency_mode() {
        // The test process never called apply(): saving must leave it alone.
        let before = unsafe { GetPriorityClass(GetCurrentProcess()) };
        drop(Unthrottled::new());
        let after = unsafe { GetPriorityClass(GetCurrentProcess()) };
        assert_eq!(before, after);
        assert_eq!(after, NORMAL_PRIORITY_CLASS.0);
    }
}
