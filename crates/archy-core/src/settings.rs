//! User settings, stored as JSON in %APPDATA%\Archy\settings.json.
//! Every recording option lives here so anyone can change it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Seconds saved by the normal hotkey / voice phrase (15-60).
    pub clip_seconds: u32,
    /// Seconds saved by the "long clip" hotkey (15-60). Also the replay buffer length.
    pub long_clip_seconds: u32,
    /// "360p" | "480p" | "720p" | "1080p" | "1440p", or "discord": sized so a normal
    /// clip always fits Discord's 10 MB upload limit.
    pub quality: String,
    /// 24-144.
    pub fps: u32,
    /// "h264" (plays everywhere, incl. Discord previews) | "hevc" | "av1" | "auto" (best available).
    pub codec: String,
    pub save_folder: PathBuf,
    pub hotkey_save: String,
    pub hotkey_save_long: String,
    /// Saves a full-size PNG of the game ("" = off, the default).
    pub hotkey_screenshot: String,
    /// Whole-session recording: "off" (default), "hotkey" (started and stopped
    /// with `hotkey_session`) or "always" (whenever a game is recorded).
    pub session_mode: String,
    /// Starts/stops session recording in "hotkey" mode ("" = none).
    pub hotkey_session: String,
    /// Push-to-talk: the mic is silent in clips except while `push_to_talk_key`
    /// is held (off by default). Voice clipping still hears you.
    pub push_to_talk: bool,
    /// A key ("V", "Ctrl+Space") or a mouse side button ("Mouse4", "Mouse5").
    pub push_to_talk_key: String,
    /// RNNoise noise suppression on the mic (fans, keyboards, hum). Off by default.
    pub noise_suppression: bool,
    /// Mic gate threshold in dBFS: quieter than this is muted (-100 = off).
    pub mic_gate_db: i32,
    /// More clip hotkeys, each with its own length (empty = just the two above).
    pub extra_hotkeys: Vec<HotkeyClip>,
    pub hotkey_pause: String,
    /// Executable names that always count as games.
    pub games: Vec<String>,
    /// Also record any other app running fullscreen.
    pub record_fullscreen_apps: bool,
    /// Executable names that are never recorded (wins over `games`).
    pub never_record: Vec<String>,
    /// The game the user picked to record ("Client-Win64-Shipping.exe"), whether
    /// or not it's in front; empty = automatic (the game being played).
    pub record_target: String,
    /// "window" (only the game; popups never appear) | "screen" (whole monitor).
    pub capture: String,
    /// Screen to record in "screen" mode (Windows' name, e.g. `\\.\DISPLAY2`);
    /// empty = the screen the game is on.
    pub capture_monitor: String,
    /// GPU that records ("vvvv:dddd"); empty = automatic.
    pub encoder_gpu: String,
    /// Ask GitHub for a newer Archy when it starts (the only time it goes online).
    pub check_updates: bool,
    /// A version the user chose to skip ("Skip this version").
    pub skipped_version: String,
    /// Put each new clip on the clipboard, ready to paste into Discord.
    pub copy_after_save: bool,
    /// Keep Archy on efficiency cores at low priority so the game keeps the fast cores.
    pub efficiency_mode: bool,
    /// Track 1: everything the PC plays (what single-track players like Discord play).
    pub audio_everything: bool,
    /// Separate track with only the game's sound.
    pub audio_game: bool,
    /// Separate track with only Discord (your friends).
    pub audio_discord: bool,
    /// Separate track with your microphone.
    pub audio_mic: bool,
    /// Track volumes, 0-200 (%). Everything = what the PC plays in track 1.
    pub volume_everything: u32,
    pub volume_game: u32,
    pub volume_discord: u32,
    /// Your mic, in track 1 and in its own track.
    pub volume_mic: u32,
    /// AAC bitrate per track: 96, 128, 160 or 192.
    pub audio_kbps: u32,
    /// "main": one volume (`volume_everything`) for the whole clip; "separate":
    /// game, Discord and mic each at their own volume (0 = not recorded).
    pub audio_mode: String,
    /// "main" mode: also record game, Discord and mic as their own tracks, so the
    /// editor can change each one later. Off: one track only (lightest: one audio
    /// encoder instead of four).
    pub record_stems: bool,
    /// "smooth": frames keep the moment they were shown (smoothest playback);
    /// "constant": exactly 1/fps apart (for After Effects / Premiere).
    pub frame_timing: String,
    /// Keep the clips folder under this many GB by moving the oldest clips to the
    /// Recycle Bin after each save (0 = no limit).
    pub clips_max_gb: u32,
    /// Microphone to record (device id); empty = Windows' default microphone.
    pub mic_device: String,
    /// Output device track 1 records from (device id); empty = Windows' default output.
    pub output_device: String,
    /// Show a Windows notification when a clip saves.
    pub notify_on_save: bool,
    /// Play Archy's chime when a clip saves.
    pub sound_on_save: bool,
    /// Show "Archy is running" when it starts.
    pub notify_on_start: bool,
    /// Start Archy (in the tray) when you sign in to Windows.
    pub start_with_windows: bool,
    /// Put a dot on the tray icon while a game is being recorded.
    pub tray_recording_dot: bool,
    /// Save clips by saying a phrase (listens on this PC only).
    pub voice_enabled: bool,
    /// Phrases and how many seconds each saves.
    pub voice_phrases: Vec<VoicePhrase>,
    /// Play a short sound the moment a phrase is heard.
    pub sound_on_voice: bool,
    /// Play the same sound the moment a clip hotkey is pressed.
    pub sound_on_hotkey: bool,
    /// Volume of the clip-saved chime, 0-200 (%).
    pub clip_sound_volume: u32,
    /// Volume of the phrase-heard blip, 0-200 (%).
    pub voice_sound_volume: u32,
    /// How easily a phrase triggers: 25 strict, 50 balanced, 80 loose, or 0 =
    /// use `voice_tuning` from the user's own calibration.
    pub voice_sensitivity: u32,
    /// Detector settings found by calibrating with the user's voice and mic.
    pub voice_tuning: Option<VoiceTuning>,
    /// Fine-tuning on top of `voice_sensitivity` (or the calibration):
    /// -50 stricter … +50 looser, 0 = as is. Set with the slider and the voice test.
    pub voice_fine_tune: i32,
    /// The last voice test the user saved, to show how much to trust the setting.
    pub voice_test: Option<VoiceTestResult>,
    /// Window look (the recorder ignores these): "dark" | "light" | "system".
    pub theme: String,
    /// "blue" | "violet" | "teal" | "orange".
    pub accent: String,
    /// "comfortable" | "compact".
    pub density: String,
    /// No animations in the window.
    pub reduce_motion: bool,
    /// The first-run welcome and tour have been seen.
    pub onboarded: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct VoiceTestResult {
    /// Phrases caught, of how many read.
    pub caught: u32,
    pub expected: u32,
    /// Times it went off on sentences without the phrase.
    pub false_alarms: u32,
    /// The settings it was tested with (a change makes the result stale).
    pub sensitivity: u32,
    pub fine_tune: i32,
    /// When, in ms since 1970.
    pub when: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct VoiceTuning {
    pub score: f32,
    pub paths: u32,
    pub threshold: f32,
    /// How many of the calibration prompts it caught, e.g. 5 of 5.
    pub hits: u32,
    pub prompts: u32,
    /// The phrase that was calibrated.
    pub phrase: String,
    /// How the user actually says it, as voice-model tokens: extra triggers.
    pub learned: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VoicePhrase {
    pub text: String,
    pub seconds: u32,
}

/// An extra clip hotkey with its own length (e.g. Ctrl+F8 = last 2 minutes).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct HotkeyClip {
    pub key: String,
    pub seconds: u32,
}

impl Default for HotkeyClip {
    fn default() -> Self {
        Self { key: String::new(), seconds: 120 }
    }
}

/// Longest clip any hotkey or phrase can save (5 minutes). The replay buffer
/// holds this much in RAM while you play: about 6 MB a second at 1080p60.
pub const MAX_CLIP_SECONDS: u32 = 300;
/// Most extra clip hotkeys.
pub const MAX_EXTRA_HOTKEYS: usize = 8;

impl Default for Settings {
    fn default() -> Self {
        Self {
            clip_seconds: 30,
            long_clip_seconds: 60,
            quality: "720p".into(),
            fps: 60,
            codec: "h264".into(),
            save_folder: default_save_folder(),
            hotkey_save: "F8".into(),
            hotkey_save_long: "Shift+F8".into(),
            extra_hotkeys: Vec::new(),
            hotkey_screenshot: String::new(),
            session_mode: "off".into(),
            hotkey_session: String::new(),
            push_to_talk: false,
            push_to_talk_key: String::new(),
            noise_suppression: false,
            mic_gate_db: -100,
            // Off by default: a key that silently stops recording is easy to hit by accident.
            hotkey_pause: String::new(),
            games: vec!["RobloxPlayerBeta.exe".into()],
            record_fullscreen_apps: true,
            never_record: Vec::new(),
            record_target: String::new(),
            capture: "window".into(),
            capture_monitor: String::new(),
            encoder_gpu: String::new(),
            check_updates: true,
            skipped_version: String::new(),
            copy_after_save: true,
            efficiency_mode: true,
            audio_everything: true,
            audio_game: true,
            audio_discord: true,
            audio_mic: true,
            audio_kbps: 160,
            audio_mode: "main".into(),
            record_stems: true,
            frame_timing: "smooth".into(),
            clips_max_gb: 0,
            volume_everything: 100,
            volume_game: 100,
            volume_discord: 100,
            volume_mic: 100,
            mic_device: String::new(),
            output_device: String::new(),
            notify_on_save: true,
            sound_on_save: true,
            notify_on_start: true,
            start_with_windows: false,
            tray_recording_dot: true,
            voice_enabled: false,
            voice_phrases: vec![
                VoicePhrase {
                    text: "Archer clip that".into(),
                    seconds: 30,
                },
                VoicePhrase {
                    text: "Archer clip the last minute".into(),
                    seconds: 60,
                },
            ],
            sound_on_voice: true,
            sound_on_hotkey: true,
            voice_sensitivity: 50,
            clip_sound_volume: 100,
            voice_sound_volume: 100,
            voice_tuning: None,
            voice_fine_tune: 0,
            voice_test: None,
            theme: "dark".into(),
            accent: "blue".into(),
            density: "comfortable".into(),
            reduce_motion: false,
            onboarded: false,
        }
    }
}

/// Exists while the voice calibration runs, so the tray doesn't clip each
/// practice phrase.
pub fn calibration_marker() -> PathBuf {
    app_dir().join("voice-calibrating")
}

pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Archy")
}

