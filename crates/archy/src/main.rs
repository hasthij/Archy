//! Archy: a local-first game clipper that lives in the tray.
//!
//! While a game is in the foreground it keeps the last N seconds in RAM; the
//! hotkey (or, later, a voice phrase) saves them as an MP4 instantly. Nothing
//! is recorded when no game is running, and nothing ever leaves the PC.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clip;
mod ptt;
mod hotkey;
mod startup;
mod voice;
mod watch;

use std::cell::RefCell;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use archy_audio::Source;
use archy_core::recorder::AudioSource;
use archy_core::settings::{self, Settings};
use archy_core::status::Status;
use archy_core::{presets, Codec, ConvertMode, Recorder, RecorderConfig, RecorderStatus, Target};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO,
    NIIF_NOSOUND, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
/// A voice phrase was heard; WPARAM = phrase index.
const WM_VOICE: u32 = WM_APP + 2;
/// The worker thread finished starting a recording (see `App::start`).
const WM_STARTED: u32 = WM_APP + 3;
const TIMER_WATCH: usize = 1;
const HK_SAVE: i32 = 1;
const HK_SAVE_LONG: i32 = 2;
const HK_SCREENSHOT: i32 = 4;
const HK_SESSION: i32 = 5;
/// Extra clip hotkeys use ids HK_EXTRA.. (one per entry in `extra_hotkeys`).
const HK_EXTRA: i32 = 100;
const HK_PAUSE: i32 = 3;
const MENU_SAVE: usize = 10;
const MENU_PAUSE: usize = 11;
const MENU_FOLDER: usize = 12;
const MENU_SETTINGS: usize = 13;
const MENU_OPEN: usize = 14;
const MENU_QUIT: usize = 15;

struct Session {
    recorder: Arc<Recorder>,
    hwnd: HWND,
    game: String,
    /// Its program file, to tell whether it's the game the user picked.
    exe: String,
    /// Recording the game's window (not a whole screen).
    window_target: bool,
}

/// A recording being started on a worker thread.
struct Starting {
    rx: std::sync::mpsc::Receiver<Started>,
    hwnd: isize,
    game: String,
    exe: String,
    window_target: bool,
}

/// What the start worker sends back.
struct Started {
    result: Result<Recorder, String>,
    /// Fallbacks that were needed, for the log.
    notes: Vec<String>,
    /// The frame rate it runs at (lower than the setting after a fallback).
    fps: u32,
}

/// A recording that was replaced (settings changed, the encoder failed, the game
/// was minimized). Stopped, but its buffer is kept for a while so a clip saved
/// right after a restart still has everything from before it.
struct Previous {
    recorder: Arc<Recorder>,
    game: String,
    ended: std::time::Instant,
}

struct App {
    window: HWND,
    settings: Settings,
    session: Option<Session>,
    /// A game whose GPU couldn't keep up with the chosen frame rate, and the
    /// rate it's recorded at instead (until the settings change).
    fps_cap: Option<(String, u32)>,
    /// Frames sent / skipped at the last check, and how many checks in a row
    /// the encoder fell behind.
    keep_up: (u64, u64, u32),
    paused: bool,
    saving: Arc<AtomicBool>,
    settings_modified: Option<std::time::SystemTime>,
    voice: Option<voice::VoiceListener>,
    /// What the running listener was started with (restart when it changes).
    voice_config: Option<voice::VoiceConfig>,
    /// Whether the tray currently shows the recording-dot icon.
    dot_shown: bool,
    /// A window that couldn't be recorded even as a screen capture; skipped
    /// until it closes, so one bad window doesn't stop recording other games.
    failed_window: Option<isize>,
    volumes: TrackVolumes,
    /// When the voice helper was last (re)started, to space out restarts.
    voice_started: Option<std::time::Instant>,
    /// Seconds of a clip requested while another was saving (0 = none).
    queued: Arc<std::sync::atomic::AtomicU32>,
    /// When "Voice clipping is off" was last shown, so a broken mic doesn't spam.
    voice_warned: std::cell::Cell<Option<std::time::Instant>>,
    /// A recording being started on the worker thread.
    starting: Option<Starting>,
    /// Stop the recording being started as soon as it's up (settings changed or
    /// paused meanwhile).
    stop_after_start: bool,
    previous: Option<Previous>,
    /// Encoders that failed twice (`RecorderInfo::encoder_key`); skipped until restart.
    avoid: Vec<String>,
    /// When the last encoder was banned; bans are forgotten 15 minutes later.
    avoid_since: Option<std::time::Instant>,
    /// Each encoder failure (avoided from the second one).
    encoder_strikes: Vec<String>,
    /// Recent failures: (exe, when).
    failures: Vec<(String, std::time::Instant)>,
    /// A game that kept failing, and when to try it again.
    blocked: Option<(String, std::time::Instant)>,
    /// Watch ticks the recorded window has been minimized.
    minimized_ticks: u32,
    ticks: u32,
    hdr_warned: bool,
    /// "hotkey" session mode: the user has turned session recording on.
    session_wanted: bool,
    /// Push-to-talk, while it's on.
    ptt: Option<ptt::PushToTalk>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// One dated line in archy.log; past 1 MB the log moves to archy.old.log (it
/// used to grow forever, with times but no dates).
fn log(msg: &str) {
    let dir = settings::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("archy.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::rename(&path, dir.join("archy.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let _ = writeln!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {msg}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
        );
    }
}

/// Finishes a recorder's session file on another thread (never the tray's) and
/// logs it; a problem (full drive, write error) is shown once.
fn end_session_file(recorder: &Arc<Recorder>, window: isize) {
    if !recorder.session_active() {
        return;
    }
    let recorder = recorder.clone();
    std::thread::spawn(move || {
        if let Some(sum) = recorder.end_session() {
            log(&format!(
                "session: saved {} ({:.1} min, {:.0} MB, {} bookmarks){}",
                sum.path.display(),
                sum.seconds / 60.0,
                sum.bytes as f64 / 1e6,
                sum.bookmarks,
                sum.problem.as_ref().map(|p| format!("; stopped early: {p}")).unwrap_or_default()
            ));
            if let Some(p) = &sum.problem {
                notify(window, "Session recording stopped", &format!("{p}. What was recorded is saved."), true);
            }
        }
    });
}

/// Keeps the clips folder under the user's limit (GB, 0 = none) by moving the
/// oldest clips to the Recycle Bin; the newest clip is always kept.
fn trim_clips_folder(folder: &std::path::Path, limit_gb: u32) {
    if limit_gb == 0 {
        return;
    }
    let clips = archy_core::files::clips_oldest_first(folder);
    for p in archy_core::files::over_limit(&clips, limit_gb as u64 * 1_000_000_000, 1) {
        match archy_core::files::recycle(&p) {
            Ok(()) => log(&format!("clips folder over {limit_gb} GB: moved {} to the Recycle Bin", p.display())),
            Err(e) => log(&format!("couldn't trim {}: {e}", p.display())),
        }
    }
}

