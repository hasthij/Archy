//! The always-on recorder: capture → convert → encode into the replay buffer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use archy_audio::{AacInfo, AudioCapture, Source};
use archy_capture::{Capture, Target};
use archy_encode::{Backend, Config, Encoder, InFlight, Input};
use archy_gpu::convert::{fit_size, AnyConverter, ConvertMode};
use archy_gpu::device::{Gpu, RECORDER_GPU_PRIORITY};
use archy_gpu::probe::Codec;
use archy_gpu::SendPtr;
use archy_mux::{AudioTrack, Mp4Writer};
use windows::core::{Error, Result};
use windows::Win32::Foundation::{E_FAIL, LUID};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use crate::cadence::{clock_now, Pacer};
use crate::ring::{ReplayBuffer, StoredPacket};

/// NV12 textures in rotation: more than can ever be in flight (QUEUE_DEPTH, plus
/// one being written), so a texture is never reused while the encoder needs it.
const NV12_POOL: usize = 12;
/// Frames allowed to wait for the encoder. Beyond this a frame is dropped, never queued.
/// Six absorbs an encoder's occasional slow frame (Media Foundation encoders had
/// ~1-2% drops with three, each a hole in the clip's constant frame rate).
const QUEUE_DEPTH: usize = 6;

#[derive(Clone, Debug)]
pub struct RecorderConfig {
    pub target: Target,
    pub fps: u32,
    /// Output height cap (aspect kept); sources smaller than this keep their size.
    pub max_height: u32,
    /// Codecs to try in order (first that has a hardware encoder wins).
    pub codecs: Vec<Codec>,
    pub max_bitrate: u32,
    /// `Some`: VBR aiming at this many bits/s (predictable size). `None`: constant quality.
    pub target_bitrate: Option<u32>,
    pub quality: u32,
    /// Seconds of history kept in RAM.
    pub buffer_seconds: u32,
    pub convert: ConvertMode,
    /// GPU to encode on ("vvvv:dddd" from the GPU list); empty = automatic.
    /// If it's missing or can't encode, recording falls back to automatic.
    pub gpu: String,
    /// Audio tracks, in file order (track 1 is what single-track players play).
    pub audio: Vec<AudioSource>,
    pub audio_kbps: u32,
    /// Encoders (`RecorderInfo::encoder_key`) that failed on this PC: skipped, so
    /// the next encoder, GPU or codec is used instead.
    pub avoid: Vec<String>,
    /// `true`: every frame exactly 1/fps apart, repeats filling the gaps (what
    /// After Effects / Premiere need). `false`: each frame keeps the moment it was
    /// shown (smoothest playback; how 0.4.0 recorded). With constant timing a
    /// 60 fps clip from a 144 Hz screen judders: frames shown 14 and 21 ms apart
    /// get stamped 16.7 ms apart.
    pub constant_rate: bool,
}

#[derive(Clone, Debug)]
pub struct AudioSource {
    pub name: String,
    pub source: Source,
    /// Live volumes for this track (0-200%).
    pub volumes: archy_audio::Volumes,
}

struct AudioTrackState {
    name: String,
    info: AacInfo,
    ring: Arc<Mutex<ReplayBuffer>>,
    _capture: AudioCapture,
}

