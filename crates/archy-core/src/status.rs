//! A tiny status file the tray app writes and the Archy window reads,
//! so the window can show what the recorder is doing without any IPC.

use serde::{Deserialize, Serialize};

use crate::settings::app_dir;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Status {
    /// "recording" | "waiting" | "paused" | "error"
    pub state: String,
    /// Game being recorded, e.g. "Roblox".
    pub game: String,
    /// e.g. "H.264 720p60 · Intel Quick Sync via oneVPL (VDEnc)".
    pub detail: String,
    /// Process id of the tray app, so the window can tell if it is running.
    pub pid: u32,
}

pub fn path() -> std::path::PathBuf {
    app_dir().join("status.json")
}

impl Status {
    pub fn write(&self) {
        let _ = std::fs::create_dir_all(app_dir());
        // Write then rename so a reader never sees half a file.
        let tmp = path().with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, path());
        }
    }

    pub fn read() -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path()).ok()?).ok()
    }
}
