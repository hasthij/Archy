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
    let result = archy_audio::listen_microphone(mic, archy_voice::SAMPLE_RATE, &stop, &mut |pcm| {
        samples.clear();
        samples.extend(pcm.iter().map(|&s| s as f32 / 32768.0));
        if let Some(i) = spotter.accept(&samples) {
            post(window, message, i, 0);
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

// ---- Calibration (`archy.exe --voice-calibrate <mic|-> <phrase>`) ----
//
// Records the user saying their phrase after each prompt, then talking
// normally, and finds the detector settings that catch them best without
// firing on normal talk. Progress goes to stdout as JSON lines for the Archy
// window: {"step":"ready"}, {"step":"say","n":1,"of":5}, {"step":"talk","secs":15},
// {"step":"analyzing"}, then {"step":"done",...} or {"step":"error","message":...}.

const PROMPTS: usize = 5;
const FIRST_PROMPT: f32 = 2.0;
const PROMPT_EVERY: f32 = 4.0;
const TALK_SECS: f32 = 15.0;

fn emit(v: serde_json::Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

pub fn calibrate_main(args: &[String]) {
    let marker = archy_core::settings::calibration_marker();
    let _ = std::fs::write(&marker, b"");
    let result = calibrate(args);
    let _ = std::fs::remove_file(&marker);
    match result {
        Ok(c) => emit(serde_json::json!({
            "step": "done",
            "hits": c.hits, "prompts": c.prompts,
            "score": c.tuning.score, "paths": c.tuning.paths, "threshold": c.tuning.threshold,
            "learned": c.learned,
        })),
        Err(e) => emit(serde_json::json!({ "step": "error", "message": e })),
    }
}

fn calibrate(args: &[String]) -> Result<archy_voice::Calibration, String> {
    use serde_json::json;
    let (Some(mic), Some(phrase)) = (args.first(), args.get(1)) else {
        return Err("bad arguments".into());
    };
    let mic = (mic != "-").then(|| mic.clone());
    let dir = archy_voice::find_dir().ok_or("the voice files are missing (reinstall Archy to get them)")?;
    normal_priority();

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<Vec<i16>>();
    let flag = stop.clone();
    let recorder = std::thread::spawn(move || {
        archy_audio::listen_microphone(mic.as_deref(), archy_voice::SAMPLE_RATE, &flag, &mut |pcm| {
            let _ = tx.send(pcm.to_vec());
        })
        .map_err(|e| e.message())
    });
    // The clock starts with the first audio, so prompts line up with the recording.
    let first = rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| "couldn't hear anything from your microphone")?;
    let start = std::time::Instant::now();
    let mut audio: Vec<i16> = first;
    emit(json!({ "step": "ready" }));
    let total = FIRST_PROMPT + PROMPT_EVERY * PROMPTS as f32 + TALK_SECS;
    let mut prompts = Vec::new();
    let mut next = 0;
    let mut talk_sent = false;
    let talk_start = FIRST_PROMPT + PROMPT_EVERY * PROMPTS as f32;
    loop {
        let now = start.elapsed().as_secs_f32();
        if now >= total {
            break;
        }
        if next < PROMPTS && now >= FIRST_PROMPT + PROMPT_EVERY * next as f32 {
            // Prompt time on the recording's own clock (samples received so far).
            prompts.push(audio.len() as f32 / archy_voice::SAMPLE_RATE as f32);
            next += 1;
            emit(json!({ "step": "say", "n": next, "of": PROMPTS }));
        }
        if !talk_sent && now >= talk_start {
            talk_sent = true;
            emit(json!({ "step": "talk", "secs": TALK_SECS }));
        }
        // Wait for the next buffer (or 20 ms), then take whatever else is queued.
        // (Draining with a timeout never ended: a mic delivers every ~10 ms.)
        if let Ok(pcm) = rx.recv_timeout(std::time::Duration::from_millis(20)) {
            audio.extend(pcm);
        }
        while let Ok(pcm) = rx.try_recv() {
            audio.extend(pcm);
        }
    }
    stop.store(true, Ordering::Release);
    recorder.join().map_err(|_| "the microphone stopped")??;
    while let Ok(pcm) = rx.try_recv() {
        audio.extend(pcm);
    }
    emit(json!({ "step": "analyzing" }));
    let audio: Vec<f32> = audio.iter().map(|&s| s as f32 / 32768.0).collect();
    let peak = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
    if peak < 0.004 {
        return Err("your microphone was almost silent. Check the microphone picked above, or turn up its input level.".into());
    }
    let talk_from = prompts.last().copied().unwrap_or(0.0) + PROMPT_EVERY;
    let talk = (talk_from, audio.len() as f32 / archy_voice::SAMPLE_RATE as f32);
    archy_voice::calibrate(&dir, phrase, &audio, &prompts, PROMPT_EVERY, talk)
}
