//! Clip editor: trim, per-track volume, text and images, and export as MP4 or GIF
//! (saved next to the clip, or put on the clipboard).
//!
//! FFmpeg does the work. The installer downloads it into `<install>\ffmpeg`
//! (`tools\ffmpeg` when running from the project). Only the Archy window uses it;
//! the recorder never does. Text and images are drawn by the window on a canvas and
//! arrive here as transparent PNGs, so what you see is exactly what's exported.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// An export is running (the window refuses to close meanwhile).
pub static BUSY: AtomicBool = AtomicBool::new(false);
/// Set by "Cancel export"; the running export stops and leaves nothing behind.
pub static CANCEL: AtomicBool = AtomicBool::new(false);

/// Clears BUSY however the export ends.
struct BusyGuard;
impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

use serde::{Deserialize, Serialize};


const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MISSING: &str =
    "The editor's video tools aren't installed yet. Update or reinstall Archy to get them.";

/// Folder holding ffmpeg.exe and ffprobe.exe, if they're installed.
pub fn tools_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut candidates = vec![exe.parent()?.join("ffmpeg")];
    // Running from the project (target\debug\archy-ui.exe): tools\ffmpeg.
    candidates.extend(exe.ancestors().skip(1).map(|a| a.join("tools").join("ffmpeg")));
    candidates
        .into_iter()
        .find(|d| d.join("ffmpeg.exe").is_file() && d.join("ffprobe.exe").is_file())
}

fn tool(name: &str) -> Result<Command, String> {
    let dir = tools_dir().ok_or(MISSING)?;
    let mut c = Command::new(dir.join(name));
    c.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
    Ok(c)
}

/// Scratch space for track previews and clipboard exports.
fn temp_dir(sub: &str) -> PathBuf {
    std::env::temp_dir().join("Archy").join(sub)
}

#[derive(Serialize)]
pub struct ClipInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// One name per audio track, in file order.
    pub tracks: Vec<String>,
}

pub fn info(path: &Path) -> Result<ClipInfo, String> {
    let out = tool("ffprobe.exe")?
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height,avg_frame_rate:format=duration", "-of", "json"])
        .arg(path)
        .output()
        .map_err(|e| format!("couldn't run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!("couldn't read the clip: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s["codec_type"] == "video").ok_or("the clip has no video")?;
    let audio = streams.iter().filter(|s| s["codec_type"] == "audio").count();
    let fps = video["avg_frame_rate"]
        .as_str()
        .and_then(|r| r.split_once('/'))
        .and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok().filter(|d| *d > 0.0)?))
        .unwrap_or(60.0);
    Ok(ClipInfo {
        duration: v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0),
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps,
        tracks: track_names(path, audio),
    })
}

