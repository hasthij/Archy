//! Voice clipping: "Archy, clip that" saves a clip, no key needed.
//!
//! A small on-device keyword spotter (archy-voice) listens to the same
//! microphone Archy records, for only the user's phrases. Nothing leaves the PC.
//! A heard phrase is posted to the tray window as a message.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW};

/// WPARAM posted when the helper couldn't start listening; LPARAM is one of the
/// `FAIL_*` codes below.
pub const VOICE_ERROR: usize = usize::MAX;
const FAIL_FILES: isize = 1;
const FAIL_ENGINE: isize = 2;
const FAIL_MIC: isize = 3;

/// The voice helper process. The keyword engine (onnxruntime) aborts the whole
/// process on an internal error, so it runs apart from the recorder: if it ever
/// dies, recording and hotkeys carry on.
pub struct VoiceListener {
    child: Option<std::process::Child>,
}

/// What the helper listens for; a change restarts it.
#[derive(Clone, PartialEq, Debug)]
pub struct VoiceConfig {
    pub phrases: Vec<String>,
    /// Microphone device id; empty = Windows' default.
    pub mic: String,
    pub tuning: archy_voice::Tuning,
    /// Learned ways the user says phrase `i` (from calibration).
    pub learned: Vec<(usize, String)>,
}

/// What to listen for, from the settings (`None` when there are no phrases).
/// Shared by the tray's listener and the voice test, so the test hears exactly
/// what the tray would.
pub fn config(s: &archy_core::settings::Settings) -> Option<VoiceConfig> {
    let phrases: Vec<String> = s
        .voice_phrases
        .iter()
        .map(|p| p.text.clone())
        .filter(|t| !t.trim().is_empty())
        .collect();
    if phrases.is_empty() {
        return None;
    }
    let base = match (s.voice_sensitivity, &s.voice_tuning) {
        (0, Some(t)) => archy_voice::Tuning { score: t.score, paths: t.paths, threshold: t.threshold },
        (v, _) => archy_voice::Tuning::preset(v),
    };
    Some(VoiceConfig {
        phrases,
        mic: s.mic_device.clone(),
        tuning: base.adjusted(s.voice_fine_tune),
        // Learned takes belong to the phrase that was calibrated, if it's still there.
        learned: match &s.voice_tuning {
            Some(t) => s
                .voice_phrases
                .iter()
                .position(|p| p.text == t.phrase)
                .map(|i| t.learned.iter().map(|l| (i, l.clone())).collect())
                .unwrap_or_default(),
            None => Vec::new(),
        },
    })
}

impl VoiceListener {
    /// Starts the helper; when phrase `i` is heard it posts `message` with
    /// WPARAM = i to `window` (or WPARAM = VOICE_ERROR on failure).
    pub fn start(config: &VoiceConfig, window: isize, message: u32) -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        let mic = if config.mic.is_empty() { "-" } else { &config.mic };
        let child = std::process::Command::new(exe)
            .arg("--voice-host")
            .arg(window.to_string())
            .arg(message.to_string())
            .arg(config.tuning.score.to_string())
            .arg(config.tuning.paths.to_string())
            .arg(config.tuning.threshold.to_string())
            .arg(mic)
            .arg(config.phrases.join("|"))
            .arg(
                config
                    .learned
                    .iter()
                    .map(|(i, t)| format!("{i}:{t}"))
                    .collect::<Vec<_>>()
                    .join("|"),
            )
            .spawn()?;
        Ok(Self { child: Some(child) })
    }
}

impl VoiceListener {
    /// Whether the helper has stopped (mic unplugged or changed, engine crash).
    pub fn exited(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => !matches!(c.try_wait(), Ok(None)),
            None => true,
        }
    }
}

impl Drop for VoiceListener {
    /// Ends the helper at once, without waiting on it.
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }
}

fn post(window: isize, message: u32, wparam: usize, lparam: isize) {
    unsafe {
        let _ = PostMessageW(Some(HWND(window as *mut _)), message, WPARAM(wparam), LPARAM(lparam));
    }
}