fn default_save_folder() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Videos").join("Archy"))
        .unwrap_or_else(|| app_dir().join("Clips"))
}

pub fn path() -> PathBuf {
    app_dir().join("settings.json")
}

impl Settings {
    /// Loads settings, creating the file with defaults on first run. A file that
    /// fails to parse is left untouched (the user may be mid-edit) and defaults are used.
    pub fn load() -> (Self, Option<String>) {
        let path = path();
        match std::fs::read_to_string(&path) {
            // Some editors save UTF-8 with a byte-order mark, which JSON parsers reject.
            Ok(text) => match serde_json::from_str::<Settings>(text.trim_start_matches('\u{feff}')) {
                Ok(s) => (s.clamped(), None),
                Err(e) => (
                    Self::default(),
                    Some(format!("settings.json has an error ({e}); using defaults")),
                ),
            },
            Err(_) => {
                let s = Self::default();
                let _ = std::fs::create_dir_all(app_dir());
                let _ = std::fs::write(&path, serde_json::to_string_pretty(&s).unwrap_or_default());
                (s, None)
            }
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(app_dir())?;
        let text = serde_json::to_string_pretty(&self.clone().clamped())?;
        let tmp = path().with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path())
    }

    /// When the settings file last changed (to pick up edits made elsewhere).
    pub fn modified() -> Option<std::time::SystemTime> {
        std::fs::metadata(path()).and_then(|m| m.modified()).ok()
    }

