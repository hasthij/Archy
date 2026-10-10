<p align="center">
  <img src="assets/archy-128.png" width="96" alt="Archy logo">
</p>

<h1 align="center">Archy</h1>

<p align="center">
  <b>Clip the last 30 seconds of your game with a key or your voice.</b><br>
  A free, local-first alternative to Medal for Windows. No account, no uploads, almost no FPS cost.
</p>

<p align="center">
  <a href="https://github.com/hasthij/Archy/releases/latest"><b>⬇ Download Archy-Setup.exe</b></a>
</p>

---

## What it does

- **Always ready.** While a game is open, Archy keeps the last moments in memory. Press **F8**, or say **"Archer, clip that"**, and those seconds are saved as an MP4.
- **Barely touches your FPS.** It uses your graphics chip's video encoder (Intel Quick Sync, NVIDIA or AMD), stays off the cores your game uses, and records nothing when no game is running.
- **Separate audio tracks.** You get everything you heard with your mic mixed in, plus the game alone, Discord alone and your mic alone, so you can edit later.
- **Discord-ready.** Each new clip is copied, so you can paste it straight into Discord. Clips over 10 MB get a one-click **Discord copy** that fits the free upload limit.
- **Yours only.** Clips, settings and voice listening all stay on your PC. There is no account and no server, and nothing is ever uploaded.

## Install

