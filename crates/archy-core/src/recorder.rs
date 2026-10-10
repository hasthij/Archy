//! The always-on recorder: capture → convert → encode into the replay buffer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use archy_audio::{AacInfo, AudioCapture, Source};
use archy_capture::{Capture, Target};
use archy_encode::{Backend, Config, Encoder, Input};
use archy_gpu::convert::{fit_size, AnyConverter, ConvertMode};
use archy_gpu::device::{Gpu, RECORDER_GPU_PRIORITY};
use archy_gpu::probe::Codec;
use archy_gpu::SendPtr;
use archy_mux::{AudioTrack, Mp4Writer};
use windows::core::{Error, Result};
use windows::Win32::Foundation::{E_FAIL, LUID};

use crate::cadence::Cadence;
use crate::ring::{ReplayBuffer, StoredPacket};

/// NV12 textures in rotation: queue depth + frames the encoder may still hold + one being written.
const NV12_POOL: usize = 8;
/// Frames allowed to wait for the encoder. Beyond this a frame is dropped, never queued.
const QUEUE_DEPTH: usize = 3;

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
    /// The GPU recording runs on.
    pub gpu: String,
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
}

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
        });
        let (ready_tx, ready_rx) = sync_channel::<Result<RecorderInfo>>(1);
        let thread_shared = shared.clone();
        let capture_thread = thread::Builder::new()
            .name("archy-capture".into())
            .spawn(move || {
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
        let info = ready_rx
            .recv()
            .map_err(|_| Error::new(E_FAIL, "recorder thread exited during start-up"))??;
        // Audio never blocks video: a source that can't start is reported and skipped.
        let mut audio = Vec::new();
        let mut audio_report = Vec::new();
        for src in audio_sources {
            let ring = Arc::new(Mutex::new(ReplayBuffer::new(buffer_seconds)));
            let sink = ring.clone();
            match AudioCapture::start(src.source.clone(), audio_kbps, src.volumes.clone(), move |p| {
                if let Ok(mut r) = sink.lock() {
                    r.push(StoredPacket {
                        data: Arc::from(p.data),
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
        std::fs::rename(&partial, path).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
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
                Ok(e) => return Ok((e, codec)),
                Err(e) => last = e,
            }
        }
    }
    Err(last)
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
    let (_gpu, capture, mut converter, mut encoder, codec, width, height) = loop {
        let Some(&adapter) = adapters.get(index) else {
            return Err(last_error);
        };
        index += 1;
        let attempt = (|| -> Result<_> {
            let gpu = Gpu::new(adapter, RECORDER_GPU_PRIORITY)?;
            // Half the interval: the compositor wakes us at most twice per wanted
            // frame, and frames arriving a hair early are never pushed to the next refresh.
            let capture = Capture::start(&gpu.device, cfg.target, interval / 2)?;
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
        gpu: archy_gpu::device::adapter_name(_gpu.luid),
    };

    let (tx, rx) = sync_channel::<SendPtr<Input>>(QUEUE_DEPTH);
    let enc_shared = shared.clone();
    let encoder_thread = thread::Builder::new()
        .name("archy-encode".into())
        .spawn(move || -> Result<()> {
            crate::init_thread()?;
            // Latest codec header (SPS/PPS, VPS/SPS/PPS) seen in the stream. Some
            // drivers send it only once, at the start; a clip starting at a later
            // keyframe then has no header and can't be written. Such keyframes get
            // the cached header put in front.
            let mut header: Option<Vec<u8>> = None;
            let stats = encoder.run(rx, |p| {
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
                if let Ok(mut ring) = enc_shared.ring.lock() {
                    ring.push(packet);
                }
                Ok(())
            });
            if let (Ok(s), Ok(mut slot)) = (&stats, enc_shared.encoder_stats.lock()) {
                *slot = Some(s.clone());
            }
            stats.map(|_| ())
        })
        .map_err(|e| Error::new(E_FAIL, e.to_string()))?;
    let _ = ready.send(Ok(info));

    let mut cadence = Cadence::new(cfg.fps);
    let result = loop {
        if shared.stop.load(Ordering::Acquire) || capture.is_closed() {
            break Ok(());
        }
        // Sleeps until the compositor has a new frame; a static screen costs nothing.
        let frame = match capture.next(Duration::from_millis(100)) {
            Ok(Some(f)) => f,
            Ok(None) => continue,
            Err(e) => break Err(e),
        };
        if !cadence.accept(frame.time) {
            continue;
        }
        // Absolute QPC time: the same clock audio uses, so tracks line up at save.
        let time = frame.time;
        let texture =
            match converter.convert(&frame.texture, (frame.content_width, frame.content_height)) {
                Ok(t) => t,
                Err(e) => break Err(e),
            };
        drop(frame); // hand the buffer back to the compositor right away
        match tx.try_send(SendPtr(Input { texture, time })) {
            Ok(()) => {
                shared.frames.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => break Err(Error::new(E_FAIL, "encoder stopped")),
        }
    };
    drop(tx);
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
