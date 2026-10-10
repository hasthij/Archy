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
    /// 24-90.
    pub fps: u32,
    /// "h264" (plays everywhere, incl. Discord previews) | "hevc" | "av1" | "auto" (best available).
    pub codec: String,
    pub save_folder: PathBuf,
    pub hotkey_save: String,
    pub hotkey_save_long: String,
    pub hotkey_pause: String,
    /// Executable names that always count as games.
    pub games: Vec<String>,
    /// Also record any other app running fullscreen.
    pub record_fullscreen_apps: bool,
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
    /// Volume of the clip-saved chime, 0-200 (%).
    pub clip_sound_volume: u32,
    /// Volume of the phrase-heard blip, 0-200 (%).
    pub voice_sound_volume: u32,
    /// How easily a phrase triggers: 25 strict, 50 balanced, 80 loose, or 0 =
    /// use `voice_tuning` from the user's own calibration.
    pub voice_sensitivity: u32,
    /// Detector settings found by calibrating with the user's voice and mic.
    pub voice_tuning: Option<VoiceTuning>,
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
            // Off by default: a key that silently stops recording is easy to hit by accident.
            hotkey_pause: String::new(),
            games: vec!["RobloxPlayerBeta.exe".into()],
            record_fullscreen_apps: true,
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
            voice_sensitivity: 50,
            clip_sound_volume: 100,
            voice_sound_volume: 100,
            voice_tuning: None,
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
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
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
            p.seconds = p.seconds.clamp(5, 60);
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
        self.clip_seconds = self.clip_seconds.clamp(5, 60);
        self.long_clip_seconds = self.long_clip_seconds.clamp(5, 60);
        self.fps = self.fps.clamp(24, 90);
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
        self
    }

    /// The replay buffer has to hold the longest clip either hotkey can ask for.
    pub fn buffer_seconds(&self) -> u32 {
        let voice = if self.voice_enabled {
            self.voice_phrases
                .iter()
                .map(|p| p.seconds)
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        self.clip_seconds.max(self.long_clip_seconds).max(voice)
    }
}