1. Download **Archy-Setup.exe** from the [latest release](https://github.com/hasthij/Archy/releases/latest).
2. Run it. It installs just for you, so it doesn't need admin rights.
   - **Windows SmartScreen may warn you** because the installer isn't code-signed yet. Click **More info**, then **Run anyway**.
3. Setup downloads the voice engine, about 24 MB. It can look paused for up to 30 seconds while this happens.
4. If Windows is missing something Archy needs, Setup tells you and opens the right page. You make the change yourself; **Archy never changes Windows settings on its own.** It checks for:
   - microphone access for desktop apps,
   - Microsoft Edge WebView2, which the settings window uses (built into Windows 11),
   - the Microsoft Visual C++ runtime, which voice clipping uses.
5. Archy starts in your **system tray**, bottom-right next to the clock. Look for the blue **A**. A red dot on the icon means it's recording a game.

**Requirements:** 64-bit Windows 10 (version 2004 or newer) or Windows 11.

## First-time setup (2 minutes)

Open Archy from the Start menu or by clicking the tray icon, then go through these steps.

### 1. Pick your microphone
**Audio → Microphone.** Choose the mic you actually talk into. Archy records your voice from it, and voice clipping listens to it too.

### 2. Calibrate voice clipping ⭐ do this one
**Audio → Voice clipping → Calibrate to your voice → Calibrate.**

Everyone says things differently, and calibration teaches Archy how **you** say your phrase. It takes about a minute:

1. When Archy shows **"Say: Archer clip that"**, say it **the way you would mid-game**: all in one go, at normal speed, no careful pauses. It asks 5 times.
2. When it shows **"Now just talk normally"**, chat or play for 15 seconds **without** saying the phrase. This is how Archy checks that it won't clip by accident.
3. Wait about 30 seconds while it learns. It then shows something like **"Caught 5 of 5"** and switches Sensitivity to **Calibrated**.

You can **Stop** or **Restart** at any time. Calibrate again if you change your mic, your phrase, or where you sit. The recording never leaves your PC and is thrown away afterwards.

> Calibrate when you're not in the middle of a match. The learning step uses a few CPU cores for about 30 seconds.

### 3. Add your games
**Games.** Roblox is already in the list. Use the running-apps picker to add anything else that's open. **Other fullscreen apps** (on by default) also records most other games running fullscreen.

### 4. Check your clips folder
**Storage.** Clips go to `Videos\Archy` by default. Click to choose a different folder.

That's it. Open a game and press **F8**.

## Saving clips

| How | What gets saved |
|---|---|
| **F8** | the last 30 seconds |
| **Shift + F8** | the last 60 seconds |
| **"Archer clip that"** | the last 30 seconds |
| **"Archer clip the last minute"** | the last 60 seconds |
| Tray menu | pause or resume recording |

You can change every hotkey, every phrase and the clip lengths in Archy.

### Voice tips
- **Use real, everyday words.** The voice model knows real English words, so made-up names and gamer tags can't be recognized. That's why the default is "**Archer**" and not "Archy".
- **3–4 words you wouldn't say by accident** work best.
- **Changed a phrase? Calibrate again.** What Archy learned belongs to the phrase you calibrated.
- **Missing you?** Calibrate again, or try **Loose** sensitivity. **Clipping by accident?** Try **Strict**.

## Settings at a glance

Each setting in Archy explains what it's best for. The main ones:

- **Quality:** Discord (10 MB), 360p up to 1440p. **720p** is the recommended balance.
- **Frame rate:** 24 to 90 fps. **60** is recommended; lower is less work for weak PCs.
- **Capture:** *Window* (only the game, popups never appear) or *Screen* (whole monitor, for games that can't be captured as a window).
- **Audio tracks:** turn each track on or off, pick the speakers or headphones to record from, and set the audio bitrate.
- **Efficiency mode:** keeps Archy on efficiency cores at low priority, so your game keeps the fast cores. On by default.
- **Notifications and sounds:** a chime when a clip saves, a blip when a voice phrase is heard, and notifications for each.
- **Start with Windows:** so clips are always ready.

## Troubleshooting

**Voice clipping doesn't react**
1. Calibrate. That fixes most cases.
2. Check **Audio → Microphone** is the mic you talk into.
3. Check Windows mic access: **Settings → Privacy & security → Microphone**. Turn on *Microphone access* and *Let desktop apps access your microphone*.
4. If Archy says the voice files are missing, run Archy-Setup.exe again to re-download them.

**No tray icon after installing**
Setup tells you if Archy didn't start. Antivirus is the usual cause: allow `archy.exe` in `%LOCALAPPDATA%\Programs\Archy`, then open Archy from the Start menu.

**My voice isn't in the clip**
Turn on **Audio → Your microphone** and check the right mic is picked. Your voice is mixed into track 1, so every player hears it.

**The clip is too big for Discord**
Click **Discord copy** on the clip in Archy, or set **Quality → Discord (10 MB)** so new clips always fit.

**Something else**
Archy writes a log to `%APPDATA%\Archy\archy.log`. Please include it when you [open an issue](https://github.com/hasthij/Archy/issues).

## Uninstall

**Settings → Apps → Installed apps → Archy → Uninstall.** Your clips and settings are kept.

## Privacy

Archy works entirely on your PC.
- There's no account, no server, no telemetry and no analytics.
- Voice clipping uses a small on-device speech model that listens only for your phrases. Audio is never stored or sent.
- The only time Archy goes online is during install, to download the voice engine from the [sherpa-onnx GitHub releases](https://github.com/k2-fsa/sherpa-onnx/releases). Each file is checked against a fixed SHA-256 fingerprint before use.

## Building from source

You need Rust (stable, MSVC toolchain) on Windows 10 or 11.

```bash
cargo build --release -p archy -p archy-ui
```

```bash
cargo build --release -p archy-setup
```

The first command builds the tray recorder and the settings window. The second packs both into `target/release/Archy-Setup.exe`, so build them first. For development, voice clipping looks for the engine in `tools/voice/` (see `crates/archy-setup/src/voice.rs` for the exact files).

| Crate | What it is |
|---|---|
| `archy` | the tray recorder: game detection, replay buffer, hotkeys, voice helper |
| `archy-ui` | the settings window and clip library (Tauri) |
| `archy-core` | recorder, settings, Discord copy |
| `archy-capture` / `archy-gpu` / `archy-encode` / `archy-mux` | screen capture, GPU colour conversion, hardware encoding, MP4 writing |
| `archy-audio` | WASAPI capture (system, per-app, mic, mixed) and AAC encoding |
| `archy-voice` | on-device keyword spotting and calibration |
| `archy-setup` | the installer |

## Credits

- Voice clipping uses [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) and its gigaspeech keyword-spotting model (Apache-2.0), running on [ONNX Runtime](https://onnxruntime.ai/) (MIT).
- The window is built with [Tauri](https://tauri.app/).

## License

[GPL-3.0](LICENSE)