/// Entry point of the helper process
/// (`archy.exe --voice-host <hwnd> <msg> <score> <paths> <threshold> <mic|-> <a|b|c> [<i:tokens|...>]`).
pub fn host_main(args: &[String], log: fn(&str)) {
    let (Some(window), Some(message), Some(score), Some(paths), Some(threshold), Some(mic), Some(phrases)) = (
        args.first().and_then(|a| a.parse::<isize>().ok()),
        args.get(1).and_then(|a| a.parse::<u32>().ok()),
        args.get(2).and_then(|a| a.parse::<f32>().ok()),
        args.get(3).and_then(|a| a.parse::<u32>().ok()),
        args.get(4).and_then(|a| a.parse::<f32>().ok()),
        args.get(5),
        args.get(6),
    ) else {
        return;
    };
    let tuning = archy_voice::Tuning { score, paths, threshold };
    let learned: Vec<(usize, String)> = args
        .get(7)
        .map(|a| {
            a.split('|')
                .filter_map(|e| e.split_once(':'))
                .filter_map(|(i, t)| Some((i.parse().ok()?, t.to_string())))
                .collect()
        })
        .unwrap_or_default();
    normal_priority();
    let phrases: Vec<String> = phrases.split('|').map(str::to_string).collect();
    let mic = (mic != "-").then_some(mic.as_str());

    let Some(dir) = archy_voice::find_dir() else {
        log("voice: the voice files are missing (reinstall Archy to get them)");
        post(window, message, VOICE_ERROR, FAIL_FILES);
        return;
    };
    let mut spotter = match archy_voice::Spotter::new(&dir, &phrases, &learned, tuning) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("voice: {e}"));
            post(window, message, VOICE_ERROR, FAIL_ENGINE);
            return;
        }
    };

    // Stop when the tray window goes away (Archy quit or crashed).
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !unsafe { IsWindow(Some(HWND(window as *mut _))) }.as_bool() {
            flag.store(true, Ordering::Release);
            return;
        }
    });

    log(&format!("voice: listening for {phrases:?}"));
    let mut samples: Vec<f32> = Vec::new();
    let mut gate = SpeechGate::new(archy_voice::SAMPLE_RATE as usize, std::env::var_os("ARCHY_VOICE_NOGATE").is_none());
    let result = archy_audio::listen_microphone(mic, archy_voice::SAMPLE_RATE, &stop, &mut |pcm| {
        samples.clear();
        samples.extend(pcm.iter().map(|&s| s as f32 / 32768.0));
        // Speech recognition only runs around sound (it ran on every 10 ms of a
        // silent mic, all game long).
        for chunk in gate.feed(&samples) {
            if let Some(i) = spotter.accept(&chunk) {
                post(window, message, i, 0);
            }
        }
    });
    if let Err(e) = result {
        log(&format!("voice: microphone: {}", e.message()));
        post(window, message, VOICE_ERROR, FAIL_MIC);
    }
}

/// The helper inherits the recorder's below-normal priority; at that priority a
/// busy game could starve it and phrases would be heard late. It stays on the
/// recorder's efficiency cores.
fn normal_priority() {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, NORMAL_PRIORITY_CLASS};
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS);
    }
}

/// Readable reason for a failure the helper reported.
pub fn explain_code(code: isize) -> String {
    match code {
        FAIL_FILES => "The voice files are missing. Reinstall Archy to download them.".into(),
        FAIL_ENGINE => "The voice engine couldn't start (see archy.log).".into(),
        FAIL_MIC => "Archy couldn't open your microphone. Check Settings > Privacy & security > Microphone > Let desktop apps access your microphone.".into(),
        _ => "Voice clipping couldn't start.".into(),
    }
}

// ---- Test (`archy.exe --voice-test`) ----
//
// Listens with the saved settings and reports every phrase it hears, so the user
// can read a practice paragraph and see what Archy catches and what sets it off
// by accident. Runs until the Archy window ends it. JSON lines: {"step":"ready"},
// {"step":"heard","phrase":i}, {"step":"retuned"} (settings changed, e.g. the
// sensitivity slider; applies within a second), {"step":"error","message":...}.
// The calibration marker is kept fresh the whole time so the tray doesn't save
// a clip for every practice phrase.

