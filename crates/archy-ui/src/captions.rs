//! Auto-caption models: where they live, whether they're installed, and downloading them
//! on request (never automatically). Everything stays on this PC; the only network use is
//! fetching a model the user chose, with curl.exe and tar.exe that ship with Windows 10/11.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::Mutex;

const NO_WINDOW: u32 = 0x0800_0000;
const GH: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

pub struct Item {
    pub id: &'static str,
    /// Folder under the captions folder.
    pub dir: &'static str,
    /// Release tag + file, after the GitHub releases host.
    pub url: &'static str,
    /// A plain .onnx download (not a .tar.bz2 archive).
    pub single_file: bool,
    /// Rough download size for the page, in MB.
    pub mb: u32,
}

pub const ITEMS: [Item; 7] = [
    Item { id: "tiny", dir: "sherpa-onnx-whisper-tiny.en", url: "asr-models/sherpa-onnx-whisper-tiny.en.tar.bz2", single_file: false, mb: 118 },
    Item { id: "base", dir: "sherpa-onnx-whisper-base.en", url: "asr-models/sherpa-onnx-whisper-base.en.tar.bz2", single_file: false, mb: 200 },
    Item { id: "small", dir: "sherpa-onnx-whisper-small.en", url: "asr-models/sherpa-onnx-whisper-small.en.tar.bz2", single_file: false, mb: 600 },
    Item { id: "parakeet110", dir: "sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8", url: "asr-models/sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8.tar.bz2", single_file: false, mb: 104 },
    Item { id: "parakeet", dir: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8", url: "asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2", single_file: false, mb: 482 },
    Item { id: "speaker_seg", dir: "sherpa-onnx-pyannote-segmentation-3-0", url: "speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2", single_file: false, mb: 7 },
    Item { id: "speaker_emb", dir: "speakers2", url: "speaker-recongition-models/wespeaker_en_voxceleb_resnet34.onnx", single_file: true, mb: 27 },
];

pub fn url_of(item: &Item) -> String {
    format!("{GH}/{}", item.url)
}

pub fn root() -> PathBuf {
    archy_core::settings::app_dir().join("captions")
}

fn item(id: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|i| i.id == id)
}

fn has_onnx(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).map(|d| d.flatten().any(|e| e.path().extension().is_some_and(|x| x == "onnx"))).unwrap_or(false)
}

pub fn installed(id: &str) -> bool {
    let Some(i) = item(id) else { return false };
    let d = root().join(i.dir);
    // A Whisper model is only usable with encoder, decoder and tokens all unpacked (a half-extracted one isn't).
    if matches!(id, "tiny" | "base" | "small" | "parakeet" | "parakeet110") {
        let has = |part: &str| std::fs::read_dir(&d).map(|r| r.flatten().any(|e| e.file_name().to_string_lossy().contains(part))).unwrap_or(false);
        // Whisper and Parakeet: encoder + decoder (+ tokens); the 110M model is one plain model file (+ tokens).
        return has("tokens") && ((has("encoder") && has("decoder")) || has("model"));
    }
    has_onnx(&d)
}

/// The speech engine (sherpa DLL) is installed with Archy; captions can't run without it.
pub fn engine_present() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let mut c = vec![exe.parent().map(|p| p.join("voice"))];
    c.extend(exe.ancestors().skip(1).take(7).map(|a| Some(a.join("tools").join("voice"))));
    c.into_iter().flatten().any(|d| d.join("sherpa-onnx-c-api.dll").exists())
}

pub fn whisper_dir(model: &str) -> Option<PathBuf> {
    let i = item(model).filter(|i| matches!(i.id, "tiny" | "base" | "small" | "parakeet" | "parakeet110"))?;
    installed(model).then(|| root().join(i.dir))
}

/// (segmentation model, embedding model) when both are installed.
pub fn speaker_models() -> Option<(PathBuf, PathBuf)> {
    let seg_dir = root().join(item("speaker_seg")?.dir);
    let seg = ["model.onnx", "model.int8.onnx"].iter().map(|n| seg_dir.join(n)).find(|p| p.is_file())?;
    let emb = root().join("speakers2").join("wespeaker_en_voxceleb_resnet34.onnx");
    emb.is_file().then_some((seg, emb))
}

/// The running download, so Cancel can end it.
static DOWNLOAD: Mutex<Option<std::process::Child>> = Mutex::new(None);

pub fn cancel_download() {
    if let Ok(mut d) = DOWNLOAD.lock() {
        if let Some(c) = d.as_mut() {
            let _ = c.kill();
        }
    }
}

/// Size in bytes the server reports for `url` (follows redirects), or 0.
fn remote_size(url: &str) -> u64 {
    let out = std::process::Command::new("curl.exe").args(["-sIL", url]).creation_flags(NO_WINDOW).output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<u64>().ok()))
        .last()
        .unwrap_or(0)
}

/// Downloads and unpacks one item. `progress` gets 0.0..=1.0.
pub fn download(id: &str, progress: &dyn Fn(f64)) -> Result<(), String> {
    let it = item(id).ok_or("unknown model")?;
    let root = root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let part = root.join("download.part");
    let _ = std::fs::remove_file(&part);
    let url = url_of(it);
    let total = remote_size(&url).max(it.mb as u64 * 1_000_000 / 2) as f64;
    let child = std::process::Command::new("curl.exe")
        .args(["-L", "--fail", "-s", "-o"])
        .arg(&part)
        .arg(&url)
        .creation_flags(NO_WINDOW)
        .spawn()
        .map_err(|e| format!("couldn't start the download: {e}"))?;
    if let Ok(mut d) = DOWNLOAD.lock() {
        *d = Some(child);
    }
    let status = loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let done = DOWNLOAD.lock().ok().and_then(|mut d| d.as_mut().and_then(|c| c.try_wait().ok().flatten()));
        if let Some(s) = done {
            break s;
        }
        progress((std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) as f64 / total).min(0.98));
    };
    if let Ok(mut d) = DOWNLOAD.lock() {
        *d = None;
    }
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err("the download didn't finish (check your connection, or it was cancelled)".into());
    }
    if it.single_file {
        let dir = root.join(it.dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let name = it.url.rsplit('/').next().unwrap_or("model.onnx");
        std::fs::rename(&part, dir.join(name)).map_err(|e| e.to_string())?;
    } else {
        let ok = std::process::Command::new("tar.exe")
            .arg("-xjf")
            .arg(&part)
            .arg("-C")
            .arg(&root)
            .creation_flags(NO_WINDOW)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(&part);
        if !ok {
            return Err("couldn't unpack the model".into());
        }
    }
    progress(1.0);
    Ok(())
}
