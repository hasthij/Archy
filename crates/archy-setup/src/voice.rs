//! Downloads the voice clipping engine into `<install>\voice` at install time:
//! sherpa-onnx's keyword spotter (Apache-2.0) and its 3.3M-parameter English
//! keyword model, both from the sherpa-onnx GitHub releases. Each archive is
//! checked against a pinned SHA-256 before anything is unpacked, and only the
//! files Archy uses are kept (~20 MB).

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// No console window flashing up for curl, certutil and tar.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn tool(name: &str) -> Command {
    let mut c = Command::new(name);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

const ENGINE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.8/sherpa-onnx-v1.13.8-win-x64-shared-MD-Release-no-tts-lib.tar.bz2";
const ENGINE_SHA256: &str = "b90992b888710715d613a4fedcd75d5b4db294ef64f3194e4f77ea0a8b387441";
const ENGINE_DIR: &str = "sherpa-onnx-v1.13.8-win-x64-shared-MD-Release-no-tts-lib/lib";
const ENGINE_FILES: &[&str] = &["sherpa-onnx-c-api.dll", "onnxruntime.dll", "onnxruntime_providers_shared.dll"];

const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2";
const MODEL_SHA256: &str = "f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a";
const MODEL_FILES: &[&str] = &[
    "encoder-epoch-12-avg-2-chunk-16-left-64.onnx",
    "decoder-epoch-12-avg-2-chunk-16-left-64.onnx",
    "joiner-epoch-12-avg-2-chunk-16-left-64.int8.onnx",
    "tokens.txt",
    "bpe.model",
];

/// The model folder name Archy looks for (archy_voice::MODEL_DIR).
const MODEL_DIR: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";

/// True when every file Archy needs is already there (an update keeps them).
pub fn installed(dir: &Path) -> bool {
    let voice = dir.join("voice");
    ENGINE_FILES.iter().all(|f| voice.join(f).exists())
        && MODEL_FILES.iter().all(|f| voice.join(MODEL_DIR).join(f).exists())
}

pub fn install(dir: &Path) -> Result<(), String> {
    let voice = dir.join("voice");
    let work = dir.join("voice-download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(voice.join(MODEL_DIR)).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let engine = fetch(ENGINE_URL, ENGINE_SHA256, &work, "engine.tar.bz2")?;
        let model = fetch(MODEL_URL, MODEL_SHA256, &work, "model.tar.bz2")?;
        unpack(&engine, &work)?;
        unpack(&model, &work)?;
        for f in ENGINE_FILES {
            copy(&work.join(ENGINE_DIR).join(f), &voice.join(f))?;
        }
        for f in MODEL_FILES {
            copy(&work.join(MODEL_DIR).join(f), &voice.join(MODEL_DIR).join(f))?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Downloads with Windows' own curl.exe (built in since Windows 10 1803).
fn fetch(url: &str, sha256: &str, work: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    let path = work.join(name);
    let status = tool("curl.exe")
        .args(["-fsSL", "--retry", "2", "--connect-timeout", "20", "-o"])
        .arg(&path)
        .arg(url)
        .status()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    if !status.success() {
        return Err("the download failed (are you online?)".into());
    }
    let actual = hash(&path)?;
    if !actual.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("{name} didn't match its expected checksum, so it wasn't used"));
    }
    Ok(path)
}

/// SHA-256 via Windows' certutil.
fn hash(path: &Path) -> Result<String, String> {
    let out = tool("certutil")
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .output()
        .map_err(|e| format!("couldn't run certutil: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().replace(' ', ""))
        .find(|l| l.len() == 64 && l.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| "couldn't check the download".into())
}

/// Unpacks with Windows' own tar.exe (handles .tar.bz2).
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let status = tool("tar.exe")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .map_err(|e| format!("couldn't run tar: {e}"))?;
    status.success().then_some(()).ok_or_else(|| "couldn't unpack the download".into())
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("{}: {e}", to.display()))
}

#[cfg(test)]
mod tests {
    /// Real download (~24 MB): `cargo test -p archy-setup -- --ignored`.
    #[test]
    #[ignore]
    fn downloads_and_unpacks() {
        let dir = std::env::temp_dir().join("archy-voice-install-test");
        let _ = std::fs::remove_dir_all(&dir);
        super::install(&dir).unwrap();
        assert!(super::installed(&dir));
        assert!(!dir.join("voice-download").exists());
    }
}
