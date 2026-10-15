//! Disk space and the Recycle Bin, for the clips folder.

use std::path::{Path, PathBuf};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::UI::Shell::{
    SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
    SHFILEOPSTRUCTW,
};

/// Bytes free for this user on the drive holding `path` (None if unknown).
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut free = 0u64;
    let dir = HSTRING::from(path.as_os_str());
    unsafe { GetDiskFreeSpaceExW(&dir, Some(&mut free), None, None) }.ok()?;
    Some(free)
}

/// Moves a file to the Recycle Bin (never deletes for good).
pub fn recycle(path: &Path) -> Result<(), String> {
    let from: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    match unsafe { SHFileOperationW(&mut op) } {
        0 => Ok(()),
        r => Err(format!("could not move to Recycle Bin (error {r})")),
    }
}

/// Clips (`*.mp4`, not half-written ones) in `folder`, oldest first, with sizes.
pub fn clips_oldest_first(folder: &Path) -> Vec<(PathBuf, u64)> {
    let mut v: Vec<(PathBuf, u64, std::time::SystemTime)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_string_lossy().to_ascii_lowercase();
            if !name.ends_with(".mp4") || name.contains(".partial.") {
                return None;
            }
            let m = e.metadata().ok()?;
            Some((p, m.len(), m.modified().ok()?))
        })
        .collect();
    v.sort_by_key(|(_, _, t)| *t);
    v.into_iter().map(|(p, n, _)| (p, n)).collect()
}

/// The oldest clips to move out so the folder fits in `limit` bytes (never the
/// newest `keep_newest`, so the clip just saved is always kept).
pub fn over_limit(clips: &[(PathBuf, u64)], limit: u64, keep_newest: usize) -> Vec<PathBuf> {
    let mut total: u64 = clips.iter().map(|(_, n)| n).sum();
    let removable = clips.len().saturating_sub(keep_newest);
    let mut out = Vec::new();
    for (p, n) in clips.iter().take(removable) {
        if total <= limit {
            break;
        }
        total -= n;
        out.push(p.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_oldest_first_and_keeps_the_newest() {
        let clips: Vec<(PathBuf, u64)> = (0..5).map(|i| (PathBuf::from(format!("{i}.mp4")), 100)).collect();
        assert_eq!(over_limit(&clips, 1000, 1), Vec::<PathBuf>::new());
        assert_eq!(over_limit(&clips, 300, 1), vec![PathBuf::from("0.mp4"), PathBuf::from("1.mp4")]);
        // Even a tiny limit keeps the newest clip.
        assert_eq!(over_limit(&clips, 0, 1).len(), 4);
    }
}