/// Warns (at most hourly) when the clips drive is nearly full.
fn warn_low_disk(window: isize, folder: &std::path::Path) {
    use std::sync::atomic::AtomicU64;
    static LAST: AtomicU64 = AtomicU64::new(0);
    let Some(free) = archy_core::files::free_bytes(folder) else { return };
    if free >= 2_000_000_000 {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now.saturating_sub(LAST.load(Ordering::Relaxed)) < 3600 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);
    notify(
        window,
        "Your clips drive is almost full",
        &format!("{:.1} GB left. Delete old clips, or set a size limit in Archy > Recording.", free as f64 / 1e9),
        true,
    );
}

fn wide_into<const N: usize>(dst: &mut [u16; N], text: &str) {
    let mut i = 0;
    for c in text.encode_utf16().take(N - 1) {
        dst[i] = c;
        i += 1;
    }
    dst[i] = 0;
}

fn tray_data(window: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        ..Default::default()
    }
}

fn set_tooltip(window: HWND, text: &str) {
    let mut nid = tray_data(window);
    nid.uFlags = NIF_TIP;
    wide_into(&mut nid.szTip, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// A Windows notification from the tray icon. Safe to call from any thread.
fn notify(window: isize, title: &str, text: &str, warning: bool) {
    let mut nid = tray_data(HWND(window as *mut _));
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = (if warning { NIIF_WARNING } else { NIIF_INFO }) | NIIF_NOSOUND;
    wide_into(&mut nid.szInfoTitle, title);
    wide_into(&mut nid.szInfo, text);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn open_path(path: &std::path::Path) {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0])
        .collect();
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Discord's free upload limit, with a margin for the MP4 container.
const DISCORD_LIMIT_BYTES: f64 = 10_000_000.0 * 0.92;

/// Live track volumes; the recorder reads them for every audio block.
#[derive(Default)]
struct TrackVolumes {
    everything: archy_audio::Volume,
    game: archy_audio::Volume,
    discord: archy_audio::Volume,
    mic: archy_audio::Volume,
}

impl TrackVolumes {
    fn update(&self, s: &Settings) {
        use std::sync::atomic::Ordering::Relaxed;
        let (everything, game, discord, mic) = s.audio_volumes();
        self.everything.store(everything, Relaxed);
        self.game.store(game, Relaxed);
        self.discord.store(discord, Relaxed);
        self.mic.store(mic, Relaxed);
    }

    fn of(&self, main: &archy_audio::Volume) -> archy_audio::Volumes {
        archy_audio::Volumes { main: main.clone(), mic: self.mic.clone() }
    }
}

fn recorder_config(s: &Settings, target: Target, game_pid: u32, vols: &TrackVolumes, avoid: &[String]) -> RecorderConfig {
    let mut audio = Vec::new();
    let (with_game, with_discord, with_mic) = s.audio_sources();
    {
        // Track 1 (always): everything you heard, and your mic, because it's what
        // single-track players (Discord, phones) play.
        let source = if s.mic_wanted() {
            Source::Mix {
                output: Some(s.output_device.clone()),
                mic: Some(s.mic_device.clone()),
            }
        } else {
            Source::System {
                device: Some(s.output_device.clone()),
            }
        };
        audio.push(AudioSource {
            name: "Everything".into(),
            source,
            volumes: vols.of(&vols.everything),
        });
    }
    if with_game && game_pid != 0 {
        audio.push(AudioSource {
            name: "Game".into(),
            source: Source::Process { pid: game_pid },
            volumes: vols.of(&vols.game),
        });
    }
    if with_discord {
        if let Some(pid) = archy_audio::find_root_process("Discord.exe") {
            audio.push(AudioSource {
                name: "Discord".into(),
                source: Source::Process { pid },
                volumes: vols.of(&vols.discord),
            });
        }
    }
    if with_mic {
        audio.push(AudioSource {
            name: "Microphone".into(),
            source: Source::Microphone {
                device: Some(s.mic_device.clone()),
            },
            volumes: vols.of(&vols.mic),
        });
    }

    let discord = s.quality.eq_ignore_ascii_case("discord");
    // Discord mode: lighter audio, and the video gets whatever is left of 10 MB.
    let audio_kbps = if discord { 96 } else { s.audio_kbps };
    let (height, target_bps) = if discord {
        let total = DISCORD_LIMIT_BYTES * 8.0 / s.clip_seconds.max(1) as f64;
        let video = (total - (audio.len() as u32 * audio_kbps * 1000) as f64).max(600_000.0);
        (if video >= 2_000_000.0 { 720 } else { 480 }, video as u32)
    } else {
        let p = presets::find(&s.quality).unwrap_or(&presets::PRESETS[2]);
        // Presets are sized for 60 fps. Above that the same bitrate is split over
        // more frames and fast motion goes soft (at 144, each frame got 2.4x less),
        // so it grows with the frame rate: less than linearly, as neighbouring
        // frames are more alike the faster they come. 144 fps: x1.69.
        let more_frames = (s.fps.max(60) as f64 / 60.0).powf(0.6);
        (p.height, (p.mbps as f64 * more_frames * 1_000_000.0) as u32)
    };
    let codecs = match s.codec.to_ascii_lowercase().as_str() {
        "hevc" | "h265" => vec![Codec::Hevc, Codec::H264],
        "av1" => vec![Codec::Av1, Codec::Hevc, Codec::H264],
        "auto" => vec![Codec::Av1, Codec::Hevc, Codec::H264],
        _ => vec![Codec::H264],
    };
    RecorderConfig {
        target,
        fps: s.fps,
        max_height: height,
        codecs,
        // Short spikes up to 2x keep fast motion clean; the average sets the size.
        max_bitrate: target_bps * 2,
        target_bitrate: Some(target_bps),
        quality: 70,
        buffer_seconds: s.buffer_seconds(),
        convert: ConvertMode::VideoEngine,
        gpu: s.encoder_gpu.clone(),
        audio,
        audio_kbps,
        avoid: avoid.to_vec(),
        constant_rate: s.frame_timing == "constant",
    }
}

impl App {
    fn register_hotkeys(&self) {
        let mut keys = vec![
            (HK_SAVE, &self.settings.hotkey_save),
            (HK_SAVE_LONG, &self.settings.hotkey_save_long),
            (HK_PAUSE, &self.settings.hotkey_pause),
            (HK_SCREENSHOT, &self.settings.hotkey_screenshot),
            (HK_SESSION, &self.settings.hotkey_session),
        ];
        // Every possible extra id is released first, so a removed one stops working.
        for i in 0..archy_core::settings::MAX_EXTRA_HOTKEYS as i32 {
            unsafe {
                let _ = UnregisterHotKey(Some(self.window), HK_EXTRA + i);
            }
        }
        for (i, h) in self.settings.extra_hotkeys.iter().enumerate() {
            keys.push((HK_EXTRA + i as i32, &h.key));
        }
        for (id, text) in keys {
            unsafe {
                let _ = UnregisterHotKey(Some(self.window), id);
            }
            if text.trim().is_empty() {
                continue; // hotkey turned off
            }
            let Some(hk) = hotkey::parse(text) else {
                notify(
                    self.window.0 as isize,
                    "Archy: hotkey not recognized",
                    &format!("\"{text}\" in settings.json"),
                    true,
                );
                continue;
            };
            if unsafe { RegisterHotKey(Some(self.window), id, hk.modifiers, hk.vk) }.is_err() {
                notify(
                    self.window.0 as isize,
                    "Archy: hotkey unavailable",
                    &format!("{text} is already used by another app. Pick another in settings."),
                    true,
                );
            }
        }
    }

    fn status_text(&self) -> String {
        if self.paused {
            return "Archy: paused".into();
        }
        match &self.session {
            Some(s) => {
                let i = &s.recorder.info;
                format!(
                    "Archy: recording {} ({} {}p{})",
                    s.game, i.codec, i.height, i.fps
                )
            }
            None => "Archy: waiting for a game".into(),
        }
    }

    /// Tray tooltip + status file for the Archy window.
    fn publish(&mut self) {
        set_tooltip(self.window, &self.status_text());
        let dot = self.settings.tray_recording_dot && self.session.is_some() && !self.paused;
        if dot != self.dot_shown {
            self.dot_shown = dot;
            let mut nid = tray_data(self.window);
            nid.uFlags = NIF_ICON;
            nid.hIcon = app_icon(dot);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            }
        }
        let (state, game, detail) = match (&self.session, self.paused) {
            (_, true) => ("paused", String::new(), String::new()),
            (Some(s), _) => {
                let i = &s.recorder.info;
                (
                    "recording",
                    s.game.clone(),
                    format!("{} {}p{} · {}", i.codec, i.height, i.fps, i.encoder),
                )
            }
            (None, _) => ("waiting", String::new(), String::new()),
        };
        Status {
            state: state.into(),
            game,
            detail,
            pid: std::process::id(),
        }
        .write();
    }

    /// Applies settings saved from the Archy window (or by hand) within one tick.
    fn pick_up_settings(&mut self) {
        let modified = Settings::modified();
        if modified == self.settings_modified {
            return;
        }
        self.settings_modified = modified;
        let (s, err) = Settings::load();
        if let Some(err) = err {
            notify(self.window.0 as isize, "Archy settings", &err, true);
            return;
        }
        let restart = recording_changed(&self.settings, &s);
        let s_fps_changed = s.fps != self.settings.fps;
        if s.start_with_windows != self.settings.start_with_windows {
            startup::set(s.start_with_windows);
        }
        self.settings = s;
        self.volumes.update(&self.settings);
        self.register_hotkeys();
        self.sync_session();
        self.sync_ptt();
        self.sync_voice();
        self.publish();
        if restart {
            // A new frame rate gets a fresh try; other changes keep the step-down.
            if s_fps_changed {
                self.fps_cap = None;
            }
            // A failed game gets a fresh try with new settings.
            self.blocked = None;
            self.failed_window = None;
            if self.starting.is_some() {
                self.stop_after_start = true;
            }
            self.stop_session("settings changed"); // restarts with the new settings on this tick
            self.paused = false;
        }
        log("settings applied");
    }

    /// Starts, restarts or stops the voice listener to match settings and pause state.
    fn sync_voice(&mut self) {
        let wanted = match voice::config(&self.settings) {
            Some(c) if self.settings.voice_enabled && !self.paused => c,
            _ => {
                self.voice = None;
                self.voice_config = None;
                return;
            }
        };
        if self.voice.is_some() && self.voice_config.as_ref() == Some(&wanted) {
            return;
        }
        self.voice = None; // stop the old listener before starting a new one
        self.voice_started = Some(std::time::Instant::now());
        match voice::VoiceListener::start(&wanted, self.window.0 as isize, WM_VOICE) {
            Ok(v) => self.voice = Some(v),
            Err(e) => log(&format!("voice: couldn't start the voice helper: {e}")),
        }
        self.voice_config = Some(wanted);
    }

    fn heard(&self, index: usize, code: isize) {
        if index == voice::VOICE_ERROR {
            let why = voice::explain_code(code);
            log(&format!("voice: stopped: {why}"));
            // The helper is restarted automatically (see `revive_voice`); warn at
            // most every 5 minutes so a missing mic doesn't spam notifications.
            let recent = self.voice_warned.get().is_some_and(|t| t.elapsed().as_secs() < 300);
            if !recent {
                self.voice_warned.set(Some(std::time::Instant::now()));
                notify(self.window.0 as isize, "Voice clipping paused", &format!("{why} Archy keeps retrying."), true);
            }
            return;
        }
        let Some(phrase) = self.settings.voice_phrases.get(index) else {
            return;
        };
        // Practice phrases during calibration aren't requests (a stale marker from
        // a crash is ignored after two minutes).
        let calibrating = std::fs::metadata(archy_core::settings::calibration_marker())
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() < 120));
        if calibrating {
            log(&format!("voice: heard \"{}\" during calibration (ignored)", phrase.text));
            return;
        }
        log(&format!("voice: heard \"{}\"", phrase.text));
        if self.settings.sound_on_voice {
            play(VOICE_WAV, self.settings.voice_sound_volume);
        }
        self.save(phrase.seconds);
    }

    /// Starts or stops recording to follow the game. Runs every 2 s.
    /// Restarts the voice helper if it stopped (mic unplugged/changed, Discord
    /// switching devices, engine crash). It used to stay dead until Archy restarted.
    fn revive_voice(&mut self) {
        let Some(v) = self.voice.as_mut() else { return };
        if !v.exited() {
            return;
        }
        if self.voice_started.is_some_and(|t| t.elapsed().as_secs() < 10) {
            return; // space out restarts while a mic is missing
        }
        log("voice: listener stopped; restarting it");
        self.voice = None;
        self.voice_config = None;
        self.sync_voice();
    }

    /// Whether the encoder has been skipping a real share of frames (over 15%
    /// for three checks in a row, ~6 s) at more than 60 fps: a GPU that can't
    /// keep up makes uneven clips, and 60 fps is smooth everywhere.
    fn falling_behind(&mut self) -> bool {
        let Some(s) = &self.session else { return false };
        let (sent, skipped) = s.recorder.frame_counts();
        let (last_sent, last_skipped, strikes) = self.keep_up;
        let (ds, dk) = (sent.saturating_sub(last_sent), skipped.saturating_sub(last_skipped));
        let behind = s.recorder.info.fps > 60 && ds + dk > 30 && dk * 100 > (ds + dk) * 15;
        let strikes = if behind { strikes + 1 } else { 0 };
        self.keep_up = (sent, skipped, strikes);
        strikes >= 3
    }

    fn watch(&mut self) {
        self.pick_up_settings();
        self.revive_voice();
        self.expire_previous();
        // A recording being started on the worker thread: take its result, or wait.
        if let Some(s) = &self.starting {
            match s.rx.try_recv() {
                Ok(done) => {
                    let s = self.starting.take().expect("checked above");
                    self.finish_start(s, done);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.starting = None,
            }
        }
        // The picked game's window, looked up once per tick.
        let picked = Some(&self.settings.record_target)
            .filter(|t| !t.is_empty())
            .and_then(|t| watch::find(t));
        let behind = self.falling_behind();
        self.ticks = self.ticks.wrapping_add(1);
        let check_discord = self.ticks % 5 == 0 && self.settings.audio_sources().1;
        if let Some(s) = &self.session {
            let window_gone = !watch::alive(s.hwnd);
            let ended = window_gone || s.recorder.status() != RecorderStatus::Recording;
            // The user picked another game and it's open: switch to it.
            let target = &self.settings.record_target;
            let switch = !ended
                && !target.is_empty()
                && !s.exe.eq_ignore_ascii_case(target)
                && picked.as_ref().is_some_and(|w| !w.minimized);
            // A minimized game gives no new frames: encoding the same frozen
            // picture for hours is pure waste. Stop (the buffer is kept) and start
            // again when it's back.
            let minimized = !ended && s.window_target && unsafe { IsIconic(s.hwnd) }.as_bool();
            self.minimized_ticks = if minimized { self.minimized_ticks + 1 } else { 0 };
            let minimized = self.minimized_ticks >= 2;
            // Discord opened after recording started: restart once to add its track.
            let discord = !ended
                && check_discord
                && !s.recorder.audio_tracks().iter().any(|t| t == "Discord")
                && archy_audio::find_root_process("Discord.exe").is_some();
            let slow = !ended && !switch && behind;
            if !ended && !switch && !slow && !minimized && !discord {
                return; // keep recording even while alt-tabbed
            }
            if slow {
                let fps = s.recorder.info.fps;
                self.fps_cap = Some((s.exe.clone(), 60));
                notify(
                    self.window.0 as isize,
                    "Recording at 60 fps",
                    &format!("Your graphics chip couldn't keep up with {fps} fps in this game, so Archy switched to 60 to keep clips smooth."),
                    true,
                );
            }
            let failed = s.recorder.status() == RecorderStatus::Failed;
            let reason = if slow {
                "the GPU couldn't keep up; continuing at 60 fps".to_string()
            } else if switch {
                format!("switching to the picked game ({target})")
            } else if window_gone {
                "the game window closed".to_string()
            } else if minimized {
                "the game was minimized (recording resumes when it's back)".to_string()
            } else if discord {
                "Discord started; adding its track".to_string()
            } else {
                s.recorder.stop_reason()
            };
            let (exe, game, key) = (s.exe.clone(), s.game.clone(), s.recorder.info.encoder_key.clone());
            if failed {
                // An encoder that stopped working is skipped from now on (C3/C4):
                // the next start uses the next encoder, GPU or codec.
                if reason.to_ascii_lowercase().contains("encoder") && !self.avoid.contains(&key) {
                    self.encoder_strikes.push(key.clone());
                    if self.encoder_strikes.iter().filter(|k| **k == key).count() >= 2 {
                        log(&format!("won't use {key} again until Archy restarts (failed twice)"));
                        self.avoid.push(key);
                        self.avoid_since = Some(std::time::Instant::now());
                    }
                }
                self.note_failure(&exe, &game, &reason);
            }
            self.stop_session(&reason);
        }
        if self.paused {
            return;
        }
        // A game the user picked is recorded whenever it's open (not minimized),
        // in front or not; otherwise, the game being played.
        let fg = match picked {
            Some(w) if !w.minimized => w,
            Some(_) => return, // the picked game is minimized: wait for it
            None => {
                let Some(fg) = watch::foreground() else {
                    return;
                };
                if !fg.is_game(&self.settings.games, self.settings.record_fullscreen_apps, &self.settings.never_record) {
                    return;
                }
                fg
            }
        };
        if self.failed_window == Some(fg.hwnd.0 as isize) {
            if watch::alive(fg.hwnd) {
                return;
            }
            self.failed_window = None;
        }
        if let Some((exe, until)) = &self.blocked {
            if std::time::Instant::now() < *until && exe.eq_ignore_ascii_case(&fg.exe) {
                return;
            }
        }
        self.start(fg);
    }

    /// Starts recording `fg` on a worker thread: creating an encoder can take
    /// seconds (and a broken driver can hang), and the tray must keep answering
    /// hotkeys, voice and its menu meanwhile.
    fn start(&mut self, fg: watch::Foreground) {
        // A ban is a guess about a driver; give the encoder another chance later.
        if self.avoid_since.is_some_and(|t| t.elapsed().as_secs() > 15 * 60) {
            log("encoder bans expired; trying every encoder again");
            self.avoid.clear();
            self.encoder_strikes.clear();
            self.avoid_since = None;
        }
        let target = if self.settings.capture.eq_ignore_ascii_case("screen") {
            // The screen picked in settings, else (or if it was unplugged) the game's.
            let chosen = archy_core::monitors::find(&self.settings.capture_monitor);
            Target::Monitor(chosen.unwrap_or_else(|| unsafe {
                MonitorFromWindow(fg.hwnd, MONITOR_DEFAULTTONEAREST)
            }))
        } else {
            Target::Window(fg.hwnd)
        };
        let game = watch::game_name(&fg.exe);
        // Fallbacks, in order: some hardware encoders (or their drivers) refuse
        // 120/144 fps, so try 60 on the same window; some refuse sizes above
        // 1080p; some games can't be captured as a window at all (exclusive
        // fullscreen, protected windows), so then record the screen the game is on.
        // A game this GPU couldn't keep up with earlier starts at the lower rate.
        let mut base = self.settings.clone();
        if let Some((exe, fps)) = &self.fps_cap {
            if exe.eq_ignore_ascii_case(&fg.exe) {
                base.fps = base.fps.min(*fps);
            }
        }
        let screen = Target::Monitor(unsafe { MonitorFromWindow(fg.hwnd, MONITOR_DEFAULTTONEAREST) });
        let config = |s: &Settings, t: Target, max_height: Option<u32>| {
            let mut c = recorder_config(s, t, fg.pid, &self.volumes, &self.avoid);
            if let Some(h) = max_height {
                c.max_height = c.max_height.min(h);
            }
            c
        };
        let mut at60 = base.clone();
        at60.fps = 60;
        let mut attempts: Vec<(RecorderConfig, String)> = Vec::new();
        let first = config(&base, target, None);
        let tall = first.max_height > 1080;
        attempts.push((first, format!("{} fps", base.fps)));
        if base.fps > 60 {
            attempts.push((config(&at60, target, None), "60 fps".into()));
        }
        if tall {
            attempts.push((config(&at60, target, Some(1080)), "1080p at 60 fps".into()));
        }
        if let Target::Window(_) = target {
            attempts.push((config(&base, screen, None), format!("its screen at {} fps", base.fps)));
            if base.fps > 60 {
                attempts.push((config(&at60, screen, None), "its screen at 60 fps".into()));
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let window = self.window.0 as isize;
        let label = game.clone();
        let spawned = std::thread::Builder::new().name("archy-start".into()).spawn(move || {
            let _ = archy_core::init_thread();
            let mut notes = Vec::new();
            let mut last = String::from("no way to record it");
            for (cfg, how) in attempts {
                let fps = cfg.fps;
                match Recorder::start(cfg) {
                    Ok(rec) => {
                        let _ = tx.send(Started { result: Ok(rec), notes, fps });
                        unsafe {
                            let _ = PostMessageW(Some(HWND(window as *mut _)), WM_STARTED, WPARAM(0), LPARAM(0));
                        }
                        return;
                    }
                    Err(e) => {
                        last = e.message();
                        notes.push(format!("recording {label} failed ({last}) at {how}"));
                    }
                }
            }
            let _ = tx.send(Started { result: Err(last), notes, fps: 0 });
            unsafe {
                let _ = PostMessageW(Some(HWND(window as *mut _)), WM_STARTED, WPARAM(0), LPARAM(0));
            }
        });
        if spawned.is_err() {
            log("couldn't start the recording thread");
            return;
        }
        self.starting = Some(Starting {
            rx,
            hwnd: fg.hwnd.0 as isize,
            game,
            exe: fg.exe,
            window_target: matches!(target, Target::Window(_)),
        });
    }

    /// The worker thread finished starting a recording.
    fn finish_start(&mut self, s: Starting, done: Started) {
        for n in &done.notes {
            log(n);
        }
        let hwnd = HWND(s.hwnd as *mut _);
        match done.result {
            Ok(rec) => {
                let i = &rec.info;
                log(&format!(
                    "recording {}: {} {}x{}@{} via {} on {}; audio: {}",
                    s.game,
                    i.codec,
                    i.width,
                    i.height,
                    i.fps,
                    i.encoder,
                    i.gpu,
                    rec.audio_report.join(", ")
                ));
                if done.fps < self.settings.fps && !done.notes.is_empty() {
                    notify(
                        self.window.0 as isize,
                        &format!("Recording at {} fps", done.fps),
                        &format!("Your graphics chip couldn't record {} fps, so Archy uses {} for this game.", self.settings.fps, done.fps),
                        true,
                    );
                }
                // HDR desktops are tone-mapped for SDR capture: say so once.
                let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
                if !self.hdr_warned && archy_core::monitors::hdr_on(monitor) {
                    self.hdr_warned = true;
                    log("HDR is on for the game's screen: clips are recorded in SDR");
                    notify(
                        self.window.0 as isize,
                        "HDR is on",
                        "Clips are recorded in standard colour, so they can look a little washed out. For exact colours, turn HDR off in Windows display settings while you play.",
                        true,
                    );
                }
                self.keep_up = (0, 0, 0);
                self.minimized_ticks = 0;
                self.session = Some(Session {
                    recorder: Arc::new(rec),
                    hwnd,
                    game: s.game,
                    exe: s.exe,
                    window_target: s.window_target,
                });
                // A settings change or pause arrived while it was starting.
                if std::mem::take(&mut self.stop_after_start) {
                    self.stop_session("settings changed while starting");
                } else {
                    self.sync_session();
                }
            }
            Err(e) => {
                log(&format!("could not record {}: {e}", s.game));
                notify(self.window.0 as isize, "Archy can't record this game", &e, true);
                // Don't retry this window every 2 s; other games still record.
                self.failed_window = Some(s.hwnd);
                self.stop_after_start = false;
                self.note_failure(&s.exe, &s.game, &e);
            }
        }
        self.publish();
    }

    /// Ends the current recording. The recorder stops at once; its buffer is kept
    /// as `previous` (a clip saved right after still has the full length), and
    /// the older one is dropped on another thread (joining its threads can take
    /// seconds, and the tray must never wait for that).
    fn stop_session(&mut self, reason: &str) {
        let Some(s) = self.session.take() else { return };
        let health = s.recorder.health();
        if health.is_empty() {
            log(&format!("stopped recording {}: {reason}", s.game));
        } else {
            log(&format!("stopped recording {}: {reason} ({health})", s.game));
        }
        end_session_file(&s.recorder, self.window.0 as isize);
        s.recorder.stop();
        let old = self.previous.replace(Previous {
            recorder: s.recorder,
            game: s.game,
            ended: std::time::Instant::now(),
        });
        if let Some(old) = old {
            std::thread::spawn(move || drop(old));
        }
        self.publish();
    }

    /// Lets go of the previous recording's buffer once no clip could need it.
    fn expire_previous(&mut self) {
        let keep = std::time::Duration::from_secs(self.settings.buffer_seconds() as u64 + 15);
        if self.previous.as_ref().is_some_and(|p| p.ended.elapsed() > keep) {
            if let Some(old) = self.previous.take() {
                std::thread::spawn(move || drop(old));
            }
        }
    }

    /// Counts failures per game; three within five minutes pause that game for
    /// five minutes (it was restarted every 2 s forever, silently), with one
    /// notification saying why.
    fn note_failure(&mut self, exe: &str, game: &str, why: &str) {
        let now = std::time::Instant::now();
        self.failures.retain(|(_, t)| now.duration_since(*t).as_secs() < 300);
        self.failures.push((exe.to_string(), now));
        let count = self.failures.iter().filter(|(e, _)| e.eq_ignore_ascii_case(exe)).count();
        if count >= 3 {
            self.failures.retain(|(e, _)| !e.eq_ignore_ascii_case(exe));
            self.blocked = Some((exe.to_string(), now + std::time::Duration::from_secs(300)));
            log(&format!("recording {game} failed 3 times in 5 minutes ({why}); retrying in 5 minutes"));
            notify(
                self.window.0 as isize,
                &format!("Archy paused recording {game}"),
                &format!("It failed 3 times in a few minutes ({why}). Archy tries again in 5 minutes."),
                true,
            );
        }
    }

    /// A clip hotkey: a ping right away (the save itself takes a moment), then save.
    fn hotkey_save(&self, seconds: u32) {
        if self.settings.sound_on_hotkey && (self.session.is_some() || self.previous.is_some()) {
            play(VOICE_WAV, self.settings.voice_sound_volume);
        }
        self.save(seconds);
    }

    /// Starts or ends the whole-session file to match the setting (and, in
    /// "hotkey" mode, the hotkey). Applies live: no restart.
    fn sync_session(&mut self) {
        let Some(s) = &self.session else { return };
        let mode = self.settings.session_mode.as_str();
        let want = mode == "always" || (mode == "hotkey" && self.session_wanted);
        let active = s.recorder.session_active();
        if want && !active {
            let clip = clip::new_path(&self.settings.save_folder, &s.game);
            let stem = clip.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            let path = clip.with_file_name(format!("{stem} (session).mp4"));
            let _ = std::fs::create_dir_all(&self.settings.save_folder);
            if s.recorder.start_session(&path) {
                log(&format!("session: recording to {}", path.display()));
            }
        } else if !want && active {
            end_session_file(&s.recorder, self.window.0 as isize);
        }
    }

    /// Starts, restarts or stops push-to-talk to match the settings.
    fn sync_ptt(&mut self) {
        let level = self.settings.audio_volumes().3;
        let want = self.settings.push_to_talk && !self.settings.push_to_talk_key.trim().is_empty();
        match (&self.ptt, want) {
            (Some(p), true) if p.key == self.settings.push_to_talk_key => p.set_level(level),
            (_, true) => {
                self.ptt = None; // the old key's thread ends first
                match ptt::PushToTalk::start(&self.settings.push_to_talk_key, self.volumes.mic.clone(), level) {
                    Some(p) => {
                        log(&format!("push-to-talk on ({})", self.settings.push_to_talk_key));
                        self.ptt = Some(p);
                    }
                    None => notify(self.window.0 as isize, "Push-to-talk", &format!("\"{}\" isn't a key Archy knows.", self.settings.push_to_talk_key), true),
                }
            }
            (Some(_), false) => {
                self.ptt = None;
                self.volumes.update(&self.settings); // the mic back at its volume
                log("push-to-talk off");
            }
            (None, false) => {}
        }
    }

    /// The session hotkey ("hotkey" mode): starts or stops the session file.
    fn toggle_session(&mut self) {
        let window = self.window.0 as isize;
        if self.settings.session_mode != "hotkey" {
            notify(window, "Session recording is off", "Turn on Settings > Recording > Record whole sessions > With a hotkey.", true);
            return;
        }
        self.session_wanted = !self.session_wanted;
        // Two different sounds, so you can tell without looking: the voice ping
        // when the session starts, the clip-saved chime when it stops.
        if self.session_wanted {
            play(VOICE_WAV, self.settings.voice_sound_volume.max(30));
        } else {
            play(CLIP_WAV, self.settings.clip_sound_volume.max(30));
        }
        if self.session_wanted {
            if self.session.is_some() {
                notify(window, "Recording the whole session", "Press the key again to stop. Your clip key adds bookmarks.", false);
            } else {
                notify(window, "Session recording on", "It starts with your next game.", false);
            }
        }
        self.sync_session();
    }

    /// The screenshot hotkey: a PNG of the game into the clips folder.
    fn screenshot(&self) {
        let window = self.window.0 as isize;
        let Some(s) = &self.session else {
            notify(window, "No screenshot", "Archy takes screenshots of the game it's recording.", true);
            return;
        };
        if self.settings.sound_on_hotkey {
            play(VOICE_WAV, self.settings.voice_sound_volume);
        }
        let recorder = s.recorder.clone();
        let folder = self.settings.save_folder.clone();
        let path = clip::new_path(&folder, &s.game).with_extension("png");
        let copy = self.settings.copy_after_save;
        let show = self.settings.notify_on_save;
        std::thread::spawn(move || {
            let _ = std::fs::create_dir_all(&folder);
            let t = std::time::Instant::now();
            match recorder.screenshot(&path) {
                Ok(p) => {
                    log(&format!("screenshot {} ({} ms)", p.display(), t.elapsed().as_millis()));
                    let copied = copy && archy_core::clipboard::copy_file(&p);
                    if show {
                        notify(window, "Screenshot saved", if copied { "Copied, paste it anywhere" } else { "In your clips folder" }, false);
                    }
                }
                Err(e) => {
                    log(&format!("screenshot failed: {e}"));
                    notify(window, "Screenshot not saved", &e, true);
                }
            }
        });
    }

    fn save(&self, seconds: u32) {
        let window = self.window.0 as isize;
        if let Some(s) = &self.session {
            if s.recorder.bookmark() {
                log("session: bookmark added");
            }
        }
        // What to save from: the current recording, plus the previous one when the
        // current started less than `seconds` ago (a restart: settings changed, the
        // encoder failed, the game was minimized). The earlier part is saved as its
        // own clip, so nothing from before the restart is lost.
        let mut parts: Vec<(Arc<Recorder>, String)> = Vec::new();
        if let Some(s) = &self.session {
            parts.push((s.recorder.clone(), s.game.clone()));
        }
        if let Some(p) = &self.previous {
            let recent = p.ended.elapsed().as_secs_f64() < seconds as f64;
            let short = self
                .session
                .as_ref()
                .is_none_or(|s| s.recorder.buffered_seconds() + 2.0 < seconds as f64);
            if recent && short {
                let name = if parts.is_empty() { p.game.clone() } else { format!("{} (before restart)", p.game) };
                parts.push((p.recorder.clone(), name));
            }
        }
        if parts.is_empty() {
            let why = if self.paused {
                "Archy is paused."
            } else {
                "Archy records while a game is open."
            };
            notify(window, "No clip saved", why, true);
            return;
        }
        let folder = self.settings.save_folder.clone();
        let _ = std::fs::create_dir_all(&folder);
        // A full drive: say so instead of failing halfway through writing.
        if let Some(free) = archy_core::files::free_bytes(&folder) {
            if free < 300_000_000 {
                log(&format!("save refused: only {} MB free", free / 1_000_000));
                notify(window, "Clip not saved: the drive is full", &format!("Only {} MB free on the drive with your clips. Free some space, or pick another folder in Archy.", free / 1_000_000), true);
                return;
            }
        }
        if self.saving.swap(true, Ordering::AcqRel) {
            // A save is running: do this one right after it, instead of dropping
            // it (a second "Archer clip that" or F8 used to do nothing).
            self.queued.fetch_max(seconds, Ordering::AcqRel);
            log("clip request queued: a save is still running");
            return;
        }
        let copy = self.settings.copy_after_save;
        let show = self.settings.notify_on_save;
        let chime = self.settings.sound_on_save;
        let chime_volume = self.settings.clip_sound_volume;
        let limit_gb = self.settings.clips_max_gb;
        let saving = self.saving.clone();
        let queued = self.queued.clone();
        std::thread::spawn(move || {
            archy_core::eco::boost_current_thread();
            let mut seconds = seconds;
            loop {
                let mut saved: Vec<archy_core::recorder::ClipInfo> = Vec::new();
                let mut error = None;
                for (recorder, game) in &parts {
                    let path = clip::new_path(&folder, game);
                    match recorder.save_clip(seconds, &path) {
                        Ok(c) => {
                            log(&format!(
                                "saved {} ({:.1} s, {:.1} MB, {} ms)",
                                c.path.display(),
                                c.seconds,
                                c.bytes as f64 / 1e6,
                                c.save_time.as_millis()
                            ));
                            saved.push(c);
                        }
                        Err(e) => {
                            log(&format!("save failed: {}", e.message()));
                            error = Some(e.message());
                        }
                    }
                }
                if let Some(c) = saved.first() {
                    let copied = copy && archy_core::clipboard::copy_file(&c.path);
                    if chime {
                        play(CLIP_WAV, chime_volume);
                    }
                    if show {
                        let mb = c.bytes as f64 / 1e6;
                        let mut extra = if copied {
                            " · copied, paste it anywhere".to_string()
                        } else {
                            String::new()
                        };
                        if saved.len() > 1 {
                            extra.push_str(" · plus the part before Archy restarted recording");
                        }
                        if mb > 10.0 {
                            extra.push_str(" · over Discord's 10 MB: use Discord copy in Archy");
                        }
                        notify(
                            window,
                            &format!("Clip saved · last {:.0} s", c.seconds),
                            &format!("{mb:.1} MB{extra}"),
                            false,
                        );
                    }
                    trim_clips_folder(&folder, limit_gb);
                    warn_low_disk(window, &folder);
                } else if let Some(e) = error {
                    notify(window, "Clip not saved", &e, true);
                }
                if let next @ 1.. = queued.swap(0, Ordering::AcqRel) {
                    seconds = next;
                    continue;
                }
                saving.store(false, Ordering::Release);
                // A request that arrived while finishing up.
                match queued.swap(0, Ordering::AcqRel) {
                    0 => break,
                    late if !saving.swap(true, Ordering::AcqRel) => seconds = late,
                    _ => break,
                }
            }
        });
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if self.paused {
            if self.starting.is_some() {
                self.stop_after_start = true;
            }
            self.stop_session("paused"); // zero load while paused
        }
        self.sync_voice();
        notify(
            self.window.0 as isize,
            if self.paused {
                "Archy paused"
            } else {
                "Archy resumed"
            },
            &self.status_text(),
            false,
        );
        self.publish();
    }

    fn show_menu(&mut self) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(
                menu,
                MF_GRAYED,
                0,
                PCWSTR(wide(&self.status_text()).as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open Archy"));
            let save = format!("Save clip ({})", self.settings.hotkey_save);
            let _ = AppendMenuW(menu, MF_STRING, MENU_SAVE, PCWSTR(wide(&save).as_ptr()));
            let pause = if self.paused {
                "Resume recording"
            } else {
                "Pause recording"
            };
            let _ = AppendMenuW(menu, MF_STRING, MENU_PAUSE, PCWSTR(wide(pause).as_ptr()));
            let _ = AppendMenuW(menu, MF_STRING, MENU_FOLDER, w!("Open clips folder"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Edit settings file"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit Archy"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // Required so the menu closes when clicking elsewhere.
            let _ = SetForegroundWindow(self.window);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                Some(0),
                self.window,
                None,
            );
            let _ = DestroyMenu(menu);
            match cmd.0 as usize {
                MENU_SAVE => self.save(self.settings.clip_seconds),
                MENU_PAUSE => self.toggle_pause(),
                MENU_FOLDER => {
                    let _ = std::fs::create_dir_all(&self.settings.save_folder);
                    open_path(&self.settings.save_folder);
                }
                MENU_SETTINGS => open_path(&settings::path()),
                MENU_OPEN => open_ui(),
                MENU_QUIT => {
                    let _ = DestroyWindow(self.window);
                }
                _ => {}
            }
        }
    }
}

static CLIP_WAV: &[u8] = include_bytes!("../../../assets/clip.wav");
static VOICE_WAV: &[u8] = include_bytes!("../../../assets/voice.wav");

/// Plays one of Archy's short sounds without blocking, at `volume` percent (0-200).
fn play(sound: &'static [u8], volume: u32) {
    if volume == 0 {
        return;
    }
    let data = if volume == 100 { sound } else { scaled(sound, volume) };
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(data.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

/// A copy of a 16-bit PCM WAV at `volume` percent. Kept for the life of the
/// process (PlaySound reads it while playing; only a few volumes are ever used).
/// Above 100% loud peaks are rounded off instead of clipping into crackle.
fn scaled(sound: &'static [u8], volume: u32) -> &'static [u8] {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashMap<(usize, u32), &'static [u8]>>> = Mutex::new(None);
    let key = (sound.as_ptr() as usize, volume);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&d) = cache.get_or_insert_with(HashMap::new).get(&key) {
        return d;
    }
    let mut out = sound.to_vec();
    // Samples start after the "data" chunk header (our WAVs are plain PCM16).
    if let Some(at) = out.windows(4).position(|w| w == b"data") {
        let gain = volume as f32 / 100.0;
        for s in out[at + 8..].chunks_exact_mut(2) {
            let x = i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0 * gain;
            let y = if x.abs() <= 0.8 {
                x
            } else {
                x.signum() * (0.8 + 0.2 * ((x.abs() - 0.8) / 0.2).tanh())
            };
            s.copy_from_slice(&((y * 32767.0) as i16).to_le_bytes());
        }
    }
    let leaked: &'static [u8] = Box::leak(out.into_boxed_slice());
    cache.get_or_insert_with(HashMap::new).insert(key, leaked);
    leaked
}

/// Archy's icon at the size Windows uses for tray icons (scales with DPI);
/// `recording` adds the red dot.
fn app_icon(recording: bool) -> HICON {
    static NORMAL: &[u8] = include_bytes!("../../../assets/archy.ico");
    static REC: &[u8] = include_bytes!("../../../assets/archy-rec.ico");
    let ico = if recording { REC } else { NORMAL };
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON);
        let offset = LookupIconIdFromDirectoryEx(ico.as_ptr(), true, size, size, LR_DEFAULTCOLOR);
        if offset > 0 {
            if let Ok(icon) = CreateIconFromResourceEx(
                &ico[offset as usize..],
                true,
                0x0003_0000,
                size,
                size,
                LR_DEFAULTCOLOR,
            ) {
                return icon;
            }
        }
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}

/// Settings that need the recorder restarted (the rest apply live).
fn recording_changed(a: &Settings, b: &Settings) -> bool {
    a.quality != b.quality
        || a.clip_seconds != b.clip_seconds
        // Which tracks exist (volumes alone apply live)
        || a.audio_sources() != b.audio_sources()
        || a.mic_wanted() != b.mic_wanted()
        || a.audio_kbps != b.audio_kbps
        || a.mic_device != b.mic_device
        || a.output_device != b.output_device
        || a.fps != b.fps
        || a.codec != b.codec
        || a.capture != b.capture
        || a.capture_monitor != b.capture_monitor
        || a.encoder_gpu != b.encoder_gpu
        || a.buffer_seconds() != b.buffer_seconds()
        // So excluding the app being recorded stops it right away.
        || a.frame_timing != b.frame_timing
        || a.never_record != b.never_record
        || a.record_target != b.record_target
}

/// Opens the Archy window, or brings it to the front if it is already open.
fn open_ui() {
    unsafe {
        if let Ok(existing) = FindWindowW(None, w!("Archy")) {
            let _ = ShowWindow(existing, SW_RESTORE);
            let _ = SetForegroundWindow(existing);
            return;
        }
    }
    let Ok(mut exe) = std::env::current_exe() else {
        return;
    };
    exe.set_file_name("archy-ui.exe");
    if let Err(e) = std::process::Command::new(&exe).spawn() {
        log(&format!("could not open {}: {e}", exe.display()));
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|a| {
        if let Ok(mut guard) = a.try_borrow_mut() {
            if let Some(app) = guard.as_mut() {
                f(app);
            }
        }
    });
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_WATCH => with_app(|a| a.watch()),
        WM_VOICE => with_app(|a| a.heard(wp.0, lp.0)),
        WM_STARTED => with_app(|a| a.watch()),
        WM_HOTKEY => with_app(|a| match wp.0 as i32 {
            HK_SAVE => a.hotkey_save(a.settings.clip_seconds),
            HK_SAVE_LONG => a.hotkey_save(a.settings.long_clip_seconds),
            HK_PAUSE => a.toggle_pause(),
            HK_SCREENSHOT => a.screenshot(),
            HK_SESSION => a.toggle_session(),
            id if id >= HK_EXTRA => {
                if let Some(h) = a.settings.extra_hotkeys.get((id - HK_EXTRA) as usize) {
                    let seconds = h.seconds;
                    a.hotkey_save(seconds);
                }
            }
            _ => {}
        }),
        WM_TRAY => match (lp.0 as u32) & 0xffff {
            WM_RBUTTONUP | WM_CONTEXTMENU => with_app(|a| a.show_menu()),
            // NIN_BALLOONUSERCLICK: a notification was clicked (e.g. "update available").
            WM_LBUTTONUP | 0x405 => open_ui(),
            _ => {}
        },
        WM_DESTROY => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().take() {
                    // A clip being saved finishes first (an update or quit used to cut it off).
                    let t = std::time::Instant::now();
                    while app.saving.load(Ordering::Acquire) && t.elapsed().as_secs() < 15 {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    unsafe {
                        let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(app.window));
                    }
                    // Tells the Archy window the recorder is gone.
                    let _ = std::fs::remove_file(archy_core::status::path());
                }
            });
            unsafe { PostQuitMessage(0) };
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}

/// `archy.exe --mic-test <device|-> <seconds> <out.wav>`: records the mic to a
/// WAV for the window to play back ("Test your mic"). Prints "ok" or the error.
fn mic_test_main(args: &[String]) {
    let device = args.first().filter(|d| d.as_str() != "-").cloned();
    let seconds: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5.0f32).clamp(1.0, 15.0);
    let Some(out) = args.get(2).map(std::path::PathBuf::from) else {
        println!("no output file");
        return;
    };
    const RATE: u32 = 48_000;
    let stop = Arc::new(AtomicBool::new(false));
    let mut samples: Vec<i16> = Vec::with_capacity((RATE as f32 * seconds) as usize);
    let want = (RATE as f32 * seconds) as usize;
    let flag = stop.clone();
    let result = archy_audio::listen_microphone(device.as_deref(), RATE, &stop, &mut |pcm| {
        samples.extend_from_slice(pcm);
        if samples.len() >= want {
            flag.store(true, Ordering::Release);
        }
    });
    if let Err(e) = result {
        println!("couldn't open the microphone: {}", e.message());
        return;
    }
    // 16-bit mono WAV.
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    match std::fs::write(&out, wav) {
        Ok(()) => println!("ok"),
        Err(e) => println!("couldn't save the recording: {e}"),
    }
}

/// `archy.exe --self-test <fps> <ffmpeg.exe>`: records a test window at `fps`
/// and checks every frame (archy_core::selftest); prints the report as JSON.
fn self_test_main(args: &[String]) {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // The same low-priority, throttled process the tray records in.
    archy_core::eco::apply();
    let fps = args.first().and_then(|a| a.parse().ok()).unwrap_or(60u32).clamp(24, 144);
    let opts = archy_core::selftest::Options {
        fps,
        seconds: 8,
        mbps: 5,
        ffmpeg: args.get(1).map(std::path::PathBuf::from).unwrap_or_default(),
    };
    let line = match archy_core::selftest::run(&opts) {
        Ok(r) => serde_json::to_string(&r).unwrap_or_default(),
        Err(e) => serde_json::json!({ "fps": fps, "error": e }).to_string(),
    };
    log(&format!("self-test: {line}"));
    println!("{line}");
}

fn main() {
    // The voice helper process (see voice.rs) shares this exe.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--voice-calibrate") {
        voice::calibrate_main(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("--voice-test") {
        voice::test_main();
        return;
    }
    if args.first().map(String::as_str) == Some("--mic-test") {
        mic_test_main(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("--self-test") {
        self_test_main(&args[1..]);
        return;
    }
    if args.first().map(String::as_str) == Some("--voice-host") {
        voice::host_main(&args[1..], log);
        return;
    }
    unsafe {
        // One Archy at a time.
        let _mutex = CreateMutexW(None, true, w!("Local\\ArchyRecorder"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if let Err(e) = archy_core::init_thread() {
        log(&format!("start-up failed: {}", e.message()));
        return;
    }
    let (settings, settings_error) = Settings::load();
    if settings.efficiency_mode {
        archy_core::eco::apply();
    }
    log("Archy started");
    // The installer keeps the replaced program files until this version runs.
    if let Ok(exe) = std::env::current_exe() {
        for old in ["archy.exe.old", "archy-ui.exe.old"] {
            let _ = std::fs::remove_file(exe.with_file_name(old));
        }
    }

    let instance = unsafe { GetModuleHandleW(None) }.expect("module handle");
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("ArchyTray"),
        ..Default::default()
    };
    unsafe { RegisterClassW(&class) };
    // A hidden top-level window: receives tray, hotkey and timer messages.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("ArchyTray"),
            // Not "Archy": that title belongs to the Archy window (see open_ui).
            w!("ArchyTray"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .expect("create window");

    let mut nid = tray_data(window);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = app_icon(false);
    wide_into(&mut nid.szTip, "Archy: waiting for a game");
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }

    let app = App {
        window,
        settings,
        session: None,
        fps_cap: None,
        keep_up: (0, 0, 0),
        paused: false,
        saving: Arc::new(AtomicBool::new(false)),
        settings_modified: Settings::modified(),
        voice: None,
        voice_config: None,
        dot_shown: false,
        failed_window: None,
        volumes: TrackVolumes::default(),
        voice_started: None,
        queued: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        voice_warned: std::cell::Cell::new(None),
        starting: None,
        stop_after_start: false,
        previous: None,
        avoid: Vec::new(),
        avoid_since: None,
        encoder_strikes: Vec::new(),
        failures: Vec::new(),
        blocked: None,
        minimized_ticks: 0,
        ticks: 0,
        hdr_warned: false,
        session_wanted: false,
        ptt: None,
    };
    app.volumes.update(&app.settings);
    app.register_hotkeys();
    let hint = format!(
        "Recording starts when a game is open. {} saves the last {} s.",
        app.settings.hotkey_save, app.settings.clip_seconds
    );
    if app.settings.notify_on_start {
        notify(window.0 as isize, "Archy is running", &hint, false);
    }
    {
        // Checks shortly after start, then every 6 hours while Archy runs (it can
        // stay open for days). One light notification per new version; clicking
        // it opens Archy, which shows the update dialog. Settings are re-read each
        // time, so "Check for updates" off or "Skip this version" apply at once.
        let hwnd = window.0 as isize;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(30)); // let start-up (and the game) settle
            let mut told = String::new();
            loop {
                let s = Settings::load().0;
                if s.check_updates {
                    if let Ok(Some(u)) = archy_core::update::check(env!("CARGO_PKG_VERSION")) {
                        if u.version != s.skipped_version && u.version != told {
                            log(&format!("update available: {}", u.version));
                            notify(hwnd, &format!("Archy {} is available", u.version), "Click to see what's new and update (it asks first).", false);
                            told = u.version;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_secs(6 * 60 * 60));
            }
        });
    }
    // Keep the Windows start-up entry in line with the setting (and the exe's location).
    startup::set(app.settings.start_with_windows);
    if let Some(err) = settings_error {
        notify(window.0 as isize, "Archy settings", &err, true);
    }
    APP.with(|a| *a.borrow_mut() = Some(app));
    unsafe {
        SetTimer(Some(window), TIMER_WATCH, 2000, None);
    }
    with_app(|a| {
        a.watch();
        a.sync_ptt();
        a.sync_voice();
        a.publish();
    });

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    log("Archy stopped");
}
