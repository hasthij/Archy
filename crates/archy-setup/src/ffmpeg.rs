//! Downloads the clip editor's video tools into `<install>\ffmpeg` at install
//! time: FFmpeg 9.0.2 (GPL-3.0, Gyan Doshi's Windows build, from its GitHub
//! release). The archive is checked against a pinned SHA-256 before anything is
//! unpacked, and only ffmpeg.exe, ffprobe.exe and their DLLs are kept. Only
//! the Archy window's editor uses them; recording never does.

use std::path::Path;

const URL: &str = "https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-full_build-shared.zip";
const SHA256: &str = "8d31e162f1616e37aab3fa2db991b97e1b8dbeb1c8465fd81a81be16ff91b328";

/// True when the tools are already there (an update keeps them).
pub fn installed(dir: &Path) -> bool {
    let d = dir.join("ffmpeg");
    d.join("ffmpeg.exe").exists() && d.join("ffprobe.exe").exists()
}

pub fn install(dir: &Path) -> Result<(), String> {
    let out = dir.join("ffmpeg");
    let work = dir.join("ffmpeg-download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let archive = crate::voice::fetch(URL, SHA256, &work, "ffmpeg.zip")?;
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        unpack(&archive, &out)
    })();
    let _ = std::fs::remove_dir_all(&work);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&out);
    }
    result
}

/// Pulls `bin\ffmpeg.exe`, `bin\ffprobe.exe` and `bin\*.dll` out of the zip,
/// flat into `out` (ffplay and the docs are left behind).
fn unpack(archive: &Path, out: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("couldn't open the download: {e}"))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("couldn't read the download: {e}"))?;
    let mut found = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("couldn't unpack the download: {e}"))?;
        let name = entry.name().replace('\\', "/");
        let Some((parent, file)) = name.rsplit_once('/') else { continue };
        let wanted = parent.ends_with("/bin")
            && (file == "ffmpeg.exe" || file == "ffprobe.exe" || file.to_ascii_lowercase().ends_with(".dll"));
        if !wanted || file.contains("..") {
            continue;
        }
        let mut dst = std::fs::File::create(out.join(file)).map_err(|e| format!("{file}: {e}"))?;
        std::io::copy(&mut entry, &mut dst).map_err(|e| format!("{file}: {e}"))?;
        found += 1;
    }
    if installed(out.parent().unwrap_or(out)) && found > 2 {
        Ok(())
    } else {
        Err("the download didn't contain FFmpeg".into())
    }
}

#[cfg(test)]
mod tests {
    /// Real download (~100 MB): `cargo test -p archy-setup -- --ignored ffmpeg`.
    #[test]
    #[ignore]
    fn ffmpeg_downloads_and_unpacks() {
        let dir = std::env::temp_dir().join("archy-ffmpeg-install-test");
        let _ = std::fs::remove_dir_all(&dir);
        super::install(&dir).unwrap();
        assert!(super::installed(&dir));
        assert!(!dir.join("ffmpeg-download").exists());
    }
}