pub fn test_main() {
    let marker = archy_core::settings::calibration_marker();
    if let Err(e) = voice_test(&marker) {
        emit(serde_json::json!({ "step": "error", "message": e }));
    }
    let _ = std::fs::remove_file(&marker);
}

fn voice_test(marker: &std::path::Path) -> Result<(), String> {
    use archy_core::settings::Settings;
    use serde_json::json;
    let dir = archy_voice::find_dir().ok_or("the voice files are missing (reinstall Archy to get them)")?;
    normal_priority();
    let spot = |c: &VoiceConfig| archy_voice::Spotter::new(&dir, &c.phrases, &c.learned, c.tuning);
    let mut current = config(&Settings::load().0).ok_or("add a phrase first")?;
    let mut spotter = spot(&current)?;
    let mic = (!current.mic.is_empty()).then(|| current.mic.clone());
    let _ = std::fs::write(marker, b"");
    let mut modified = Settings::modified();
    let mut checked = std::time::Instant::now();
    // Normally the window ends this process; if the window itself is gone (its
    // end of the output pipe closed), stop, or the marker would keep voice
    // clipping silenced.
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let mut samples: Vec<f32> = Vec::new();
    emit(json!({ "step": "ready" }));
    archy_audio::listen_microphone(mic.as_deref(), archy_voice::SAMPLE_RATE, &stop, &mut |pcm| {
        if checked.elapsed().as_secs() >= 1 {
            checked = std::time::Instant::now();
            if !emit(json!({ "step": "listening" })) {
                flag.store(true, Ordering::Release);
                return;
            }
            let _ = std::fs::write(marker, b"");
            let now = Settings::modified();
            if now != modified {
                modified = now;
                // Only a change to what's listened for counts (saving a test
                // result also touches the file).
                if let Some(c) = config(&Settings::load().0).filter(|c| *c != current) {
                    if let Ok(s) = spot(&c) {
                        spotter = s;
                        current = c;
                        emit(json!({ "step": "retuned" }));
                    }
                }
            }
        }
        samples.clear();
        samples.extend(pcm.iter().map(|&s| s as f32 / 32768.0));
        if let Some(i) = spotter.accept(&samples) {
            emit(json!({ "step": "heard", "phrase": i }));
        }
    })
    .map_err(|e| format!("couldn't open your microphone: {}", e.message()))
}

// ---- Calibration (`archy.exe --voice-calibrate <mic|-> <phrase>`) ----
//
// The user paces it: the Archy window sends commands on stdin and this process
// records exactly what they say, so nobody races a timer.
//   "take"  start recording the phrase      "talk" start recording a look-alike sentence
//   "stop"  end the current recording       "undo" drop the last take
//   "analyze"  find the best setting (then the process ends)
// Output, as JSON lines: {"step":"ready"}, {"step":"level","v":0..1} (~10 a second,
// for the mic meter), {"step":"take","n":k,"heard":bool} (each take checked at
// once with the Balanced setting), {"step":"talked","secs":s}, {"step":"undone","n":k},
// {"step":"analyzing"}, then {"step":"done",...} or {"step":"error","message":...}.

use archy_voice::{CALIBRATION_LEAD as LEAD, CALIBRATION_TAIL as TAIL};
/// A take longer than this is cut (a held button, not a phrase).
const MAX_TAKE: f32 = 6.0;

/// Writes one progress line; false once nobody is reading any more.
fn emit(v: serde_json::Value) -> bool {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{v}").and_then(|_| out.flush()).is_ok()
}

pub fn calibrate_main(args: &[String]) {
    let marker = archy_core::settings::calibration_marker();
    let _ = std::fs::write(&marker, b"");
    let result = calibrate(args, &marker);
    let _ = std::fs::remove_file(&marker);
    let _ = match result {
        Ok(c) => emit(serde_json::json!({
            "step": "done",
            "hits": c.hits, "prompts": c.prompts,
            "score": c.tuning.score, "paths": c.tuning.paths, "threshold": c.tuning.threshold,
            "learned": c.learned,
        })),
        Err(e) => emit(serde_json::json!({ "step": "error", "message": e })),
    };
}