/// What the recorder ended up running with.
#[derive(Clone, Debug)]
pub struct RecorderInfo {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
    /// Encoder + codec, as `RecorderConfig::avoid` names it.
    pub encoder_key: String,
    /// The GPU recording runs on.
    pub gpu: String,
    /// Recording on the Intel oneVPL path (the GPU priority adapts only there; see GpuContest).
    pub vpl: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecorderStatus {
    Recording,
    /// The captured window closed; the recorder has stopped on its own.
    TargetClosed,
    Failed,
}

#[derive(Debug)]
pub struct ClipInfo {
    pub path: PathBuf,
    pub seconds: f64,
    pub bytes: u64,
    pub save_time: Duration,
}

struct Shared {
    ring: Mutex<ReplayBuffer>,
    stop: AtomicBool,
    finished: AtomicBool,
    failed: AtomicBool,
    frames: AtomicU64,
    dropped: AtomicU64,
    /// Packets the encoder labelled wrongly (keyframe or not), per the bitstream.
    mislabeled: AtomicU64,
    /// The encoder's own counters, once it has finished.
    encoder_stats: Mutex<Option<archy_encode::Stats>>,
    /// Why recording ended, for the log and the user.
    reason: Mutex<String>,
    /// Capture-clock time of the encoder's latest packet (0: none yet).
    last_output: AtomicI64,
    /// Capture-clock time of the latest frame handed to the encoder.
    last_input: AtomicI64,
    /// Repeats written as near-free skip frames (oneVPL).
    skip_frames: AtomicU64,
    /// A screenshot waiting for the capture loop (see `Recorder::screenshot`).
    screenshot: Mutex<Option<crate::screenshot::Request>>,
    /// The whole-session writer, while a session records (see `start_session`).
    session: Arc<Mutex<Option<std::sync::mpsc::Sender<crate::session::Msg>>>>,
}

/// A GPU encoder that hasn't taken a single frame for this long while frames
/// wait has hung (a driver fault); recording ends so the tray can start it again.
const ENCODER_STALL: Duration = Duration::from_secs(10);
/// How long to wait for the encoder to finish when recording stops. A hung
/// driver can block it forever; Archy must never freeze waiting for it.
const ENCODER_EXIT_WAIT: Duration = Duration::from_secs(5);
/// A started recording must be up within this; a driver that hangs while the
/// encoder is created must never freeze the caller (the tray's message thread).
const START_LIMIT: Duration = Duration::from_secs(15);
/// Frames going in but no packet coming out for this long: the encoder has
/// silently stopped (seen with misbehaving drivers). Recording ends with an
/// "encoder" reason so the tray restarts on the next encoder.
/// Long enough that a GPU overloaded for a moment (a game plus a calibration:
/// frames taking over a second each) isn't mistaken for a dead encoder.
const OUTPUT_STALL: i64 = 80_000_000; // 8 s, capture-clock units


pub struct Recorder {
    shared: Arc<Shared>,
    capture_thread: Option<JoinHandle<()>>,
    audio: Vec<AudioTrackState>,
    pub info: RecorderInfo,
    /// Audio tracks that started (and any that failed, with the reason).
    pub audio_report: Vec<String>,
}

impl Recorder {
    /// Starts recording into the replay buffer. Returns once the pipeline is up.
    pub fn start(cfg: RecorderConfig) -> Result<Self> {
        let audio_sources = cfg.audio.clone();
        let audio_kbps = cfg.audio_kbps;
        let buffer_seconds = cfg.buffer_seconds;
        let shared = Arc::new(Shared {
            ring: Mutex::new(ReplayBuffer::new(cfg.buffer_seconds)),
            stop: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            mislabeled: AtomicU64::new(0),
            encoder_stats: Mutex::new(None),
            reason: Mutex::new(String::new()),
            last_output: AtomicI64::new(0),
            last_input: AtomicI64::new(0),
            skip_frames: AtomicU64::new(0),
            screenshot: Mutex::new(None),
            session: Arc::new(Mutex::new(None)),
        });
        let (ready_tx, ready_rx) = sync_channel::<Result<RecorderInfo>>(1);
        let thread_shared = shared.clone();
        let capture_thread = thread::Builder::new()
            .name("archy-capture".into())
            .spawn(move || {
                let _mmcss = crate::eco::video_thread();
                let result = capture_loop(cfg, &thread_shared, &ready_tx);
                if let Ok(mut r) = thread_shared.reason.lock() {
                    *r = match &result {
                        Err(e) => e.message(),
                        Ok(()) if thread_shared.stop.load(Ordering::Acquire) => "stopped".into(),
                        Ok(()) => "the captured window closed".into(),
                    };
                }
                if let Err(e) = result {
                    thread_shared.failed.store(true, Ordering::Release);
                    // Start-up failures go back to `start`; later ones just end the recording.
                    let _ = ready_tx.try_send(Err(e));
                }
                thread_shared.finished.store(true, Ordering::Release);
            })
            .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
        let info = match ready_rx.recv_timeout(START_LIMIT) {
            Ok(r) => r?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // The thread is stuck in a driver call: tell it to stop if it ever
                // returns, and leave it behind rather than wait forever.
                shared.stop.store(true, Ordering::Release);
                return Err(Error::new(E_FAIL, "the GPU encoder took over 15 s to start (driver problem)"));
            }
            Err(_) => return Err(Error::new(E_FAIL, "recorder thread exited during start-up")),
        };
        // Audio never blocks video: a source that can't start is reported and skipped.
        let mut audio = Vec::new();
        let mut audio_report = Vec::new();
        for src in audio_sources {
            let ring = Arc::new(Mutex::new(ReplayBuffer::new(buffer_seconds)));
            let sink = ring.clone();
            let session = shared.session.clone();
            let track = audio.len(); // its index if it starts
            match AudioCapture::start(src.source.clone(), audio_kbps, src.volumes.clone(), move |p| {
                let packet = StoredPacket { data: Arc::from(p.data), time: p.time, duration: p.duration, keyframe: true };
                if let Some(tx) = session.lock().ok().and_then(|s| s.clone()) {
                    let _ = tx.send(crate::session::Msg::Audio(track, packet.clone()));
                }
                if let Ok(mut r) = sink.lock() {
                    r.push(StoredPacket {
                        data: packet.data,
                        time: p.time,
                        duration: p.duration,
                        keyframe: true,
                    });
                }
            }) {
                Ok(capture) => {
                    audio_report.push(format!("{}: ok", src.name));
                    audio.push(AudioTrackState {
                        name: src.name,
                        info: capture.info.clone(),
                        ring,
                        _capture: capture,
                    });
                }
                Err(e) => audio_report.push(format!("{}: {}", src.name, e.message())),
            }
        }
        Ok(Self {
            shared,
            capture_thread: Some(capture_thread),
            audio,
            info,
            audio_report,
        })
    }

