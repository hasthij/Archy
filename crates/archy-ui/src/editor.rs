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
    /// Text and images, bottom layer first.
    pub overlays: Vec<Overlay>,
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
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut graph = vec!["[0:v]setpts=PTS-STARTPTS[v0]".to_string()];
    let mut last = "v0".to_string();
    for (i, c) in req.overlays.iter().enumerate() {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD
            .decode(c.png.as_bytes())
            .map_err(|_| "a caption image was damaged")?;
        let file = work.join(format!("caption{i}.png"));
        std::fs::write(&file, png).map_err(|e| e.to_string())?;
        inputs.push(file);
        let (a, b) = ((c.start - req.start).max(0.0), (c.end - req.start).max(0.0));
        let next = format!("v{}", i + 1);
        graph.push(format!(
            "[{last}][{}:v]overlay=0:0:enable='between(t,{a:.3},{b:.3})'[{next}]",
            i + 1
        ));
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
    let has_audio = !gif && !active.is_empty();
    if has_audio {
        let mut labels = String::new();
        for (i, v) in &active {
            graph.push(format!("[0:a:{i}]volume={:.2}[a{i}]", *v as f32 / 100.0));
            labels.push_str(&format!("[a{i}]"));
        }
        let loud = active.len() > 1 || active.iter().any(|(_, v)| *v > 100);
        let mix = if active.len() > 1 {
            format!("{labels}amix=inputs={}:normalize=0:duration=longest", active.len())
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
        for i in &inputs {
            cmd.arg("-i").arg(i);
        }
        cmd.args(["-filter_complex", &graph.join(";"), "-map", "[vout]"]);
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
        };
        let last = std::cell::Cell::new(0.0);
        let mp4 = export(&src, &req, &|p| last.set(p)).unwrap();
        let out = info(&mp4).unwrap();
        println!("mp4: {} {:.2} s, {} track(s), progress reached {:.2}, encoder {:?}", mp4.display(), out.duration, out.tracks.len(), last.get(), ENCODER.lock().unwrap());
        assert!((out.duration - 5.0).abs() < 0.3, "{}", out.duration);
        assert_eq!(out.tracks.len(), usize::from(!clip_info.tracks.is_empty()));
        assert!(last.get() > 0.5);

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
        let _ = std::fs::remove_dir_all(&work);
    }
}
