//! Audio for clips: WASAPI capture of the whole PC, a single app (game,
//! Discord) or the microphone, encoded live to AAC.
//!
//! Each source runs on its own thread, wakes only when Windows hands it audio,
//! and keeps only compressed AAC (~1.2 MB per minute per track) — never raw PCM.
//! Packet times use the same QPC clock as screen capture, so audio and video
//! line up without any extra syncing.

mod aac;
mod capture;
mod mix;
mod pcm;
mod process;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::thread::JoinHandle;

pub use aac::AacInfo;
pub use pcm::listen_microphone;
pub use process::find_root_process;
use windows::core::{Error, Result};
use windows::Win32::Foundation::E_FAIL;

/// 48 kHz stereo 16-bit everywhere: what the AAC encoder takes, and what
/// Windows converts any device or app to for us.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;

#[derive(Clone, Debug)]
pub enum Source {
    /// Everything an output device plays. `None` = Windows' default output.
    System { device: Option<String> },
    /// One app and the processes it starts (e.g. a game, or Discord).
    Process { pid: u32 },
    /// A microphone. `None` = Windows' default microphone.
    Microphone { device: Option<String> },
    /// An output device's mix plus a microphone, added together (track 1, so
    /// single-track players still hear your voice).
    Mix {
        output: Option<String>,
        mic: Option<String>,
    },
}

pub use capture::{default_device_id, list_devices, AudioDevice};

/// A track volume in percent (0-200), shared with the settings so a change
/// applies to the next audio at once, without restarting the recording.
pub type Volume = Arc<AtomicU32>;

/// Volumes for one track: `main` for the source; `mic` is the microphone's
/// share when the track mixes one in (`Source::Mix`).
#[derive(Clone, Debug)]
pub struct Volumes {
    pub main: Volume,
    pub mic: Volume,
}

impl Default for Volumes {
    fn default() -> Self {
        Self {
            main: Arc::new(AtomicU32::new(100)),
            mic: Arc::new(AtomicU32::new(100)),
        }
    }
}

/// Audio threads must never be starved: Archy's process runs low-priority and
/// throttled (efficiency mode), and on a busy CPU (a 4-core laptop running a
/// game) a capture thread that wakes late lets Windows' buffer overflow, which
/// is heard as choppy, garbled audio. This puts the calling thread in Windows'
/// "Audio" scheduling class (MMCSS) and exempts it from power throttling.
pub(crate) fn audio_thread() {
    use windows::core::w;
    use windows::Win32::System::Threading::{
        AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadInformation,
        ThreadPowerThrottling, THREAD_POWER_THROTTLING_CURRENT_VERSION,
        THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE,
    };
    unsafe {
        let mut index = 0u32;
        let _ = AvSetMmThreadCharacteristicsW(w!("Audio"), &mut index);
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
    }
}

/// Rounds off peaks instead of hard clipping (louder than 100% would crackle).
pub(crate) fn soft_limit(x: f32) -> i16 {
    let s = x / 32768.0;
    let y = if s.abs() <= 0.8 {
        s
    } else {
        s.signum() * (0.8 + 0.2 * ((s.abs() - 0.8) / 0.2).tanh())
    };
    (y * 32767.0) as i16
}

/// `pcm` at `percent` volume, into `out` (reused buffer). None at 100%: use as is.
pub(crate) fn with_volume<'a>(pcm: &'a [i16], percent: u32, out: &'a mut Vec<i16>) -> &'a [i16] {
    if percent == 100 {
        return pcm;
    }
    let gain = percent as f32 / 100.0;
    out.clear();
    out.extend(pcm.iter().map(|&s| soft_limit(s as f32 * gain)));
    out
}

/// One encoded AAC frame (1024 samples, ~21 ms).
pub struct AudioPacket<'a> {
    pub data: &'a [u8],
    /// 100 ns QPC units, same clock as captured video frames.
    pub time: i64,
    pub duration: i64,
}

pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub info: AacInfo,
}

impl AudioCapture {
    /// Starts capturing `source`; `on_packet` receives every AAC frame in order.
    pub fn start(
        source: Source,
        bitrate_kbps: u32,
        volumes: Volumes,
        mut on_packet: impl FnMut(AudioPacket) + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = sync_channel::<Result<AacInfo>>(1);
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("archy-audio".into())
            .spawn(move || {
                let result = match source {
                    Source::Mix { output, mic } => mix::run(
                        output,
                        mic,
                        bitrate_kbps,
                        &volumes,
                        &thread_stop,
                        &ready_tx,
                        &mut on_packet,
                    ),
                    other => capture::run(
                        other,
                        bitrate_kbps,
                        &volumes.main,
                        &thread_stop,
                        &ready_tx,
                        &mut on_packet,
                    ),
                };
                if let Err(e) = result {
                    let _ = ready_tx.try_send(Err(e));
                }
            })
            .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        let info = ready_rx
            .recv()
            .map_err(|_| Error::new(E_FAIL, "audio thread exited during start-up"))??;
        Ok(Self {
            stop,
            thread: Some(thread),
            info,
        })
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