    fn clamped(mut self) -> Self {
        for p in &mut self.voice_phrases {
            p.seconds = p.seconds.clamp(5, MAX_CLIP_SECONDS);
            // The voice model only knows real words: "Archy" was never heard, while
            // "Archer" (said almost the same) is. Old default phrases move over.
            p.text = p
                .text
                .split_whitespace()
                .map(|w| match w.to_ascii_lowercase().trim_matches(|c: char| !c.is_alphanumeric()) {
                    "archy" | "archie" | "archi" => "Archer",
                    _ => w,
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
        self.voice_phrases.retain(|p| !p.text.trim().is_empty());
        if self.voice_sensitivity == 0 && self.voice_tuning.is_none() {
            self.voice_sensitivity = 50;
        }
        self.clip_seconds = self.clip_seconds.clamp(5, MAX_CLIP_SECONDS);
        self.long_clip_seconds = self.long_clip_seconds.clamp(5, MAX_CLIP_SECONDS);
        self.extra_hotkeys.truncate(MAX_EXTRA_HOTKEYS);
        for h in &mut self.extra_hotkeys {
            h.seconds = h.seconds.clamp(5, MAX_CLIP_SECONDS);
        }
        self.fps = self.fps.clamp(24, 144);
        self.clip_sound_volume = self.clip_sound_volume.min(200);
        for v in [
            &mut self.volume_everything,
            &mut self.volume_game,
            &mut self.volume_discord,
            &mut self.volume_mic,
        ] {
            *v = (*v).min(200);
        }
        self.voice_sound_volume = self.voice_sound_volume.min(200);
        self.voice_fine_tune = self.voice_fine_tune.clamp(-50, 50);
        // Unknown look values (a hand edit, a newer Archy) fall back to the defaults.
        if self.audio_mode != "separate" {
            self.audio_mode = "main".into();
        }
        if !["dark", "light", "system"].contains(&self.theme.as_str()) {
            self.theme = "dark".into();
        }
        if !["blue", "violet", "teal", "orange"].contains(&self.accent.as_str()) {
            self.accent = "blue".into();
        }
        if !["comfortable", "compact"].contains(&self.density.as_str()) {
            self.density = "comfortable".into();
        }
        self
    }

    /// Whether the microphone is heard in the clip at all (mixed into track 1).
    /// In "separate" mode a mic at 0% is never opened.
    pub fn mic_wanted(&self) -> bool {
        self.audio_mode != "separate" || self.volume_mic > 0
    }

    /// Which sources get their own track (game, Discord, mic). Everything always
    /// has track 1. In "separate" mode a source at 0% isn't recorded at all
    /// (a mic at 0% is never opened); in "main" mode all are kept for the editor
    /// unless `record_stems` is off.
    pub fn audio_sources(&self) -> (bool, bool, bool) {
        if self.audio_mode == "separate" {
            (self.volume_game > 0, self.volume_discord > 0, self.volume_mic > 0)
        } else if self.record_stems {
            (true, true, true)
        } else {
            (false, false, false)
        }
    }

    /// Live volumes (everything, game, Discord, mic) for the current mode: in
    /// "main" mode only the main volume applies; in "separate" mode track 1 stays
    /// at 100% and each source has its own.
    pub fn audio_volumes(&self) -> (u32, u32, u32, u32) {
        if self.audio_mode == "separate" {
            (100, self.volume_game, self.volume_discord, self.volume_mic)
        } else {
            (self.volume_everything, 100, 100, 100)
        }
    }

    /// The replay buffer has to hold the longest clip either hotkey can ask for.
    /// Voice phrases count even while voice is off: switching voice on must not
    /// resize the buffer (a resize restarts the recording and empties it).
    pub fn buffer_seconds(&self) -> u32 {
        let voice = self.voice_phrases.iter().map(|p| p.seconds).max().unwrap_or(0);
        // An extra hotkey that's turned off (no key) doesn't make the buffer longer.
        let extra = self.extra_hotkeys.iter().filter(|h| !h.key.trim().is_empty()).map(|h| h.seconds).max().unwrap_or(0);
        self.clip_seconds.max(self.long_clip_seconds).max(voice).max(extra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_volume_records_every_source_and_only_the_main_volume_counts() {
        let s = Settings { volume_everything: 70, volume_game: 0, volume_discord: 30, volume_mic: 0, ..Default::default() };
        assert_eq!(s.audio_mode, "main");
        assert_eq!(s.audio_sources(), (true, true, true), "kept for the editor");
        assert_eq!(s.audio_volumes(), (70, 100, 100, 100));
    }

    #[test]
    fn each_source_uses_its_own_volume_and_zero_means_not_recorded() {
        let s = Settings {
            audio_mode: "separate".into(),
            volume_everything: 30,
            volume_game: 120,
            volume_discord: 0,
            volume_mic: 80,
            ..Default::default()
        };
        assert_eq!(s.audio_sources(), (true, false, true));
        assert_eq!(s.audio_volumes(), (100, 120, 0, 80));
        let quiet_mic = Settings { volume_mic: 0, ..s };
        assert!(!quiet_mic.audio_sources().2, "a mic at 0% is never opened");
    }

    #[test]
    fn unknown_values_fall_back() {
        let s = Settings { audio_mode: "weird".into(), theme: "neon".into(), accent: "pink".into(), ..Default::default() }.clamped();
        assert_eq!((s.audio_mode.as_str(), s.theme.as_str(), s.accent.as_str()), ("main", "dark", "blue"));
    }

    #[test]
    fn a_settings_file_with_a_byte_order_mark_is_read() {
        let text = format!("\u{feff}{}", serde_json::to_string(&Settings { fps: 144, ..Default::default() }).unwrap());
        let s: Settings = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap();
        assert_eq!(s.fps, 144);
    }

    #[test]
    fn longer_clips_and_extra_hotkeys() {
        // Off (the default): exactly the old buffer.
        let s = Settings::default();
        assert!(s.extra_hotkeys.is_empty());
        assert_eq!(s.buffer_seconds(), 60);
        // Up to 5 minutes, never more.
        let s: Settings = serde_json::from_str(r#"{"clip_seconds": 900, "extra_hotkeys": [{"key": "Ctrl+F8", "seconds": 120}]}"#).unwrap();
        let s = s.clamped();
        assert_eq!(s.clip_seconds, 300);
        assert_eq!(s.buffer_seconds(), 300);
        // An extra hotkey with no key doesn't grow the buffer.
        let s: Settings = serde_json::from_str(r#"{"extra_hotkeys": [{"key": "", "seconds": 300}]}"#).unwrap();
        assert_eq!(s.clamped().buffer_seconds(), 60);
        // At most 8 extra hotkeys.
        let many = (0..20).map(|i| format!(r#"{{"key":"Ctrl+F{}","seconds":30}}"#, i % 12 + 1)).collect::<Vec<_>>().join(",");
        let s: Settings = serde_json::from_str(&format!(r#"{{"extra_hotkeys":[{many}]}}"#)).unwrap();
        assert_eq!(s.clamped().extra_hotkeys.len(), MAX_EXTRA_HOTKEYS);
    }
}
