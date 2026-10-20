//! Whole-session recording: everything the recorder encodes, also written to a
//! file on disk while you play.
//!
//! No extra encoding: the packets the replay buffer already gets are handed to
//! a writer thread, which appends them to a fragmented MP4 (playable even if
//! Archy or the PC stops mid-session). Bookmarks (the clip key pressed while a
//! session records) go to `<file>.bookmarks.json` for the editor's timeline.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, SyncSender};
use std::time::{Duration, Instant};

use archy_gpu::probe::Codec;
use archy_mux::{AudioTrack, Mp4Writer};

use crate::ring::StoredPacket;

/// What the writer thread is told.
pub enum Msg {
    Video(StoredPacket),
    Audio(usize, StoredPacket),
    Bookmark,
    Stop(SyncSender<Summary>),
}

/// How a session ended.
#[derive(Debug, Clone)]
pub struct Summary {
    pub path: PathBuf,
    pub seconds: f64,
    pub bytes: u64,
    pub bookmarks: usize,
    /// Why it ended early (disk full, write error); `None` = stopped normally.
    pub problem: Option<String>,
}

/// One audio track's format, owned (the writer thread outlives the caller's borrow).
#[derive(Clone)]
pub struct TrackFormat {
    pub sample_rate: u32,
    pub channels: u32,
    pub bytes_per_second: u32,
    pub user_data: Vec<u8>,
}

/// What the file is made from.
#[derive(Clone)]
pub struct Format {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio: Vec<TrackFormat>,
}

/// Below this much free space a session stops (clips still save).
const MIN_FREE: u64 = 1_000_000_000;

/// The sidecar with bookmark times (seconds into the session file).
pub fn bookmarks_path(video: &Path) -> PathBuf {
    let mut p = video.as_os_str().to_owned();
    p.push(".bookmarks.json");
    PathBuf::from(p)
}

/// Bookmarks saved for `video` (empty if none).
pub fn read_bookmarks(video: &Path) -> Vec<f64> {
    std::fs::read_to_string(bookmarks_path(video))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Starts the writer thread; packets sent before the first keyframe are skipped.
pub fn start(path: PathBuf, format: Format) -> Sender<Msg> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("archy-session".into())
        .spawn(move || run(path, format, rx))
        .expect("session writer thread");
    tx
}

fn run(path: PathBuf, format: Format, rx: Receiver<Msg>) {
    let mut writer: Option<Mp4Writer> = None;
    let mut base = 0i64;
    let mut last_video = 0i64;
    let mut bookmarks: Vec<f64> = Vec::new();
    let mut problem: Option<String> = None;
    let mut checked = Instant::now();
    let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let tracks: Vec<AudioTrack> = format
        .audio
        .iter()
        .map(|a| AudioTrack { sample_rate: a.sample_rate, channels: a.channels, bytes_per_second: a.bytes_per_second, user_data: &a.user_data })
        .collect();
    let _ = crate::init_thread();
    for msg in rx {
        match msg {
            Msg::Video(p) if problem.is_none() => {
                if writer.is_none() {
                    if !p.keyframe {
                        continue; // a file can only start on a keyframe
                    }
                    match Mp4Writer::create_fragmented(&path, format.codec, format.width, format.height, format.fps, &p.data, &tracks, Some(p.time)) {
                        Ok(w) => {
                            writer = Some(w);
                            base = p.time;
                        }
                        Err(e) => {
                            problem = Some(format!("couldn't create the session file ({})", e.message()));
                            continue;
                        }
                    }
                }
                last_video = p.time;
                if let Some(w) = writer.as_mut() {
                    if let Err(e) = w.write(&p.data, p.time, p.duration, p.keyframe) {
                        problem = Some(format!("writing failed ({})", e.message()));
                    }
                }
                // A full drive ends the session (clips still save).
                if checked.elapsed() > Duration::from_secs(10) {
                    checked = Instant::now();
                    if crate::files::free_bytes(&folder).is_some_and(|f| f < MIN_FREE) {
                        problem = Some("the drive is almost full".into());
                    }
                }
            }
            Msg::Audio(track, p) if problem.is_none() => {
                if let Some(w) = writer.as_mut() {
                    let _ = w.write_audio(track, &p.data, p.time, p.duration);
                }
            }
            Msg::Bookmark => {
                if writer.is_some() {
                    bookmarks.push(((last_video - base) as f64 / 1e7 * 10.0).round() / 10.0);
                    let _ = std::fs::write(bookmarks_path(&path), serde_json::to_string(&bookmarks).unwrap_or_default());
                }
            }
            Msg::Stop(reply) => {
                let ended = writer.take().map(|w| w.finish());
                if let Some(Err(e)) = ended {
                    problem.get_or_insert(format!("couldn't finish the file ({})", e.message()));
                }
                let _ = reply.send(Summary {
                    path: path.clone(),
                    seconds: (last_video - base).max(0) as f64 / 1e7,
                    bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                    bookmarks: bookmarks.len(),
                    problem,
                });
                return;
            }
            _ => {} // after a problem: everything else is dropped
        }
    }
    // The sender went away without Stop (recorder dropped): still finish the file.
    if let Some(w) = writer {
        let _ = w.finish();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bookmarks_sit_next_to_the_video() {
        let p = std::path::Path::new(r"C:\clips\Roblox 2026-10-20 18-00-00 (session).mp4");
        assert_eq!(
            super::bookmarks_path(p),
            std::path::PathBuf::from(r"C:\clips\Roblox 2026-10-20 18-00-00 (session).mp4.bookmarks.json")
        );
        assert!(super::read_bookmarks(std::path::Path::new("Z:\\nothing.mp4")).is_empty());
    }
}
