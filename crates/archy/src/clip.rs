//! Clip file names.

use std::path::{Path, PathBuf};

use windows::Win32::System::SystemInformation::GetLocalTime;

/// "<folder>\Roblox 2026-10-10 14-03-22.mp4" (never overwrites an existing file).
pub fn new_path(folder: &Path, game: &str) -> PathBuf {
    let t = unsafe { GetLocalTime() };
    let safe: String = game
        .chars()
        .filter(|c| !r#"\/:*?"<>|"#.contains(*c))
        .collect();
    let stem = format!(
        "{} {:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        safe.trim(),
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond
    );
    let mut path = folder.join(format!("{stem}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = folder.join(format!("{stem} ({n}).mp4"));
        n += 1;
    }
    path
}