    /// Stops capturing (video and audio) without waiting, and keeps the replay
    /// buffer readable: a recording that is replaced (a setting changed, the
    /// encoder failed, the game was minimized) can still be saved from. The
    /// threads are joined when the recorder is dropped.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        for a in &self.audio {
            a._capture.stop();
        }
    }

    /// Starts writing everything recorded from now on to `path` (fragmented MP4,
    /// from the next keyframe). No extra encoding: the same packets as the replay
    /// buffer. Does nothing if a session is already running.
    pub fn start_session(&self, path: &Path) -> bool {
        let Ok(mut slot) = self.shared.session.lock() else { return false };
        if slot.is_some() {
            return false;
        }
        let format = crate::session::Format {
            codec: self.info.codec,
            width: self.info.width,
            height: self.info.height,
            fps: self.info.fps,
            audio: self
                .audio
                .iter()
                .map(|a| crate::session::TrackFormat {
                    sample_rate: a.info.sample_rate,
                    channels: a.info.channels,
                    bytes_per_second: a.info.bytes_per_second,
                    user_data: a.info.user_data.clone(),
                })
                .collect(),
        };
        *slot = Some(crate::session::start(path.to_path_buf(), format));
        true
    }

    /// Whether a session is being written.
    pub fn session_active(&self) -> bool {
        self.shared.session.lock().map(|s| s.is_some()).unwrap_or(false)
    }

    /// Marks this moment in the session (the clip key while a session records).
    pub fn bookmark(&self) -> bool {
        match self.shared.session.lock().ok().and_then(|s| s.clone()) {
            Some(tx) => tx.send(crate::session::Msg::Bookmark).is_ok(),
            None => false,
        }
    }

    /// Ends the session and finishes its file (waits up to 10 s).
    pub fn end_session(&self) -> Option<crate::session::Summary> {
        let tx = self.shared.session.lock().ok()?.take()?;
        let (reply, summary) = sync_channel(1);
        tx.send(crate::session::Msg::Stop(reply)).ok()?;
        summary.recv_timeout(Duration::from_secs(10)).ok()
    }

    /// A full-size PNG of what's being recorded, written to `path`. The capture
    /// loop takes the next frame (or, on a still screen, the last recorded one);
    /// returns once the file is written (about 0.1-0.3 s).
    pub fn screenshot(&self, path: &Path) -> std::result::Result<PathBuf, String> {
        if self.status() != RecorderStatus::Recording {
            return Err("not recording".into());
        }
        let (reply, result) = sync_channel(1);
        let request = crate::screenshot::Request { path: path.to_path_buf(), reply, asked: Instant::now() };
        match self.shared.screenshot.lock() {
            Ok(mut slot) => *slot = Some(request),
            Err(_) => return Err("recorder state poisoned".into()),
        }
        result.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| Err("the picture didn't come back in time".into()))
    }

    /// Seconds of video in the replay buffer.
    pub fn buffered_seconds(&self) -> f64 {
        self.shared.ring.lock().map(|r| r.span_seconds()).unwrap_or(0.0)
    }

    /// Names of the audio tracks being recorded, in file order.
    pub fn audio_tracks(&self) -> Vec<String> {
        self.audio.iter().map(|a| a.name.clone()).collect()
    }

    pub fn status(&self) -> RecorderStatus {
        if self.shared.failed.load(Ordering::Acquire) {
            RecorderStatus::Failed
        } else if self.shared.finished.load(Ordering::Acquire) {
            RecorderStatus::TargetClosed
        } else {
            RecorderStatus::Recording
        }
    }

    /// Why recording ended (empty while still recording).
    pub fn stop_reason(&self) -> String {
        self.shared
            .reason
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Encoder trouble worth logging when a recording ends (empty when all was well).
    pub fn health(&self) -> String {
        let mut notes = Vec::new();
        let dropped = self.shared.dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            notes.push(format!("{dropped} frames skipped (encoder busy)"));
        }
        let mislabeled = self.shared.mislabeled.load(Ordering::Relaxed);
        if mislabeled > 0 {
            notes.push(format!("{mislabeled} keyframe labels corrected"));
        }
        let skips = self.shared.skip_frames.load(Ordering::Relaxed);
        if skips > 0 {
            notes.push(format!("{skips} repeats written as skip frames"));
        }
        if let Some(s) = self.shared.encoder_stats.lock().ok().and_then(|s| s.clone()) {
            if s.late > 0 {
                notes.push(format!("{} slow frames (>1 s)", s.late));
            }
            if s.forced_keyframes > 0 {
                notes.push(format!("{} keyframes forced (driver ignored the interval)", s.forced_keyframes));
            }
        }
        notes.join(", ")
    }

    /// (frames recorded, frames dropped because the encoder was busy).
    pub fn frame_counts(&self) -> (u64, u64) {
        (
            self.shared.frames.load(Ordering::Relaxed),
            self.shared.dropped.load(Ordering::Relaxed),
        )
    }

    /// Bytes of video currently held in the replay buffer.
    pub fn buffer_bytes(&self) -> usize {
        self.shared.ring.lock().map(|r| r.bytes()).unwrap_or(0)
    }

    /// Writes the last `seconds` to `path` as MP4. Only copies packets; nothing is
    /// re-encoded. The file appears atomically (written under a temporary name).
    pub fn save_clip(&self, seconds: u32, path: &Path) -> Result<ClipInfo> {
        let started = Instant::now();
        let _fast = crate::eco::Unthrottled::new();
        let packets = self
            .shared
            .ring
            .lock()
            .map_err(|_| Error::new(E_FAIL, "replay buffer poisoned"))?
            .last(seconds);
        let Some(first) = packets.first() else {
            return Err(Error::new(E_FAIL, "nothing recorded yet"));
        };
        crate::init_thread()?;
        let start = first.time;
        let last = packets.last().expect("non-empty");
        let end = last.time + last.duration;
        // Audio covering exactly the video's time range. Tracks with nothing in that
        // window (e.g. a device that was idle) are left out instead of written empty.
        let mut audio: Vec<Vec<StoredPacket>> = Vec::new();
        let mut tracks: Vec<AudioTrack> = Vec::new();
        let mut names: Vec<&str> = Vec::new();
        for a in &self.audio {
            let clip: Vec<StoredPacket> = a
                .ring
                .lock()
                .map(|r| {
                    r.last(seconds + 2)
                        .into_iter()
                        .filter(|p| p.time >= start && p.time < end)
                        .collect()
                })
                .unwrap_or_default();
            if clip.is_empty() {
                continue;
            }
            audio.push(clip);
            names.push(&a.name);
            tracks.push(AudioTrack {
                sample_rate: a.info.sample_rate,
                channels: a.info.channels,
                bytes_per_second: a.info.bytes_per_second,
                user_data: &a.info.user_data,
            });
        }
        let partial = path.with_extension("partial.mp4");
        let info = &self.info;
        let write = || -> Result<()> {
            let mut writer = Mp4Writer::create_with_audio(
                &partial,
                info.codec,
                info.width,
                info.height,
                info.fps,
                &first.data,
                &tracks,
                Some(start),
            )?;
            // Interleave by time so the file plays smoothly from any point.
            let mut next = vec![0usize; audio.len()];
            for v in &packets {
                for (t, track) in audio.iter().enumerate() {
                    while next[t] < track.len() && track[next[t]].time <= v.time {
                        let a = &track[next[t]];
                        writer.write_audio(t, &a.data, a.time, a.duration)?;
                        next[t] += 1;
                    }
                }
                writer.write(&v.data, v.time, v.duration, v.keyframe)?;
            }
            for (t, track) in audio.iter().enumerate() {
                for a in &track[next[t]..] {
                    writer.write_audio(t, &a.data, a.time, a.duration)?;
                }
            }
            writer.finish()
        };
        if let Err(e) = write() {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
        append_track_names(&partial, &names);
        // OneDrive or antivirus can hold a new file for a moment: retry, and never
        // leave a stray .partial.mp4 behind.
        let mut renamed = std::fs::rename(&partial, path);
        for _ in 0..10 {
            if renamed.is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
            renamed = std::fs::rename(&partial, path);
        }
        if let Err(e) = renamed {
            let _ = std::fs::remove_file(&partial);
            return Err(Error::new(E_FAIL, format!("couldn't finish the clip file ({e})")));
        }
        Ok(ClipInfo {
            path: path.to_path_buf(),
            seconds: (last.time + last.duration - first.time) as f64 / 1e7,
            bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            save_time: started.elapsed(),
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(t) = self.capture_thread.take() {
            let _ = t.join();
        }
    }
}

/// The recorder's GPU priority, adapted to how busy the GPU is. It starts below
/// the game's (RECORDER_GPU_PRIORITY: the game wins every contest, zero FPS
/// cost). On a GPU the game keeps busy (integrated graphics, where the game and
/// the recorder share one chip) the recorder's small jobs then wait behind every
/// game frame, its queue fills and the clip loses frames: 13% at 1080p60 in
/// Roblox on Intel UHD, seen as stutter. So while frames are being lost the
/// recorder gets the same priority as the game (its work is ~1-2 ms a frame),
/// and drops back once it has been clean for a while.
/// Only on the oneVPL (Intel) path: on Media Foundation the encoder shares this
/// device, and raising it there would also raise NVENC/AMF work, whose cost to
/// the game is untested.
struct GpuContest {
    dxgi: Option<windows::Win32::Graphics::Dxgi::IDXGIDevice>,
    boosted: bool,
    at: Instant,
    clean_since: Instant,
    counts: (u64, u64),
}

impl GpuContest {
    const EVERY: Duration = Duration::from_secs(1);
    const CALM: Duration = Duration::from_secs(30);

    fn new(device: &windows::Win32::Graphics::Direct3D11::ID3D11Device) -> Self {
        use windows::core::Interface;
        Self { dxgi: device.cast().ok(), boosted: false, at: Instant::now(), clean_since: Instant::now(), counts: (0, 0) }
    }

    fn check(&mut self, shared: &Shared) {
        if self.at.elapsed() < Self::EVERY {
            return;
        }
        self.at = Instant::now();
        let now = (shared.frames.load(Ordering::Relaxed), shared.dropped.load(Ordering::Relaxed));
        let (sent, lost) = (now.0 - self.counts.0, now.1 - self.counts.1);
        self.counts = now;
        let losing = lost * 50 > sent + lost; // over 2% of this second's frames
        let want = if losing {
            self.clean_since = Instant::now();
            true
        } else {
            self.boosted && self.clean_since.elapsed() < Self::CALM
        };
        if want != self.boosted {
            self.boosted = want;
            if let Some(d) = &self.dxgi {
                unsafe {
                    let _ = d.SetGPUThreadPriority(if want { 0 } else { RECORDER_GPU_PRIORITY });
                }
            }
        }
    }
}

/// Starts the track-name note at the end of a saved clip.
pub const TRACK_NAMES_MARKER: &[u8] = b"archy-tracks:";

/// Notes the audio track names ("Everything|Game|Microphone") at the end of the
/// file, for the clip editor. It's a top-level MP4 `free` box, which every player
/// skips. Best effort: a clip without it still plays; the editor says "Track N".
pub fn append_track_names(path: &Path, names: &[&str]) {
    use std::io::Write;
    if names.is_empty() {
        return;
    }
    let payload = [TRACK_NAMES_MARKER, names.join("|").as_bytes()].concat();
    let mut bx = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    bx.extend_from_slice(b"free");
    bx.extend_from_slice(&payload);
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = f.write_all(&bx);
    }
}

/// How `RecorderConfig::avoid` names an encoder: its name and the codec.
pub fn encoder_key(name: &str, codec: Codec) -> String {
    format!("{name} | {codec:?}")
}

fn open_encoder(
    gpu: &Gpu,
    cfg: &RecorderConfig,
    width: u32,
    height: u32,
) -> Result<(Encoder, Codec)> {
    let mut last = Error::new(E_FAIL, "no codec configured");
    // Backend first, then codec: on Intel, every codec is tried on oneVPL (fixed-function)
    // before Media Foundation, whose Intel AV1 encoder on GPUs without hardware AV1
    // runs partly on shader cores. NVIDIA/AMD have no oneVPL and go straight to MF.
    // ARCHY_ENCODER=mf|vpl forces one path, e.g. to test the AMD/NVIDIA (Media
    // Foundation) path on an Intel PC.
    let backends: &[Backend] = match std::env::var("ARCHY_ENCODER").as_deref() {
        Ok("mf") => &[Backend::MediaFoundation],
        Ok("vpl") => &[Backend::Vpl],
        _ => &[Backend::Vpl, Backend::MediaFoundation],
    };
    // An encoder that failed earlier is only used when nothing else works:
    // recording must never stop entirely because of a ban.
    let mut fallback: Option<(Encoder, Codec)> = None;
    for &backend in backends {
        for &codec in &cfg.codecs {
            let enc_cfg = Config {
                codec,
                width,
                height,
                fps: cfg.fps,
                max_bitrate: cfg.max_bitrate,
                quality: cfg.quality,
                target_bitrate: cfg.target_bitrate,
                // A keyframe every 2 s: half the expensive full pictures of a 1 s GOP, so
                // more quality per MB. Clips may start up to 2 s earlier than asked.
                gop: cfg.fps * 2,
            };
            match Encoder::new(backend, gpu, &enc_cfg) {
                Ok(e) if cfg.avoid.contains(&encoder_key(e.name(), codec)) => {
                    if fallback.is_none() {
                        fallback = Some((e, codec));
                    }
                }
                Ok(e) => return Ok((e, codec)),
                Err(e) => last = e,
            }
        }
    }
    fallback.ok_or(last)
}

fn capture_loop(
    cfg: RecorderConfig,
    shared: &Arc<Shared>,
    ready: &std::sync::mpsc::SyncSender<Result<RecorderInfo>>,
) -> Result<()> {
    crate::init_thread()?;
    let interval = Duration::from_secs_f64(1.0 / cfg.fps as f64);
    // Windows' default GPU first; if it has no video encoder (e.g. an NVIDIA MX
    // laptop GPU), every other GPU, Intel first. Capture, conversion and
    // encoding all run on the GPU that has the encoder.
    let mut adapters: Vec<Option<LUID>> = vec![None];
    if !cfg.gpu.is_empty() {
        match archy_gpu::device::adapter_by_id(&cfg.gpu) {
            Some(luid) => adapters.insert(0, Some(luid)),
            None => {} // not present any more (e.g. an unplugged eGPU): automatic
        }
    }
    let mut tried_others = false;
    let mut last_error = Error::new(E_FAIL, "no GPU found");
    let mut index = 0;
    let (gpu, capture, mut converter, mut encoder, codec, width, height) = loop {
        let Some(&adapter) = adapters.get(index) else {
            return Err(last_error);
        };
        index += 1;
        let attempt = (|| -> Result<_> {
            let gpu = Gpu::new(adapter, RECORDER_GPU_PRIORITY)?;
            // At most one frame per 3/4 interval: enough for every frame a game at
            // up to 1.33x the clip's rate shows (so none is lost on a 144 Hz screen),
            // and on 165-240 Hz screens the compositor copies far fewer frames
            // nobody uses (half the interval let through up to 120 a second for 60).
            let capture = Capture::start(&gpu.device, cfg.target, interval * 3 / 4)?;
            let (width, height) = fit_size(capture.width, capture.height, cfg.max_height);
            // Hardware encoders reject tiny frames (a minimized or collapsed window).
            let (width, height) = (width.max(160), height.max(120));
            let converter = AnyConverter::new(
                &gpu,
                cfg.convert,
                (capture.width, capture.height),
                (width, height),
                cfg.fps,
                NV12_POOL,
            )?;
            if archy_encode::fault("start_hang") {
                thread::sleep(Duration::from_secs(25)); // a driver call that never returns in time
            }
            let (encoder, codec) = open_encoder(&gpu, &cfg, width, height)?;
            Ok((gpu, capture, converter, encoder, codec, width, height))
        })();
        match attempt {
            Ok(parts) => break parts,
            Err(e) => {
                if !tried_others {
                    tried_others = true;
                    let default = Gpu::new(None, RECORDER_GPU_PRIORITY).map(|g| g.luid).ok();
                    let except = default.unwrap_or_default();
                    adapters.extend(archy_gpu::device::other_adapters(except).into_iter().map(Some));
                }
                last_error = e;
            }
        }
    };
    let info = RecorderInfo {
        codec,
        width,
        height,
        fps: cfg.fps,
        encoder: encoder.name().to_string(),
        encoder_key: encoder_key(encoder.name(), codec),
        gpu: archy_gpu::device::adapter_name(gpu.luid),
        vpl: matches!(encoder, Encoder::Vpl(_)),
    };
    let vpl = info.vpl;

    let (tx, rx) = sync_channel::<SendPtr<Input>>(QUEUE_DEPTH);
    let enc_shared = shared.clone();
    let encoder_thread = thread::Builder::new()
        .name("archy-encode".into())
        .spawn(move || -> Result<()> {
            crate::init_thread()?;
            let _mmcss = crate::eco::video_thread();
            // Latest codec header (SPS/PPS, VPS/SPS/PPS) seen in the stream. Some
            // drivers send it only once, at the start; a clip starting at a later
            // keyframe then has no header and can't be written. Such keyframes get
            // the cached header put in front.
            let mut header: Option<Vec<u8>> = None;
            let mut packets = 0u64;
            let stall = archy_encode::fault("output_stall");
            let stats = encoder.run(rx, |p| {
                packets += 1;
                if stall && packets > 120 {
                    thread::sleep(Duration::from_secs(60)); // as if the driver hung mid-stream
                }
                // Clips may only start on a frame that really resets the decoder, so
                // the bitstream decides; starting on a mislabelled frame shows blocks
                // and smeared colours until the next real keyframe.
                let keyframe = archy_encode::random_access(codec, p.data).unwrap_or(p.keyframe);
                if keyframe != p.keyframe {
                    enc_shared.mislabeled.fetch_add(1, Ordering::Relaxed);
                }
                let data: Arc<[u8]> = if keyframe && codec != Codec::Av1 {
                    match archy_mux::codec_header(codec, p.data) {
                        Some(h) => {
                            header = Some(h);
                            Arc::from(p.data)
                        }
                        None => match &header {
                            Some(h) => [h.as_slice(), p.data].concat().into(),
                            None => Arc::from(p.data),
                        },
                    }
                } else {
                    Arc::from(p.data)
                };
                let packet = StoredPacket {
                    data,
                    time: p.time,
                    duration: p.duration,
                    keyframe,
                };
                if let Some(tx) = enc_shared.session.lock().ok().and_then(|s| s.clone()) {
                    let _ = tx.send(crate::session::Msg::Video(packet.clone()));
                }
                if let Ok(mut ring) = enc_shared.ring.lock() {
                    ring.push(packet);
                }
                enc_shared.last_output.store(clock_now(), Ordering::Release);
                Ok(())
            });
            if let (Ok(s), Ok(mut slot)) = (&stats, enc_shared.encoder_stats.lock()) {
                enc_shared.skip_frames.store(s.skipped, Ordering::Relaxed);
                *slot = Some(s.clone());
            }
            stats.map(|_| ())
        })
        .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
    let _ = ready.send(Ok(info));

    let mut pacer = Pacer::new(cfg.fps);
    // ARCHY_PACE_TRACE=1: timings of the first two seconds, for diagnosing start-up.
    let trace = std::env::var_os("ARCHY_PACE_TRACE").is_some();
    let t_start = Instant::now();
    // The last converted frame, repeated on slots that get no new frame.
    let mut last: Option<ID3D11Texture2D> = None;
    // A stale first frame held back (see below).
    let mut stale: Option<ID3D11Texture2D> = None;
    let constant = cfg.constant_rate;
    // A screenshot being read back from the GPU.
    let mut pending_shot: Option<crate::screenshot::Pending> = None;
    // Time of the last frame sent (smooth timing keeps times strictly increasing).
    let mut last_time = i64::MIN;
    // Since when every frame has found the encoder's queue full.
    let mut backed_up: Option<Instant> = None;
    // Sends one frame stamped `time`; Err ends the recording.
    // A full queue gets up to one frame to make room (this thread waits, never
    // the game): dropping at once left a gap in the clip every time the encoder
    // had a slow frame, e.g. while it warms up.
    let patience = Duration::from_micros(1_000_000 / cfg.fps.max(1) as u64);
    // Frames the encoder hasn't finished taking (queued, or being read).
    let in_flight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let busy = {
        let in_flight = in_flight.clone();
        move || in_flight.load(Ordering::Acquire) >= QUEUE_DEPTH
    };
    let mut send = |texture: &ID3D11Texture2D, time: i64, repeat: bool| -> Result<()> {
        let mut input = SendPtr(Input { texture: texture.clone(), time, ticket: Some(InFlight::new(&in_flight)), repeat });
        let waited_from = Instant::now();
        let outcome = loop {
            match tx.try_send(input) {
                Err(TrySendError::Full(back)) if waited_from.elapsed() < patience => {
                    input = back;
                    thread::sleep(Duration::from_micros(500));
                }
                other => break other,
            }
        };
        match outcome {
            Ok(()) => {
                // The output watchdog starts with the first frame sent.
                let now = clock_now();
                let _ = shared.last_output.compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire);
                shared.last_input.store(now, Ordering::Release);
                shared.frames.fetch_add(1, Ordering::Relaxed);
                backed_up = None;
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                shared.dropped.fetch_add(1, Ordering::Relaxed);
                // The queue has stayed full the whole time: the encoder has hung.
                if backed_up.get_or_insert_with(Instant::now).elapsed() > ENCODER_STALL {
                    return Err(Error::new(E_FAIL, "the GPU encoder stopped responding"));
                }
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(Error::new(E_FAIL, "encoder stopped")),
        }
    };
    let mut contest = GpuContest::new(&gpu.device);
    // Off by default: on a GPU shared with the game (integrated graphics), raising
    // the recorder to the game's priority took GPU time from the game and it felt
    // laggy (2026-10-15). The recorder always yields to the game, as in 0.4.0.
    // ARCHY_GPU_BOOST=1 turns it back on for testing.
    let gpu_boost = std::env::var_os("ARCHY_GPU_BOOST").is_some();
    let result = loop {
        if shared.stop.load(Ordering::Acquire) || capture.is_closed() {
            break Ok(());
        }
        if vpl && gpu_boost {
            contest.check(shared);
        }
        // A screenshot copy on its way back from the GPU (never waits for it).
        if let Some(p) = pending_shot.take() {
            pending_shot = crate::screenshot::poll(&gpu.context, p);
        }
        // Frames are still going in (the last one under a second ago), yet nothing
        // has come out for 8 s: the encoder has stopped. A still screen sends no
        // frames, and an encoder keeps its last few frames until more arrive, so
        // "no output" alone isn't a stall (that false alarm banned working
        // encoders in 0.5.0 whenever a window stood still).
        let out = shared.last_output.load(Ordering::Acquire);
        let now = clock_now();
        if out != 0 && now - out > OUTPUT_STALL && now - shared.last_input.load(Ordering::Acquire) < 10_000_000 {
            break Err(Error::new(E_FAIL, "the GPU encoder stopped producing video (no output for 8 s)"));
        }
        // Sleeps until the compositor has a new frame or the next slot is due,
        // whichever is first (a static screen still gets its slots, as repeats).
        let wait = match pacer.deadline() {
            Some(due) if constant => Duration::from_micros(((due - clock_now()).max(0) / 10) as u64)
                .clamp(Duration::from_millis(1), Duration::from_millis(100)),
            _ => Duration::from_millis(100),
        };
        // Slots the pacer had to leave empty are frames missing from the clip
        // (in smooth timing there are no slots to fill: a quiet screen is just quiet).
        let lost = pacer.take_lost();
        if lost > 0 && constant {
            shared.dropped.fetch_add(lost, Ordering::Relaxed);
        }
        let t_wait = Instant::now();
        let frame = match capture.next(wait) {
            Ok(f) => f,
            Err(e) => break Err(e),
        };
        if trace && t_start.elapsed() < Duration::from_secs(2) {
            eprintln!(
                "[pace] {:>5} ms: waited {:>4} ms for {}, frame age {} ms",
                t_start.elapsed().as_millis(),
                t_wait.elapsed().as_millis(),
                if frame.is_some() { "a frame" } else { "nothing" },
                frame.as_ref().map_or(0, |f| (clock_now() - f.time) / 10_000)
            );
        }
        let Some(frame) = frame else {
            // Started with only a stale frame and nothing fresh came: use it now.
            if !pacer.started() {
                if let Some(tex) = stale.take() {
                    let at = pacer.frame(clock_now(), clock_now()).map(|s| s.at).unwrap_or_default();
                    if let Err(e) = send(&tex, at, false) {
                        break Err(e);
                    }
                    last_time = at;
                    last = Some(tex);
                }
                continue;
            }
            // A still screen sends no frames: a screenshot asked for a while ago
            // takes the last recorded frame instead.
            if let (None, Some(tex)) = (&pending_shot, &last) {
                let waiting = shared.screenshot.lock().ok().and_then(|mut s| {
                    s.take_if(|r| r.asked.elapsed() > Duration::from_millis(250))
                });
                if let Some(req) = waiting {
                    match crate::screenshot::begin_nv12(&gpu.device, &gpu.context, tex, req) {
                        Ok(p) => pending_shot = Some(p),
                        Err(req) => {
                            let _ = req.reply.try_send(Err("couldn't copy the picture".into()));
                        }
                    }
                }
            }
            // Smooth timing on a still screen: re-send the last frame twice a second.
            // An encoder keeps its last few frames until more arrive, so without
            // this a still screen (a menu, a paused game) never reached the clip
            // buffer and saving said "nothing recorded yet". Repeats are near-free
            // (skip frames on Intel).
            if let (Some(tex), false, false) = (&last, busy(), constant) {
                let now = clock_now();
                if now.saturating_sub(last_time) >= 5_000_000 {
                    if let Err(e) = send(tex, now, true) {
                        break Err(e);
                    }
                    last_time = now;
                }
            }
            // Nothing new: repeat the last frame on every slot that's due (unless
            // the encoder is behind: those slots stay open and are filled later).
            // Smooth timing has no slots to fill.
            if let (Some(tex), false, true) = (&last, busy(), constant) {
                if let Err(e) = pacer.idle(clock_now()).into_iter().try_for_each(|t| send(tex, t, true)) {
                    break Err(e);
                }
            }
            continue;
        };
        if pending_shot.is_none() {
            let waiting = shared.screenshot.lock().ok().and_then(|mut s| s.take());
            if let Some(req) = waiting {
                match crate::screenshot::begin_bgra(&gpu.device, &gpu.context, &frame.texture, frame.content_width, frame.content_height, req) {
                    Ok(p) => pending_shot = Some(p),
                    Err(req) => {
                        let _ = req.reply.try_send(Err("couldn't copy the picture".into()));
                    }
                }
            }
        }
        // The first frame of a capture is what was last on screen, often from a
        // second ago: hold it back and start with a fresh one if one comes soon.
        if !pacer.started() && stale.is_none() && clock_now() - frame.time > 2_500_000 {
            match converter.convert(&frame.texture, (frame.content_width, frame.content_height)) {
                Ok(t) => stale = Some(t),
                Err(e) => break Err(e),
            }
            continue;
        }
        // The encoder is behind: skip this frame without converting it (a convert
        // now could overwrite a pooled texture a queued frame still points at).
        // Its slot stays open and is filled with a repeat once there's room.
        if busy() {
            // Only a frame that would have had a slot counts as dropped.
            if pacer.wants(frame.time) {
                shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
            continue;
        }
        // Times are on the capture clock (QPC), the same one audio uses, so
        // tracks line up at save; slots keep the frame rate exactly constant.
        let Some(slots) = pacer.frame(frame.time, clock_now()) else {
            continue; // this slot already has a frame
        };
        if let (Some(tex), true) = (&last, constant) {
            if let Err(e) = slots.repeat.iter().try_for_each(|&t| send(tex, t, true)) {
                break Err(e);
            }
        }
        // Smooth timing: the frame keeps the moment it was shown (the very first
        // one uses the pacer's time, which re-stamps a stale first frame).
        let at = if constant || last.is_none() { slots.at } else { frame.time.max(last_time + 1) };
        let texture =
            match converter.convert(&frame.texture, (frame.content_width, frame.content_height)) {
                Ok(t) => t,
                Err(e) => break Err(e),
            };
        drop(frame); // hand the buffer back to the compositor right away
        if let Err(e) = send(&texture, at, false) {
            break Err(e);
        }
        last_time = at;
        last = Some(texture);
    };
    drop(tx);
    let deadline = Instant::now() + ENCODER_EXIT_WAIT;
    while !encoder_thread.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !encoder_thread.is_finished() {
        // Leave a hung encoder thread behind rather than freeze the tray on it.
        return Err(Error::new(E_FAIL, "the GPU encoder stopped responding"));
    }
    let encoded = encoder_thread
        .join()
        .unwrap_or_else(|_| Err(Error::new(E_FAIL, "encoder panicked")));
    // When the encoder fails, the capture side only sees "encoder stopped";
    // report the encoder's own error instead, so the log says why.
    match encoded {
        Err(e) => Err(e),
        Ok(()) => result,
    }
}
