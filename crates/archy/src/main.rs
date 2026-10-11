//! Archy: a local-first game clipper that lives in the tray.
//!
//! While a game is in the foreground it keeps the last N seconds in RAM; the
//! hotkey (or, later, a voice phrase) saves them as an MP4 instantly. Nothing
//! is recorded when no game is running, and nothing ever leaves the PC.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clip;
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
const TIMER_WATCH: usize = 1;
const HK_SAVE: i32 = 1;
const HK_SAVE_LONG: i32 = 2;
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
}

struct App {
    window: HWND,
    settings: Settings,
    session: Option<Session>,
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
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn log(msg: &str) {
    let dir = settings::app_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("archy.log"))
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let _ = writeln!(f, "{:02}:{:02}:{:02} {msg}", t.wHour, t.wMinute, t.wSecond);
    }
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
        self.everything.store(s.volume_everything, Relaxed);
        self.game.store(s.volume_game, Relaxed);
        self.discord.store(s.volume_discord, Relaxed);
        self.mic.store(s.volume_mic, Relaxed);
    }

    fn of(&self, main: &archy_audio::Volume) -> archy_audio::Volumes {
        archy_audio::Volumes { main: main.clone(), mic: self.mic.clone() }
    }
}

fn recorder_config(s: &Settings, target: Target, game_pid: u32, vols: &TrackVolumes) -> RecorderConfig {
    let mut audio = Vec::new();
    if s.audio_everything {
        // Track 1 is what single-track players play, so it carries your mic too.
        let source = if s.audio_mic {
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
    if s.audio_game && game_pid != 0 {
        audio.push(AudioSource {
            name: "Game".into(),
            source: Source::Process { pid: game_pid },
            volumes: vols.of(&vols.game),
        });
    }
    if s.audio_discord {
        if let Some(pid) = archy_audio::find_root_process("Discord.exe") {
            audio.push(AudioSource {
                name: "Discord".into(),
                source: Source::Process { pid },
                volumes: vols.of(&vols.discord),
            });
        }
    }
    if s.audio_mic {
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
        (p.height, (p.mbps * 1_000_000.0) as u32)
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
    }
}

impl App {
    fn register_hotkeys(&self) {
        let keys = [
            (HK_SAVE, &self.settings.hotkey_save),
            (HK_SAVE_LONG, &self.settings.hotkey_save_long),
            (HK_PAUSE, &self.settings.hotkey_pause),
        ];
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
        if s.start_with_windows != self.settings.start_with_windows {
            startup::set(s.start_with_windows);
        }
        self.settings = s;
        self.volumes.update(&self.settings);
        self.register_hotkeys();
        self.sync_voice();
        self.publish();
        if restart {
            self.session = None; // restarts with the new quality on this tick
            self.paused = false;
        }
        log("settings applied");
    }

    /// Starts, restarts or stops the voice listener to match settings and pause state.
    fn sync_voice(&mut self) {
        let phrases: Vec<String> = self
            .settings
            .voice_phrases
            .iter()
            .map(|p| p.text.clone())
            .filter(|t| !t.trim().is_empty())
            .collect();
        if !self.settings.voice_enabled || self.paused || phrases.is_empty() {
            self.voice = None;
            self.voice_config = None;
            return;
        }
        let wanted = voice::VoiceConfig {
            phrases,
            mic: self.settings.mic_device.clone(),
            tuning: match (self.settings.voice_sensitivity, &self.settings.voice_tuning) {
                (0, Some(t)) => archy_voice::Tuning { score: t.score, paths: t.paths, threshold: t.threshold },
                (s, _) => archy_voice::Tuning::preset(s),
            },
            // Learned takes belong to the phrase that was calibrated, if it's still there.
            learned: match &self.settings.voice_tuning {
                Some(t) => self
                    .settings
                    .voice_phrases
                    .iter()
                    .position(|p| p.text == t.phrase)
                    .map(|i| t.learned.iter().map(|l| (i, l.clone())).collect())
                    .unwrap_or_default(),
                None => Vec::new(),
            },
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

    fn watch(&mut self) {
        self.pick_up_settings();
        self.revive_voice();
        if let Some(s) = &self.session {
            let ended = !watch::alive(s.hwnd) || s.recorder.status() != RecorderStatus::Recording;
            if !ended {
                return; // keep recording even while alt-tabbed
            }
            let reason = if !watch::alive(s.hwnd) {
                "the game window closed".to_string()
            } else {
                s.recorder.stop_reason()
            };
            let health = s.recorder.health();
            if health.is_empty() {
                log(&format!("stopped recording {}: {reason}", s.game));
            } else {
                log(&format!("stopped recording {}: {reason} ({health})", s.game));
            }
            self.session = None;
            self.publish();
        }
        if self.paused {
            return;
        }
        let Some(fg) = watch::foreground() else {
            return;
        };
        if !fg.is_game(&self.settings.games, self.settings.record_fullscreen_apps) {
            return;
        }
        if self.failed_window == Some(fg.hwnd.0 as isize) {
            if watch::alive(fg.hwnd) {
                return;
            }
            self.failed_window = None;
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
        let mut started = Recorder::start(recorder_config(&self.settings, target, fg.pid, &self.volumes));
        // Some games can't be captured as a window (exclusive fullscreen, protected
        // windows): record the screen the game is on instead.
        if let (Err(e), Target::Window(_)) = (&started, target) {
            log(&format!("window capture of {game} failed ({}); recording its screen instead", e.message()));
            let monitor = unsafe { MonitorFromWindow(fg.hwnd, MONITOR_DEFAULTTONEAREST) };
            started = Recorder::start(recorder_config(&self.settings, Target::Monitor(monitor), fg.pid, &self.volumes));
        }
        match started {
            Ok(rec) => {
                let i = &rec.info;
                log(&format!(
                    "recording {game}: {} {}x{}@{} via {} on {}; audio: {}",
                    i.codec,
                    i.width,
                    i.height,
                    i.fps,
                    i.encoder,
                    i.gpu,
                    rec.audio_report.join(", ")
                ));
                self.session = Some(Session {
                    recorder: Arc::new(rec),
                    hwnd: fg.hwnd,
                    game,
                });
            }
            Err(e) => {
                log(&format!("could not record {game}: {}", e.message()));
                notify(
                    self.window.0 as isize,
                    "Archy can't record this game",
                    &e.message(),
                    true,
                );
                // Don't retry this window every 2 s; other games still record.
                self.failed_window = Some(fg.hwnd.0 as isize);
            }
        }
        self.publish();
    }

    fn save(&self, seconds: u32) {
        let window = self.window.0 as isize;
        let Some(session) = &self.session else {
            let why = if self.paused {
                "Archy is paused."
            } else {
                "Archy records while a game is open."
            };
            notify(window, "No clip saved", why, true);
            return;
        };
        if self.saving.swap(true, Ordering::AcqRel) {
            // A save is running: do this one right after it, instead of dropping
            // it (a second "Archer clip that" or F8 used to do nothing).
            self.queued.fetch_max(seconds, Ordering::AcqRel);
            log("clip request queued: a save is still running");
            return;
        }
        let recorder = session.recorder.clone();
        let folder = self.settings.save_folder.clone();
        let game = session.game.clone();
        let copy = self.settings.copy_after_save;
        let show = self.settings.notify_on_save;
        let chime = self.settings.sound_on_save;
        let chime_volume = self.settings.clip_sound_volume;
        let saving = self.saving.clone();
        let queued = self.queued.clone();
        std::thread::spawn(move || {
            archy_core::eco::boost_current_thread();
            let mut seconds = seconds;
            loop {
                let _ = std::fs::create_dir_all(&folder);
                let path = clip::new_path(&folder, &game);
                match recorder.save_clip(seconds, &path) {
                    Ok(c) => {
                        let copied = copy && archy_core::clipboard::copy_file(&c.path);
                        log(&format!(
                            "saved {} ({:.1} s, {:.1} MB, {} ms)",
                            c.path.display(),
                            c.seconds,
                            c.bytes as f64 / 1e6,
                            c.save_time.as_millis()
                        ));
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
                    }
                    Err(e) => {
                        log(&format!("save failed: {}", e.message()));
                        notify(window, "Clip not saved", &e.message(), true);
                    }
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
            self.session = None; // dropping the recorder stops it: zero load while paused
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
        || a.audio_everything != b.audio_everything
        || a.audio_game != b.audio_game
        || a.audio_discord != b.audio_discord
        || a.audio_mic != b.audio_mic
        || a.audio_kbps != b.audio_kbps
        || a.mic_device != b.mic_device
        || a.output_device != b.output_device
        || a.fps != b.fps
        || a.codec != b.codec
        || a.capture != b.capture
        || a.capture_monitor != b.capture_monitor
        || a.encoder_gpu != b.encoder_gpu
        || a.buffer_seconds() != b.buffer_seconds()
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
        WM_HOTKEY => with_app(|a| match wp.0 as i32 {
            HK_SAVE => a.save(a.settings.clip_seconds),
            HK_SAVE_LONG => a.save(a.settings.long_clip_seconds),
            HK_PAUSE => a.toggle_pause(),
            _ => {}
        }),
        WM_TRAY => match (lp.0 as u32) & 0xffff {
            WM_RBUTTONUP | WM_CONTEXTMENU => with_app(|a| a.show_menu()),
            WM_LBUTTONUP => open_ui(),
            _ => {}
        },
        WM_DESTROY => {
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().take() {
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

fn main() {
    // The voice helper process (see voice.rs) shares this exe.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--voice-calibrate") {
        voice::calibrate_main(&args[1..]);
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
    if app.settings.check_updates {
        let skipped = app.settings.skipped_version.clone();
        let hwnd = window.0 as isize;
        std::thread::spawn(move || {
            // Let start-up (and the game) settle first.
            std::thread::sleep(std::time::Duration::from_secs(30));
            if let Ok(Some(u)) = archy_core::update::check(env!("CARGO_PKG_VERSION")) {
                if u.version != skipped {
                    log(&format!("update available: {}", u.version));
                    notify(hwnd, &format!("Archy {} is available", u.version), "Open Archy to update (it asks first).", false);
                }
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