enum Recording {
    Take,
    Talk,
}

fn calibrate(args: &[String], marker: &std::path::Path) -> Result<archy_voice::Calibration, String> {
    use serde_json::json;
    let (Some(mic), Some(phrase)) = (args.first(), args.get(1)) else {
        return Err("bad arguments".into());
    };
    let mic = (mic != "-").then(|| mic.clone());
    let dir = archy_voice::find_dir().ok_or("the voice files are missing (reinstall Archy to get them)")?;
    normal_priority();
    let rate = archy_voice::SAMPLE_RATE as f32;

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<Vec<i16>>();
    let flag = stop.clone();
    let recorder = std::thread::spawn(move || {
        archy_audio::listen_microphone(mic.as_deref(), archy_voice::SAMPLE_RATE, &flag, &mut |pcm| {
            let _ = tx.send(pcm.to_vec());
        })
        .map_err(|e| e.message())
    });
    // Commands from the window, one per line.
    let (cmd_tx, commands) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if cmd_tx.send(line.trim().to_string()).is_err() {
                break;
            }
        }
        let _ = cmd_tx.send("quit".into()); // the window went away
    });
    rx.recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| "couldn't hear anything from your microphone")?;
    emit(json!({ "step": "ready" }));

    // Takes are checked one at a time, in order, on one thread.
    let (check_tx, check_rx) = std::sync::mpsc::channel::<(usize, Vec<f32>)>();
    {
        let (dir, phrase) = (dir.clone(), phrase.clone());
        std::thread::spawn(move || {
            for (n, audio) in check_rx {
                let heard = archy_voice::heard_in(&dir, &phrase, &audio, archy_voice::Tuning::preset(50)).unwrap_or(false);
                emit(serde_json::json!({ "step": "checked", "n": n, "heard": heard }));
            }
        });
    }
    let mut takes: Vec<Vec<f32>> = Vec::new();
    let mut talk: Vec<f32> = Vec::new();
    let mut current: Option<(Recording, Vec<f32>)> = None;
    let mut level_at = std::time::Instant::now();
    let mut peak = 0f32;
    let to_f32 = |pcm: &[i16]| pcm.iter().map(|&s| s as f32 / 32768.0).collect::<Vec<f32>>();
    let result = loop {
        let pcm = match rx.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok(pcm) => Some(pcm),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            // The microphone stopped (unplugged, taken by another app, blocked):
            // say so instead of spinning here at full speed.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err("the microphone stopped sending sound (unplugged, or another app took it). Check it and try again.".to_string());
            }
        };
        if let Some(pcm) = pcm {
            let samples = to_f32(&pcm);
            peak = samples.iter().fold(peak, |m, s| m.max(s.abs()));
            if let Some((_, buf)) = current.as_mut() {
                if (buf.len() as f32) < MAX_TAKE * rate {
                    buf.extend(samples);
                }
            }
        }
        if level_at.elapsed().as_millis() >= 100 {
            level_at = std::time::Instant::now();
            // Also keeps the marker fresh, so the tray ignores practice phrases.
            let _ = std::fs::write(marker, b"");
            emit(json!({ "step": "level", "v": peak.min(1.0) }));
            peak = 0.0;
        }
        let Ok(cmd) = commands.try_recv() else { continue };
        match cmd.as_str() {
            "take" => current = Some((Recording::Take, Vec::new())),
            "talk" => current = Some((Recording::Talk, Vec::new())),
            "stop" => match current.take() {
                Some((Recording::Take, buf)) => {
                    // Shown at once; whether it was heard follows when checked.
                    let audio = padded(&buf, rate);
                    takes.push(buf);
                    let n = takes.len();
                    emit(json!({ "step": "take", "n": n, "heard": null }));
                    let _ = check_tx.send((n, audio));
                }
                Some((Recording::Talk, buf)) => {
                    talk.extend(std::iter::repeat_n(0.0, (0.3 * rate) as usize));
                    talk.extend(buf);
                    emit(json!({ "step": "talked", "secs": talk.len() as f32 / rate }));
                }
                None => {}
            },
            "undo" => {
                takes.pop();
                emit(json!({ "step": "undone", "n": takes.len() }));
            }
            "analyze" => break Ok(()),
            "quit" => break Err("stopped".to_string()),
            _ => {}
        }
    };
    stop.store(true, Ordering::Release);
    recorder.join().map_err(|_| "the microphone stopped")??;
    result?;
    if takes.is_empty() {
        return Err("record the phrase at least once first".into());
    }
    let loudest = takes.iter().flatten().fold(0f32, |m, s| m.max(s.abs()));
    if loudest < 0.004 {
        return Err("your microphone was almost silent. Check the microphone picked above, or turn up its input level.".into());
    }
    drop(check_tx); // no more takes: the checker finishes what's queued
    emit(json!({ "step": "analyzing" }));
    archy_voice::calibrate_takes(&dir, phrase, &takes, &talk)
}