/// Track names the recorder stored at the end of the file (see
/// `archy_core::recorder::append_track_names`); "Track N" for older clips.
pub fn track_names(path: &Path, count: usize) -> Vec<String> {
    let fallback = || (1..=count).map(|i| format!("Track {i}")).collect();
    let Ok(mut f) = std::fs::File::open(path) else { return fallback() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut tail = Vec::new();
    if f.seek(SeekFrom::Start(len.saturating_sub(1024))).is_err() || f.read_to_end(&mut tail).is_err() {
        return fallback();
    }
    let marker = archy_core::recorder::TRACK_NAMES_MARKER;
    let Some(at) = tail.windows(marker.len()).rposition(|w| w == marker) else { return fallback() };
    let names: Vec<String> = String::from_utf8_lossy(&tail[at + marker.len()..])
        .split('|')
        .map(|s| s.trim_matches(char::from(0)).to_string())
        .collect();
    if names.len() == count { names } else { fallback() }
}

/// Each audio track as its own file, so the editor can play them together
/// with live volumes (the video element only plays track 1). Fast: no re-encode.
pub fn preview_tracks(path: &Path, count: usize) -> Result<Vec<String>, String> {
    // A fresh folder each time: the window may still hold the last ones open
    // for a moment, and a locked file can't be overwritten. Old ones are cleared.
    let root = preview_dir();
    if let Ok(old) = std::fs::read_dir(&root) {
        for e in old.flatten() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let dir = root.join(stamp.to_string());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut cmd = tool("ffmpeg.exe")?;
    cmd.args(["-y", "-v", "error", "-i"]).arg(path);
    let mut outs = Vec::new();
    for i in 0..count {
        let out = dir.join(format!("track{i}.m4a"));
        cmd.args(["-map", &format!("0:a:{i}"), "-c", "copy", "-vn"]).arg(&out);
        outs.push(out.to_string_lossy().into_owned());
    }
    let r = cmd.output().map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
    if !r.status.success() {
        return Err(format!("couldn't prepare the audio tracks: {}", String::from_utf8_lossy(&r.stderr).trim()));
    }
    Ok(outs)
}

fn preview_dir() -> PathBuf {
    temp_dir("preview")
}

/// Longest video or GIF that can be put on a clip.
pub const MAX_MEDIA_SECONDS: f64 = 20.0;
/// Longest sound (music, an effect) that can be put on a clip (clips are at most 60 s).
pub const MAX_AUDIO_SECONDS: f64 = 60.0;

/// Where imported media (and its preview copies) is kept.
pub fn media_dir() -> PathBuf {
    temp_dir("media")
}

/// A file just added to the editor, before it's trimmed (if it needs to be).
#[derive(Serialize, Debug)]
pub struct StagedMedia {
    pub path: String,
    /// "video", "gif" or "audio".
    pub kind: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    /// The most that can be used (20 s for video/GIF, 60 s for sound).
    pub limit: f64,
}

#[derive(Serialize, Debug)]
pub struct MediaInfo {
    /// The (trimmed) file the export uses.
    pub path: String,
    /// What the window plays: a small WebM for video/GIF (keeps GIF transparency),
    /// the file itself for sound.
    pub proxy: String,
    pub kind: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

/// Only files Archy put in the media folder are ever used.
fn in_media_dir(path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    let root = media_dir().canonicalize().map_err(|e| e.to_string())?;
    match p.canonicalize() {
        Ok(c) if c.starts_with(&root) => Ok(p),
        _ => Err("that file is gone; add it again".into()),
    }
}

/// Stores a video, GIF or sound the user added and says what it is. Nothing is
/// refused for length here: long files are trimmed with `finish_media`.
pub fn stage_media(name: &str, bytes: &[u8]) -> Result<StagedMedia, String> {
    let dir = media_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ext: String = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".into());
    let path = dir.join(format!("{}.{ext}", stamp()));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    match probe_staged(&path) {
        Ok(m) if m.duration > 0.0 => Ok(m),
        _ => {
            let _ = std::fs::remove_file(&path);
            Err(format!("{name} isn't a video, GIF or sound file Archy can read"))
        }
    }
}

/// A copy of a staged file the window can surely play, for the trim popup
/// (when the original's format isn't one the window plays itself).
pub fn trim_preview(path: &str) -> Result<String, String> {
    let src = in_media_dir(path)?;
    let audio_only = probe_staged(&src)?.kind == "audio";
    let out = src.with_extension(if audio_only { "trimview.m4a" } else { "trimview.webm" });
    let mut cmd = tool("ffmpeg.exe")?;
    cmd.args(["-y", "-v", "error", "-i"]).arg(&src);
    if audio_only {
        cmd.args(["-vn", "-c:a", "aac", "-b:a", "128k"]);
    } else {
        cmd.args(["-vf", "scale='min(480,iw)':-2", "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "600k", "-c:a", "libopus", "-b:a", "96k"]);
    }
    let r = cmd.arg(&out).output().map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
    if !r.status.success() {
        return Err("couldn't open it for trimming".into());
    }
    Ok(out.to_string_lossy().into_owned())
}

/// Cuts `start..end` (seconds) out of a staged file (no cut when it's already
/// short enough and the whole thing is picked), then makes the preview copy.
pub fn finish_media(path: &str, start: f64, end: f64) -> Result<MediaInfo, String> {
    let src = in_media_dir(path)?;
    let staged = probe_staged(&src)?;
    let limit = staged.limit + 0.5;
    let (start, end) = (start.max(0.0), end.min(staged.duration));
    if end - start <= 0.05 {
        return Err("pick a longer part".into());
    }
    if end - start > limit {
        return Err(format!("pick at most {:.0} seconds", staged.limit));
    }
    let whole = start < 0.05 && end > staged.duration - 0.05;
    let kind = staged.kind.as_str();
    let file = if whole {
        src.clone()
    } else {
        // Re-encoded so the cut is exact (a copy can only cut on keyframes).
        let out = src.with_file_name(format!("{}.{}", stamp(), match kind { "audio" => "m4a", "gif" => "gif", _ => "mp4" }));
        let mut cmd = tool("ffmpeg.exe")?;
        cmd.args(["-y", "-v", "error", "-ss", &format!("{start:.3}"), "-t", &format!("{:.3}", end - start), "-i"]).arg(&src);
        match kind {
            "audio" => cmd.args(["-vn", "-c:a", "aac", "-b:a", "192k"]),
            "gif" => cmd.args(["-an"]),
            _ => cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "160k"]),
        };
        let r = cmd.arg(&out).output().map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
        if !r.status.success() {
            return Err(format!("couldn't cut it: {}", String::from_utf8_lossy(&r.stderr).lines().last().unwrap_or("")));
        }
        out
    };
    let duration = if whole { staged.duration } else { end - start };
    let proxy = if kind == "audio" {
        // The window plays common sound files itself; anything else gets an M4A copy.
        let common = file.extension().is_some_and(|e| ["mp3", "wav", "m4a", "aac", "ogg", "opus"].contains(&e.to_string_lossy().to_ascii_lowercase().as_str()));
        if common {
            file.clone()
        } else {
            let p = file.with_extension("preview.m4a");
            let r = tool("ffmpeg.exe")?.args(["-y", "-v", "error", "-i"]).arg(&file).args(["-vn", "-c:a", "aac", "-b:a", "160k"]).arg(&p).output().map_err(|e| e.to_string())?;
            if !r.status.success() {
                return Err("couldn't prepare the sound".into());
            }
            p
        }
    } else {
        let p = file.with_extension("preview.webm");
        let mut cmd = tool("ffmpeg.exe")?;
        cmd.args(["-y", "-v", "error", "-i"]).arg(&file);
        cmd.args(["-vf", "scale='min(640,iw)':-2,format=yuva420p", "-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "1M"]);
        if staged.has_audio && kind != "gif" {
            cmd.args(["-c:a", "libopus", "-b:a", "96k"]);
        } else {
            cmd.arg("-an");
        }
        let r = cmd.arg(&p).output().map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
        if !r.status.success() {
            return Err(format!("couldn't prepare it: {}", String::from_utf8_lossy(&r.stderr).lines().last().unwrap_or("")));
        }
        p
    };
    Ok(MediaInfo {
        path: file.to_string_lossy().into_owned(),
        proxy: proxy.to_string_lossy().into_owned(),
        kind: kind.into(),
        duration,
        width: staged.width,
        height: staged.height,
        has_audio: staged.has_audio && kind != "gif",
    })
}

/// What a file in the media folder is: video, GIF or sound, how long, how big.
fn probe_staged(path: &Path) -> Result<StagedMedia, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let out = tool("ffprobe.exe")?
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height:stream_disposition=attached_pic:format=duration,format_name", "-of", "json"])
        .arg(path)
        .output()
        .map_err(|e| format!("couldn't run ffprobe: {e}"))?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let video = streams.iter().find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1);
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    let duration: f64 = v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    let kind = match video {
        Some(_) if v["format"]["format_name"].as_str().unwrap_or("").contains("gif") => "gif",
        Some(_) => "video",
        None if has_audio => "audio",
        None => return Err(format!("{name} can't be read")),
    };
    Ok(StagedMedia {
        path: path.to_string_lossy().into_owned(),
        kind: kind.into(),
        duration,
        width: video.and_then(|s| s["width"].as_u64()).unwrap_or(0) as u32,
        height: video.and_then(|s| s["height"].as_u64()).unwrap_or(0) as u32,
        has_audio,
        limit: if kind == "audio" { MAX_AUDIO_SECONDS } else { MAX_MEDIA_SECONDS },
    })
}

/// A video, GIF or sound put on the clip.
#[derive(Deserialize)]
pub struct MediaOverlay {
    /// "video", "gif" or "audio" (sound only: nothing is drawn).
    #[serde(default)]
    pub kind: String,
    /// From `import_media` (only files in the media folder are used).
    pub path: String,
    /// Seconds into the original clip; a video loops if it's shorter than this span.
    pub start: f64,
    pub end: f64,
    /// Centre, as a fraction of the clip's width/height.
    pub x: f64,
    pub y: f64,
    /// Width as a fraction of the clip's width.
    pub w: f64,
    /// Width / height of the video.
    pub aspect: f64,
    /// Its sound, 0-200 % (0 = silent; already includes its track's volume).
    pub volume: u32,
    /// Where in the file it starts (trimmed off the front on the timeline), seconds.
    #[serde(default)]
    pub offset: f64,
    pub has_audio: bool,
}

#[derive(Deserialize)]
pub struct Overlay {
    /// Transparent PNG at the clip's size, base64 (no `data:` prefix).
    pub png: String,
    /// Seconds into the original clip.
    pub start: f64,
    pub end: f64,
}

#[derive(Deserialize)]
pub struct ExportRequest {
    pub path: String,
    /// Seconds into the clip.
    pub start: f64,
    pub end: f64,
    /// Percent (0-200) per audio track, in file order. The export has one mixed track.
    pub volumes: Vec<u32>,
    /// Text and images, bottom layer first (drawn over `media`).
    pub overlays: Vec<Overlay>,
    /// Videos and GIFs, bottom layer first (under the text and images).
    #[serde(default)]
    pub media: Vec<MediaOverlay>,
    /// "mp4" or "gif".
    pub format: String,
    pub gif_width: u32,
    pub gif_fps: u32,
    /// Put the result on the clipboard instead of saving it.
    pub to_clipboard: bool,
    /// Where to save it (chosen in the "Save as" dialog). `None`: next to the clip.
    #[serde(default)]
    pub save_to: Option<String>,
}

/// First file name that's free: "<stem> (edit).mp4", "<stem> (edit 2).mp4", ...
fn free_name(dir: &Path, stem: &str, label: &str, ext: &str) -> PathBuf {
    (1..)
        .map(|n| {
            let tag = if n == 1 { label.to_string() } else { format!("{label} {n}") };
            dir.join(format!("{stem} ({tag}).{ext}"))
        })
        .find(|p| !p.exists())
        .expect("endless names")
}

/// Video encoder that worked last time (hardware first, x264 always works).
static ENCODER: Mutex<Option<&'static str>> = Mutex::new(None);
const ENCODERS: &[&str] = &["h264_nvenc", "h264_amf", "h264_qsv", "libx264"];

fn encoder_args(name: &str) -> &'static [&'static str] {
    match name {
        "h264_nvenc" => &["-preset", "p5", "-rc", "vbr", "-cq", "21", "-b:v", "0"],
        "h264_amf" => &["-quality", "quality", "-rc", "cqp", "-qp_i", "20", "-qp_p", "22"],
        "h264_qsv" => &["-preset", "medium", "-global_quality", "21"],
        _ => &["-preset", "veryfast", "-crf", "20"],
    }
}

/// The clip's video size (overlays are placed in its pixels).
fn clip_size(src: &Path) -> Result<(u32, u32), String> {
    let i = info(src)?;
    Ok((i.width.max(2), i.height.max(2)))
}

/// Exports the edit; returns the new file's path. `progress` gets 0..1 as it goes
/// (the window shows it as `export-progress` events).
pub fn export(src: &Path, req: &ExportRequest, progress: &dyn Fn(f64)) -> Result<PathBuf, String> {
    let gif = req.format == "gif";
    let ext = if gif { "gif" } else { "mp4" };
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let length = (req.end - req.start).max(0.1);
    let work = temp_dir("export-work");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    let out = if req.to_clipboard {
        // Clipboard exports live in temp; old ones are cleared out here.
        let dir = temp_dir("exports");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        dir.join(format!("{stem}.{ext}"))
    } else if let Some(to) = &req.save_to {
        PathBuf::from(to).with_extension(ext)
    } else {
        free_name(src.parent().unwrap_or(Path::new(".")), &stem, if gif { "GIF" } else { "edit" }, ext)
    };

    // ---- the filter graph ----
    // Inputs after the clip itself: each with the options that go before its -i.
    let mut inputs: Vec<(Vec<String>, PathBuf)> = Vec::new();
    let mut graph = vec!["[0:v]setpts=PTS-STARTPTS[v0]".to_string()];
    let mut last = "v0".to_string();
    let mut layer = 0;
    let mut media_audio: Vec<String> = Vec::new();
    let (cw, ch) = clip_size(src)?;
    let media_root = media_dir().canonicalize().ok();
    // Videos and GIFs first (bottom layers), each looped to fill its span.
    for m in &req.media {
        let path = PathBuf::from(&m.path);
        let inside = path.canonicalize().ok().zip(media_root.clone()).is_some_and(|(p, r)| p.starts_with(r));
        if !inside {
            return Err("a video or sound on the clip couldn't be found; add it again".into());
        }
        let sound_only = m.kind == "audio";
        if sound_only && (gif || m.volume == 0) {
            continue; // nothing to see, nothing to hear
        }
        let (a, b) = (m.start - req.start, m.end - req.start);
        if b <= 0.0 || a >= length {
            continue;
        }
        let span = m.end - m.start;
        let mut opts: Vec<String> = Vec::new();
        if !sound_only {
            opts.extend(["-stream_loop".into(), "-1".into()]);
        }
        if m.offset > 0.0 {
            opts.extend(["-ss".into(), format!("{:.3}", m.offset)]);
        }
        opts.extend(["-t".into(), format!("{span:.3}")]);
        inputs.push((opts, path));
        let k = inputs.len();
        let w = ((m.w * cw as f64).round() as u32).max(2) & !1;
        let h = ((w as f64 / m.aspect.max(0.01)).round() as u32).max(2) & !1;
        let x = (m.x * cw as f64 - w as f64 / 2.0).round() as i64;
        let y = (m.y * ch as f64 - h as f64 / 2.0).round() as i64;
        if !sound_only {
            layer += 1;
            let next = format!("v{layer}");
            graph.push(format!("[{k}:v]scale={w}:{h},format=rgba,setpts=PTS-STARTPTS+{a:.3}/TB[m{k}]"));
            graph.push(format!(
                "[{last}][m{k}]overlay=x={x}:y={y}:eof_action=pass:enable='between(t,{:.3},{b:.3})'[{next}]",
                a.max(0.0)
            ));
            last = next;
        }
        if m.has_audio && m.volume > 0 && !gif {
            let shift = if a >= 0.0 {
                format!("adelay={}:all=1", (a * 1000.0).round() as i64)
            } else {
                format!("atrim=start={:.3},asetpts=PTS-STARTPTS", -a)
            };
            graph.push(format!("[{k}:a]aresample=48000,volume={:.2},{shift}[ma{k}]", m.volume.min(200) as f32 / 100.0));
            media_audio.push(format!("[ma{k}]"));
        }
    }
    // Then text and images (drawn by the window as clip-sized PNGs).
    for (i, c) in req.overlays.iter().enumerate() {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD
            .decode(c.png.as_bytes())
            .map_err(|_| "a caption image was damaged")?;
        let file = work.join(format!("caption{i}.png"));
        std::fs::write(&file, png).map_err(|e| e.to_string())?;
        inputs.push((Vec::new(), file));
        let k = inputs.len();
        let (a, b) = ((c.start - req.start).max(0.0), (c.end - req.start).max(0.0));
        layer += 1;
        let next = format!("v{layer}");
        graph.push(format!("[{last}][{k}:v]overlay=0:0:enable='between(t,{a:.3},{b:.3})'[{next}]"));
        last = next;
    }
    if gif {
        let fps = req.gif_fps.clamp(5, 30);
        let width = req.gif_width.clamp(160, 1280);
        graph.push(format!(
            "[{last}]fps={fps},scale='min({width},iw)':-2:flags=lanczos,split[g1][g2];\
             [g1]palettegen=stats_mode=diff[pal];[g2][pal]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle[vout]"
        ));
    } else {
        graph.push(format!("[{last}]format=yuv420p[vout]"));
    }
    // One mixed audio track (what every player plays). Track 1 already holds
    // everything you heard, so the others are added on top only when turned up.
    let active: Vec<(usize, u32)> = req
        .volumes
        .iter()
        .enumerate()
        .filter(|(_, v)| **v > 0)
        .map(|(i, v)| (i, (*v).min(200)))
        .collect();
    let has_audio = !gif && (!active.is_empty() || !media_audio.is_empty());
    if has_audio {
        let mut labels = String::new();
        for (i, v) in &active {
            graph.push(format!("[0:a:{i}]volume={:.2}[a{i}]", *v as f32 / 100.0));
            labels.push_str(&format!("[a{i}]"));
        }
        let inputs_n = active.len() + media_audio.len();
        labels.push_str(&media_audio.concat());
        let loud = inputs_n > 1 || active.iter().any(|(_, v)| *v > 100);
        let mix = if inputs_n > 1 {
            format!("{labels}amix=inputs={inputs_n}:normalize=0:duration=longest")
        } else {
            format!("{labels}anull")
        };
        // Round off peaks instead of clipping when tracks add up past full scale.
        let limit = if loud { ",alimiter=limit=0.97:level=false" } else { "" };
        graph.push(format!("{mix}{limit}[aout]"));
    }

    let encoders: Vec<&'static str> = if gif {
        vec!["gif"]
    } else {
        // The one that worked last time first, then the rest in order.
        let known = *ENCODER.lock().unwrap_or_else(|e| e.into_inner());
        known.into_iter().chain(ENCODERS.iter().copied().filter(|e| Some(*e) != known)).collect()
    };
    // Written under a temporary name and renamed when complete, so a cancelled
    // or failed export never leaves a broken file (or ruins one being replaced).
    let partial = out.with_extension(format!("partial.{ext}"));
    BUSY.store(true, Ordering::Release);
    CANCEL.store(false, Ordering::Release);
    let _busy = BusyGuard;
    let mut last_error = String::new();
    for enc in encoders {
        let mut cmd = tool("ffmpeg.exe")?;
        cmd.args(["-y", "-hide_banner", "-v", "error", "-nostats", "-progress", "pipe:1"]);
        cmd.args(["-ss", &format!("{:.3}", req.start), "-t", &format!("{length:.3}"), "-i"]).arg(src);
        for (opts, path) in &inputs {
            cmd.args(opts).arg("-i").arg(path);
        }
        cmd.args(["-filter_complex", &graph.join(";"), "-map", "[vout]"]);
        // Never longer than the trimmed clip (a looping video or delayed sound can't stretch it).
        cmd.args(["-t", &format!("{length:.3}")]);
        if gif {
            cmd.args(["-loop", "0"]);
        } else {
            cmd.args(["-c:v", enc]).args(encoder_args(enc));
            if has_audio {
                cmd.args(["-map", "[aout]", "-c:a", "aac", "-b:a", "160k"]);
            }
            cmd.args(["-movflags", "+faststart"]);
        }
        cmd.arg(&partial).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("couldn't run ffmpeg: {e}"))?;
        let stderr = child.stderr.take();
        let errors = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut e) = stderr {
                let _ = e.read_to_string(&mut s);
            }
            s
        });
        let mut cancelled = false;
        if let Some(stdout) = child.stdout.take() {
            // FFmpeg reports progress about twice a second; a cancel is checked each time.
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if CANCEL.swap(false, Ordering::AcqRel) {
                    cancelled = true;
                    let _ = child.kill();
                }
                if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()) {
                    progress((us / 1e6 / length).clamp(0.0, 1.0));
                }
            }
        }
        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        let err = errors.join().unwrap_or_default();
        if cancelled {
            let _ = std::fs::remove_file(&partial);
            let _ = std::fs::remove_dir_all(&work);
            return Err("cancelled".into());
        }
        if ok && std::fs::rename(&partial, &out).is_ok() {
            if !gif {
                *ENCODER.lock().unwrap_or_else(|e| e.into_inner()) = Some(enc);
            }
            let _ = std::fs::remove_dir_all(&work);
            return Ok(out);
        }
        let _ = std::fs::remove_file(&partial);
        last_error = err.lines().last().unwrap_or("unknown error").to_string();
    }
    let _ = std::fs::remove_dir_all(&work);
    Err(format!("export failed: {last_error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_names_round_trip() {
        let file = std::env::temp_dir().join("archy-track-names-test.mp4");
        std::fs::write(&file, vec![0u8; 4096]).unwrap();
        assert_eq!(track_names(&file, 2), ["Track 1", "Track 2"]);
        archy_core::recorder::append_track_names(&file, &["Everything", "Game", "Microphone"]);
        assert_eq!(track_names(&file, 3), ["Everything", "Game", "Microphone"]);
        // A count that doesn't match (e.g. a re-muxed file) falls back.
        assert_eq!(track_names(&file, 2), ["Track 1", "Track 2"]);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn free_names_count_up() {
        let dir = std::env::temp_dir().join("archy-free-name-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let first = free_name(&dir, "Roblox 2026-10-13", "edit", "mp4");
        assert!(first.ends_with("Roblox 2026-10-13 (edit).mp4"));
        std::fs::write(&first, b"").unwrap();
        assert!(free_name(&dir, "Roblox 2026-10-13", "edit", "mp4").ends_with("Roblox 2026-10-13 (edit 2).mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Real exports with the dev FFmpeg (`tools\ffmpeg`) on a real clip:
/// `cargo test -p archy-ui -- --ignored exports` (ARCHY_TEST_CLIP picks the clip;
/// default: the newest one in the clips folder).
#[cfg(test)]
mod export_tests {
    use super::*;
    use base64::Engine as _;

    fn clip() -> PathBuf {
        if let Some(p) = std::env::var_os("ARCHY_TEST_CLIP") {
            return p.into();
        }
        let folder = archy_core::settings::Settings::load().0.save_folder;
        let mut clips: Vec<_> = std::fs::read_dir(folder)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "mp4") && !p.to_string_lossy().contains('('))
            .collect();
        clips.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        clips.pop().expect("no clip to test with")
    }

    #[test]
    #[ignore]
    fn exports() {
        let work = std::env::temp_dir().join("archy-export-test");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let src = work.join("clip.mp4");
        std::fs::copy(clip(), &src).unwrap();
        // What a new recording looks like: track names noted at the end. Every
        // reader (FFmpeg, Media Foundation via Discord copy) must still take it.
        let count = info(&src).unwrap().tracks.len();
        let names = ["Everything", "Game", "Discord", "Microphone"];
        archy_core::recorder::append_track_names(&src, &names[..count.min(4)]);
        let clip_info = info(&src).unwrap();
        if count <= 4 {
            assert_eq!(clip_info.tracks, names[..count]);
        }
        let discord = work.join("discord.mp4");
        let copy = archy_core::discord::make_copy(&src, &discord, 10_000_000).unwrap();
        println!("discord copy of a named clip: {} KB", copy.bytes / 1024);
        println!("clip: {:.1} s {}x{} @{:.0} tracks {:?}", clip_info.duration, clip_info.width, clip_info.height, clip_info.fps, clip_info.tracks);
        assert!(clip_info.duration > 6.0);

        let previews = preview_tracks(&src, clip_info.tracks.len()).unwrap();
        assert!(previews.iter().all(|p| std::fs::metadata(p).unwrap().len() > 0));

        // A half-transparent bar as the caption image, at the clip's size.
        let png = work.join("cap.png");
        let ok = tool("ffmpeg.exe").unwrap()
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", &format!("color=red@0.5:s={}x{},format=rgba", clip_info.width, clip_info.height), "-frames:v", "1"])
            .arg(&png)
            .status()
            .unwrap();
        assert!(ok.success());
        let png = base64::engine::general_purpose::STANDARD.encode(std::fs::read(&png).unwrap());

        let mut req = ExportRequest {
            path: String::new(),
            start: 2.0,
            end: 7.0,
            volumes: clip_info.tracks.iter().enumerate().map(|(i, _)| if i == 0 { 150 } else { 50 }).collect(),
            overlays: vec![Overlay { png, start: 3.0, end: 5.0 }],
            format: "mp4".into(),
            gif_width: 480,
            gif_fps: 15,
            to_clipboard: false,
            save_to: None,
            media: Vec::new(),
        };
        let last = std::cell::Cell::new(0.0);
        let mp4 = export(&src, &req, &|p| last.set(p)).unwrap();
        let out = info(&mp4).unwrap();
        println!("mp4: {} {:.2} s, {} track(s), progress reached {:.2}, encoder {:?}", mp4.display(), out.duration, out.tracks.len(), last.get(), ENCODER.lock().unwrap());
        assert!((out.duration - 5.0).abs() < 0.3, "{}", out.duration);
        assert_eq!(out.tracks.len(), usize::from(!clip_info.tracks.is_empty()));
        assert!(last.get() > 0.5);

        // A 3 s video with sound and a 2 s GIF, imported the way the editor does,
        // laid over the clip (the video loops to fill 4 s).
        let make = |args: &[&str], out: &Path| {
            let ok = tool("ffmpeg.exe").unwrap().args(["-y", "-v", "error"]).args(args).arg(out).status().unwrap();
            assert!(ok.success());
        };
        let vid = work.join("meme.mp4");
        make(&["-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=30", "-f", "lavfi", "-i", "sine=frequency=440:duration=3", "-shortest"], &vid);
        let gif_in = work.join("meme.gif");
        make(&["-f", "lavfi", "-i", "testsrc=duration=2:size=160x120:rate=10"], &gif_in);
        let too_long = work.join("long.mp4");
        make(&["-f", "lavfi", "-i", "testsrc=duration=25:size=64x64:rate=5"], &too_long);
        let add = |name: &str, file: &Path| {
            let staged = stage_media(name, &std::fs::read(file).unwrap()).unwrap();
            finish_media(&staged.path, 0.0, staged.duration).unwrap()
        };
        // Too long: staged fine, refused whole, accepted once trimmed (exact cut).
        let long = stage_media("long.mp4", &std::fs::read(&too_long).unwrap()).unwrap();
        assert!(long.kind == "video" && long.duration > 24.0 && long.limit == MAX_MEDIA_SECONDS);
        assert!(finish_media(&long.path, 0.0, long.duration).unwrap_err().contains("at most 20"));
        let cut = finish_media(&long.path, 3.0, 13.0).unwrap();
        assert!((info(Path::new(&cut.path)).unwrap().duration - 10.0).abs() < 0.3);
        // A song: an MP3 with cover art is still a sound, trimmed to 60 s at most.
        let song = work.join("song.mp3");
        let raw = work.join("raw.mp3");
        let cover = work.join("cover.jpg");
        make(&["-f", "lavfi", "-i", "sine=frequency=330:duration=75"], &raw);
        make(&["-f", "lavfi", "-i", "color=red:size=64x64", "-frames:v", "1"], &cover);
        make(&["-i", raw.to_str().unwrap(), "-i", cover.to_str().unwrap(), "-map", "0:a", "-map", "1:v", "-c:a", "copy", "-c:v", "mjpeg", "-disposition:v", "attached_pic"], &song);
        let staged_song = stage_media("song.mp3", &std::fs::read(&song).unwrap()).unwrap();
        assert_eq!(staged_song.kind, "audio", "cover art must not make it a video");
        let whole = finish_media(&staged_song.path, 0.0, 75.0);
        assert!(whole.as_ref().err().is_some_and(|e| e.contains("at most 60")), "duration {} -> {:?}", staged_song.duration, whole);
        let s = finish_media(&staged_song.path, 10.0, 40.0).unwrap();
        assert!(s.kind == "audio" && Path::new(&s.proxy).exists());
        let wav = work.join("beep.wav");
        make(&["-f", "lavfi", "-i", "sine=frequency=880:duration=2"], &wav);
        let w = add("beep.wav", &wav);
        assert_eq!(w.kind, "audio");
        let v = add("meme.mp4", &vid);
        let g = add("meme.gif", &gif_in);
        assert_eq!(g.kind, "gif");
        assert!(v.has_audio && !g.has_audio && Path::new(&v.proxy).exists() && Path::new(&g.proxy).exists());
        req.media = vec![
            MediaOverlay { kind: "video".into(), path: v.path.clone(), start: 2.5, end: 6.5, x: 0.25, y: 0.3, w: 0.3, aspect: 320.0 / 240.0, volume: 100, has_audio: true, offset: 0.5 },
            MediaOverlay { kind: "gif".into(), path: g.path.clone(), start: 3.0, end: 5.0, x: 0.75, y: 0.7, w: 0.2, aspect: 160.0 / 120.0, volume: 0, has_audio: false, offset: 0.0 },
            MediaOverlay { kind: "audio".into(), path: s.path.clone(), start: 2.0, end: 7.0, x: 0.5, y: 0.5, w: 0.1, aspect: 1.0, volume: 80, has_audio: true, offset: 4.0 },
            MediaOverlay { kind: "audio".into(), path: w.path.clone(), start: 4.0, end: 6.0, x: 0.5, y: 0.5, w: 0.1, aspect: 1.0, volume: 150, has_audio: true, offset: 0.0 },
        ];
        let with_media = export(&src, &req, &|_| {}).unwrap();
        let _ = std::fs::copy(&with_media, work.join("with-media.mp4"));
        let out = info(&with_media).unwrap();
        println!("mp4 with video+gif+2 sounds: {:.2} s, {} track(s)", out.duration, out.tracks.len());
        assert!((out.duration - 5.0).abs() < 0.3, "{}", out.duration);
        assert_eq!(out.tracks.len(), 1);
        req.media.clear();

        // Cancel mid-way: an error, and no file (finished or partial) left behind.
        let target = work.join("cancelled.mp4");
        req.save_to = Some(target.to_string_lossy().into_owned());
        req.start = 0.0;
        req.end = clip_info.duration;
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            CANCEL.store(true, Ordering::Release);
        });
        assert_eq!(export(&src, &req, &|_| {}).unwrap_err(), "cancelled");
        assert!(!target.exists() && !target.with_extension("partial.mp4").exists());
        assert!(!BUSY.load(Ordering::Acquire));
        req.save_to = None;
        req.start = 2.0;
        req.end = 7.0;

        req.format = "gif".into();
        req.overlays.clear();
        let gif = export(&src, &req, &|_| {}).unwrap();
        assert!(gif.extension().is_some_and(|e| e == "gif"));
        println!("gif: {} {} KB", gif.display(), std::fs::metadata(&gif).unwrap().len() / 1024);
        if std::env::var_os("ARCHY_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&work);
        }
    }
}
