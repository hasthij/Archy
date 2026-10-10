//! Archy's recorder as a library: capture → convert → hardware encode into a
//! RAM replay buffer, and instant clip saving (no re-encode).

pub mod cadence;
pub mod clipboard;
pub mod discord;
pub mod eco;
pub mod permissions;
pub mod presets;
pub mod process;
pub mod recorder;
pub mod ring;
pub mod settings;
pub mod status;

pub use archy_capture::Target;
pub use archy_gpu::convert::ConvertMode;
pub use archy_gpu::probe::Codec;
pub use recorder::{ClipInfo, Recorder, RecorderConfig, RecorderInfo, RecorderStatus};

use std::sync::Once;

use windows::core::Result;
use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// COM (multithreaded) for the calling thread and Media Foundation for the process.
pub fn init_thread() -> Result<()> {
    static MF: Once = Once::new();
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let mut result = Ok(());
    MF.call_once(|| result = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) });
    result
}