/// A take with silence around it, so the detector hears it start and finish.
fn padded(take: &[f32], rate: f32) -> Vec<f32> {
    let mut v = vec![0.0; (LEAD * rate) as usize];
    v.extend_from_slice(take);
    v.extend(std::iter::repeat_n(0.0, (TAIL * rate) as usize));
    v
}

/// Passes mic audio on only around sound: while the level is clearly above the
/// room's noise floor, plus a pre-roll (so the start of the phrase is never cut)
/// and a hangover (so its end is heard). Silence costs nothing.
pub struct SpeechGate {
    on: bool,
    preroll: std::collections::VecDeque<f32>,
    preroll_max: usize,
    /// Samples left to pass after the sound stopped.
    hangover: usize,
    hangover_max: usize,
    /// Slowly tracked background level (RMS).
    floor: f32,
}

impl SpeechGate {
    pub fn new(rate: usize, on: bool) -> Self {
        Self {
            on,
            preroll: std::collections::VecDeque::new(),
            preroll_max: rate / 2,
            hangover: 0,
            hangover_max: rate,
            floor: 0.002,
        }
    }

    /// The audio to pass to the recognizer for this block (none in silence).
    pub fn feed(&mut self, block: &[f32]) -> Vec<Vec<f32>> {
        if !self.on {
            return vec![block.to_vec()];
        }
        let rms = (block.iter().map(|s| s * s).sum::<f32>() / block.len().max(1) as f32).sqrt();
        // The floor falls fast and rises slowly, so speech never raises it much.
        self.floor = if rms < self.floor { self.floor * 0.9 + rms * 0.1 } else { self.floor * 0.999 + rms * 0.001 };
        self.floor = self.floor.clamp(0.0003, 0.05);
        let loud = rms > (self.floor * 2.0).max(0.001); // about +6 dB over the room
        let mut out = Vec::new();
        if loud {
            if self.hangover == 0 && !self.preroll.is_empty() {
                out.push(self.preroll.drain(..).collect());
            }
            self.hangover = self.hangover_max;
            out.push(block.to_vec());
        } else if self.hangover > 0 {
            self.hangover = self.hangover.saturating_sub(block.len());
            out.push(block.to_vec());
        } else {
            self.preroll.extend(block.iter().copied());
            while self.preroll.len() > self.preroll_max {
                self.preroll.pop_front();
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::SpeechGate;

    #[test]
    fn silence_is_skipped_and_speech_keeps_its_start() {
        let mut g = SpeechGate::new(16_000, true);
        let quiet = vec![0.0005f32; 160];
        for _ in 0..200 {
            assert!(g.feed(&quiet).is_empty(), "silence reaches the recognizer");
        }
        let speech = vec![0.1f32; 160];
        let out = g.feed(&speech);
        // Pre-roll (the half second before) comes first, then the block itself.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 8_000);
        // The second after the sound still passes.
        assert!(!g.feed(&quiet).is_empty());
    }

    #[test]
    fn off_passes_everything() {
        let mut g = SpeechGate::new(16_000, false);
        assert_eq!(g.feed(&[0.0; 160]).len(), 1);
    }
}
