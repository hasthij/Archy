//! The Archy window: clip library and every setting, with plain explanations.
//!
//! It only runs while open; recording lives in the tray app (archy.exe), which
//! picks up saved settings within two seconds. The two talk through files in
//! %APPDATA%\Archy (settings.json, status.json), so there is no server or socket.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod editor;

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use archy_core::presets::PRESETS;
use archy_core::settings::Settings;
use archy_core::status::Status;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows::core::{w, PCWSTR};
use windows::core::BOOL;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, STILL_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Threading::{
    CreateMutexW, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SHFileOperationW,
    FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FO_DELETE,
    SHFILEOPSTRUCTW, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow, ShowWindow, GW_OWNER,
    SW_RESTORE,
};

#[derive(Serialize)]
struct Clip {
    path: String,
    name: String,
    bytes: u64,
    /// Milliseconds since 1970, for sorting and "Today 21:14" labels.
    modified: u64,
}

#[derive(Serialize)]
struct StatusView {
    /// Whether the tray recorder is running at all.
    running: bool,
    state: String,
    game: String,
    detail: String,
}

#[derive(Serialize)]
struct AppView {
    /// Window title, e.g. "Roblox".
    title: String,
    /// Program file name, e.g. "RobloxPlayerBeta.exe".
    exe: String,
}

#[derive(Serialize)]
struct DeviceView {
    id: String,
    name: String,
}

#[derive(Serialize)]
struct PresetView {
    name: &'static str,
    height: u32,
    mbps: f32,
}

fn process_alive(pid: u32) -> bool {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(h);
        ok
    }
}

/// Lets the page play videos from the clips folder (and nothing else on disk).
fn allow_folder(app: &AppHandle, folder: &Path) {
    let _ = app.asset_protocol_scope().allow_directory(folder, false);
}

#[tauri::command]
fn get_settings() -> Settings {
    Settings::load().0
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    settings.save().map_err(|e| e.to_string())?;
    let saved = Settings::load().0;
    allow_folder(&app, &saved.save_folder);
    Ok(saved)
}

#[tauri::command]
fn presets() -> Vec<PresetView> {
    PRESETS
        .iter()
        .map(|p| PresetView {
            name: p.name,
            height: p.height,
            mbps: p.mbps,
        })
        .collect()
}

#[tauri::command]
fn get_status() -> StatusView {
    match Status::read() {
        Some(s) if process_alive(s.pid) => StatusView {
            running: true,
            state: s.state,
            game: s.game,
            detail: s.detail,
        },
        _ => StatusView {
            running: false,
            state: "stopped".into(),
            game: String::new(),
            detail: String::new(),
        },
    }
}

#[tauri::command]
fn list_clips() -> Vec<Clip> {
    let folder = Settings::load().0.save_folder;
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut clips: Vec<Clip> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            // Skip clips still being written.
            if !name.to_ascii_lowercase().ends_with(".mp4") || name.contains(".partial.") {
                return None;
            }
            let meta = e.metadata().ok()?;
            let modified = meta
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some(Clip {
                path: path.to_string_lossy().into_owned(),
                name: name.trim_end_matches(".mp4").to_string(),
                bytes: meta.len(),
                modified,
            })
        })
        .collect();
    clips.sort_by(|a, b| b.modified.cmp(&a.modified));
    clips
}

/// Only paths inside the clips folder are accepted from the page.
fn clip_path(path: &str) -> Result<PathBuf, String> {
    let folder = Settings::load().0.save_folder;
    let p = PathBuf::from(path);
    let inside = p
        .canonicalize()
        .ok()
        .zip(folder.canonicalize().ok())
        .is_some_and(|(p, f)| p.starts_with(f));
    if inside {
        Ok(p)
    } else {
        Err("not a clip in the clips folder".into())
    }
}

#[tauri::command]
fn copy_clip(path: String) -> Result<(), String> {
    let p = clip_path(&path)?;
    if archy_core::clipboard::copy_file(&p) {
        Ok(())
    } else {
        Err("could not use the clipboard".into())
    }
}

#[tauri::command]
fn reveal_clip(path: String) -> Result<(), String> {
    let p = clip_path(&path)?;
    std::process::Command::new("explorer")
        .arg(format!("/select,{}", p.display()))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Moves a clip to the Recycle Bin (recoverable), never deletes it outright.
#[tauri::command]
fn recycle_clip(path: String) -> Result<(), String> {
    let p = clip_path(&path)?;
    let from: Vec<u16> = p
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0, 0])
        .collect();
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r == 0 {
        Ok(())
    } else {
        Err(format!("could not move to Recycle Bin (error {r})"))
    }
}

#[tauri::command]
fn open_clips_folder() -> Result<(), String> {
    let folder = Settings::load().0.save_folder;
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(&folder)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The last `lines` lines of Archy's log (%APPDATA%\Archy\archy.log), newest last.
#[tauri::command]
fn read_log(lines: usize) -> String {
    let path = archy_core::settings::app_dir().join("archy.log");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines.clamp(1, 2000))..].join("\n")
}

#[derive(Serialize)]
struct GpuView {
    id: String,
    name: String,
    /// Codecs its hardware encoder can make, best first; empty = can't record.
    encoders: Vec<String>,
    integrated: bool,
}

/// GPUs and their video encoders, for the GPU picker.
#[tauri::command]
async fn gpus() -> Vec<GpuView> {
    archy_gpu::probe::probe()
        .unwrap_or_default()
        .into_iter()
        .map(|a| {
            let mut encoders: Vec<String> = Vec::new();
            for e in &a.encoders {
                let c = e.codec.to_string();
                if !encoders.contains(&c) {
                    encoders.push(c);
                }
            }
            GpuView { id: a.id, name: a.name, encoders, integrated: a.integrated }
        })
        .collect()
}

/// The update the banner offered, for "Update".
static OFFERED: std::sync::Mutex<Option<archy_core::update::Update>> = std::sync::Mutex::new(None);

/// A newer Archy on GitHub, unless checking is off or the user skipped it.
#[tauri::command]
async fn check_update() -> Option<archy_core::update::Update> {
    let s = Settings::load().0;
    if !s.check_updates {
        return None;
    }
    let update = tauri::async_runtime::spawn_blocking(|| {
        archy_core::update::check(env!("CARGO_PKG_VERSION")).ok().flatten()
    })
    .await
    .ok()
    .flatten()?;
    if update.version == s.skipped_version {
        return None;
    }
    if let Ok(mut o) = OFFERED.lock() {
        *o = Some(update.clone());
    }
    Some(update)
}

/// Downloads and verifies the offered installer, then runs it. The installer
/// asks before changing anything and closes Archy itself.
#[tauri::command]
async fn install_update() -> Result<(), String> {
    let update = OFFERED.lock().ok().and_then(|o| o.clone()).ok_or("no update offered")?;
    let path = tauri::async_runtime::spawn_blocking(move || archy_core::update::download(&update))
        .await
        .map_err(|e| e.to_string())??;
    std::process::Command::new(&path).spawn().map_err(|e| format!("couldn't start the installer: {e}"))?;
    Ok(())
}

/// Opens a release page in the browser (only this project's GitHub pages).
#[tauri::command]
fn open_release_page(url: String) -> Result<(), String> {
    if !url.starts_with("https://github.com/hasthij/Archy/") {
        return Err("not an Archy release page".into());
    }
    std::process::Command::new("explorer").arg(&url).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Connected screens, for the screen picker.
#[tauri::command]
fn monitors() -> Vec<archy_core::monitors::Monitor> {
    archy_core::monitors::list()
}

/// Opens the folder with archy.log and settings.json, with the log selected.
#[tauri::command]
fn open_log_folder() -> Result<(), String> {
    let dir = archy_core::settings::app_dir();
    let log = dir.join("archy.log");
    let mut cmd = std::process::Command::new("explorer");
    if log.exists() {
        cmd.arg(format!("/select,{}", log.display()));
    } else {
        cmd.arg(&dir);
    }
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Windows' own folder picker, owned by the Archy window. `None` if cancelled.
#[tauri::command]
async fn pick_folder(window: tauri::WebviewWindow) -> Option<String> {
    let owner = window.hwnd().ok().map(|h| h.0 as isize);
    // The dialog needs a single-threaded COM apartment of its own.
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        let _ = dialog.SetTitle(w!("Choose where Archy saves clips"));
        let current = Settings::load().0.save_folder;
        let start: Vec<u16> = current
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain([0])
            .collect();
        if let Ok(item) =
            SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(start.as_ptr()), None)
        {
            let _ = dialog.SetFolder(&item);
        }
        dialog.Show(owner.map(|h| HWND(h as *mut _))).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    })
    .join()
    .ok()
    .flatten()
}

/// Re-encodes a clip to fit Discord's free 10 MB upload limit and copies the
/// result to the clipboard. Returns the new file's path.
#[tauri::command]
async fn discord_copy(path: String) -> Result<String, String> {
    let src = clip_path(&path)?;
    let dst = archy_core::discord::copy_path(&src);
    let info = tauri::async_runtime::spawn_blocking(move || archy_core::discord::make_copy(&src, &dst, 10_000_000))
        .await
        .map_err(|_| "Discord copy crashed".to_string())?
        .map_err(|e| e.message())?;
    archy_core::clipboard::copy_file(&info.path);
    Ok(info.path.to_string_lossy().into_owned())
}

/// Length, size and audio track names of a clip, for the editor.
#[tauri::command]
async fn clip_info(path: String) -> Result<editor::ClipInfo, String> {
    let src = clip_path(&path)?;
    tauri::async_runtime::spawn_blocking(move || editor::info(&src)).await.map_err(|_| "couldn't read the clip".to_string())?
}

/// Each audio track as its own file, for the editor's live volume preview.
#[tauri::command]
async fn editor_tracks(app: AppHandle, path: String, count: usize) -> Result<Vec<String>, String> {
    let src = clip_path(&path)?;
    let tracks = tauri::async_runtime::spawn_blocking(move || editor::preview_tracks(&src, count))
        .await
        .map_err(|_| "couldn't prepare the audio tracks".to_string())??;
    if let Some(dir) = tracks.first().and_then(|t| Path::new(t).parent()) {
        allow_folder(&app, dir);
    }
    Ok(tracks)
}

/// Whether the editor's video tools (FFmpeg) are installed.
#[tauri::command]
fn editor_ready() -> bool {
    editor::tools_dir().is_some()
}

/// Where the user chose to save the next export ("Save as" dialog). Exports are
/// only ever written there or to temp, never to a path the page makes up.
static EXPORT_TARGET: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Asks where to save an export ("Save as", starting in the clips folder with a
/// suggested name). `None` if the user cancels.
#[tauri::command]
async fn choose_export_path(window: tauri::WebviewWindow, path: String, format: String) -> Option<String> {
    let src = clip_path(&path).ok()?;
    let owner = window.hwnd().ok().map(|h| h.0 as isize);
    let gif = format == "gif";
    let stem = src.file_stem()?.to_string_lossy().into_owned();
    let folder = src.parent()?.to_path_buf();
    let chosen = tauri::async_runtime::spawn_blocking(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let wide = |t: &str| t.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (label, pattern, ext) = if gif { ("GIF", "*.gif", "gif") } else { ("MP4 video", "*.mp4", "mp4") };
        let (label, pattern, ext) = (wide(label), wide(pattern), wide(ext));
        let types = [windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC {
            pszName: PCWSTR(label.as_ptr()),
            pszSpec: PCWSTR(pattern.as_ptr()),
        }];
        let _ = dialog.SetFileTypes(&types);
        let _ = dialog.SetDefaultExtension(PCWSTR(ext.as_ptr()));
        let name = wide(&format!("{stem} ({}).{}", if gif { "GIF" } else { "edit" }, if gif { "gif" } else { "mp4" }));
        let _ = dialog.SetFileName(PCWSTR(name.as_ptr()));
        let _ = dialog.SetTitle(w!("Save the export as"));
        let start = wide(&folder.to_string_lossy());
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(start.as_ptr()), None) {
            let _ = dialog.SetFolder(&item);
        }
        dialog.Show(owner.map(|h| HWND(h as *mut _))).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    })
    .await
    .ok()
    .flatten()?;
    if let Ok(mut t) = EXPORT_TARGET.lock() {
        *t = Some(PathBuf::from(&chosen));
    }
    Some(chosen)
}

/// Exports an edit as MP4 or GIF: to the path picked with `choose_export_path`,
/// or copied to the clipboard. Returns the new file's path.
#[tauri::command]
async fn export_clip(app: AppHandle, mut request: editor::ExportRequest) -> Result<String, String> {
    let src = clip_path(&request.path)?;
    if !request.to_clipboard {
        let picked = EXPORT_TARGET.lock().ok().and_then(|mut t| t.take());
        match (&request.save_to, picked) {
            (Some(want), Some(picked)) if Path::new(want) == picked => {}
            _ => return Err("pick where to save it first".into()),
        }
    } else {
        request.save_to = None;
    }
    let to_clipboard = request.to_clipboard;
    let out = tauri::async_runtime::spawn_blocking(move || editor::export(&src, &request, &|p| { let _ = app.emit("export-progress", p); }))
        .await
        .map_err(|_| "the export crashed".to_string())??;
    if to_clipboard && !archy_core::clipboard::copy_file(&out) {
        return Err("exported, but the clipboard couldn't be used".into());
    }
    if !to_clipboard {
        // Show where it went.
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", out.display())).spawn();
    }
    Ok(out.to_string_lossy().into_owned())
}

/// The version of this Archy (shown in the sidebar and General).
#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Stops the running export; nothing is left behind.
#[tauri::command]
fn cancel_export() {
    editor::CANCEL.store(true, std::sync::atomic::Ordering::Release);
}

/// Programs with a visible window right now, for "add from running apps".
#[tauri::command]
fn running_apps() -> Vec<AppView> {
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let apps = &mut *(lparam.0 as *mut Vec<AppView>);
        if !IsWindowVisible(hwnd).as_bool()
            || !GetWindow(hwnd, GW_OWNER).unwrap_or_default().0.is_null()
        {
            return true.into();
        }
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return true.into();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        // Also works for games whose anti-cheat blocks opening their process.
        if let Some(exe) = archy_core::process::exe_name(pid) {
            const SKIP: &[&str] = &[
                "explorer.exe",
                "archy.exe",
                "archy-ui.exe",
                "ApplicationFrameHost.exe",
                "TextInputHost.exe",
                "SystemSettings.exe",
            ];
            if !SKIP.iter().any(|s| s.eq_ignore_ascii_case(&exe))
                && !apps.iter().any(|a| a.exe.eq_ignore_ascii_case(&exe))
            {
                apps.push(AppView { title, exe });
            }
        }
        true.into()
    }
    let mut apps: Vec<AppView> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut apps as *mut _ as isize));
    }
    apps.sort_by_key(|a| a.title.to_lowercase());
    apps
}

/// Microphones (`input`) or output devices for the device pickers.
#[tauri::command]
async fn audio_devices(input: bool) -> Vec<DeviceView> {
    tauri::async_runtime::spawn_blocking(move || archy_audio::list_devices(input))
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|d| DeviceView {
            id: d.id,
            name: d.name,
        })
        .collect()
}

/// Starts the tray recorder if it isn't running.
#[tauri::command]
fn start_recorder() -> Result<(), String> {
    let mut exe = std::env::current_exe().map_err(|e| e.to_string())?;
    exe.set_file_name("archy.exe");
    std::process::Command::new(&exe)
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(())
}

/// The calibration process while it runs, so Stop can end it.
static CALIBRATION: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// Stops a running voice calibration (nothing is saved).
#[tauri::command]
fn stop_calibration() {
    if let Ok(mut c) = CALIBRATION.lock() {
        if let Some(child) = c.as_mut() {
            let _ = child.kill();
        }
    }
    // The tray ignores phrases while this marker exists; a killed run can't remove it.
    let _ = std::fs::remove_file(archy_core::settings::calibration_marker());
}

/// Runs the voice calibration (`archy.exe --voice-calibrate`, see the tray's
/// voice.rs), passing each progress line to the page as it happens. The voice
/// engine runs in that process, never in this window.
#[tauri::command]
async fn calibrate_voice(
    phrase: String,
    mic: String,
    progress: tauri::ipc::Channel<serde_json::Value>,
) -> Result<(), String> {
    let mic = if mic.is_empty() { "-".to_string() } else { mic };
    voice_child(vec!["--voice-calibrate".into(), mic, phrase], progress, "the calibration").await
}

/// Where commands for the running calibration go ("take", "stop", "analyze"...).
static VOICE_INPUT: std::sync::Mutex<Option<std::process::ChildStdin>> = std::sync::Mutex::new(None);

/// Sends one command to the running calibration (see the tray's voice.rs).
#[tauri::command]
fn calibration_input(command: String) -> Result<(), String> {
    use std::io::Write;
    let mut input = VOICE_INPUT.lock().map_err(|e| e.to_string())?;
    let pipe = input.as_mut().ok_or("the calibration isn't running")?;
    writeln!(pipe, "{}", command.trim()).and_then(|_| pipe.flush()).map_err(|_| "the calibration stopped".to_string())
}

/// Runs the voice test (`archy.exe --voice-test`): every phrase the tray's
/// settings would hear is passed to the page until `stop_calibration` ends it.
#[tauri::command]
async fn test_voice(progress: tauri::ipc::Channel<serde_json::Value>) -> Result<(), String> {
    voice_child(vec!["--voice-test".into()], progress, "the voice test").await
}

/// Runs archy.exe with `args` in the CALIBRATION slot (so Stop can end it) and
/// forwards its JSON lines to the page.
async fn voice_child(
    args: Vec<String>,
    progress: tauri::ipc::Channel<serde_json::Value>,
    what: &'static str,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::BufRead;
        use std::os::windows::process::CommandExt;
        let mut exe = std::env::current_exe().map_err(|e| e.to_string())?;
        exe.set_file_name("archy.exe");
        let mut child = std::process::Command::new(&exe)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stdin(std::process::Stdio::piped())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .spawn()
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        let stdout = child.stdout.take().ok_or(format!("no output from {what}"))?;
        if let Ok(mut input) = VOICE_INPUT.lock() {
            *input = child.stdin.take();
        }
        if let Ok(mut c) = CALIBRATION.lock() {
            if let Some(mut old) = c.replace(child) {
                let _ = old.kill();
            }
        }
        let mut finished = false;
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                finished |= matches!(v["step"].as_str(), Some("done" | "error"));
                let _ = progress.send(v);
            }
        }
        let stopped = match CALIBRATION.lock().ok().and_then(|mut c| c.take()) {
            Some(mut child) => !matches!(child.wait(), Ok(s) if s.success()),
            None => true,
        };
        if !finished {
            return Err(if stopped { "stopped".into() } else { format!("{what} stopped unexpectedly (see archy.log)") });
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    unsafe {
        // One window at a time: a second launch just brings the first one forward.
        let _mutex = CreateMutexW(None, true, w!("Local\\ArchyWindow"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(None, w!("Archy")) {
                let _ = ShowWindow(existing, SW_RESTORE);
                let _ = SetForegroundWindow(existing);
            }
            return;
        }
    }
    tauri::Builder::default()
        // Closing Archy mid-export would throw the export away: ask the page to
        // explain instead (it offers Cancel export).
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if editor::BUSY.load(std::sync::atomic::Ordering::Acquire) {
                    api.prevent_close();
                    let _ = window.emit("export-busy", ());
                }
            }
        })
        .setup(|app| {
            allow_folder(app.handle(), &Settings::load().0.save_folder);
            // Opening Archy from the Start menu also makes sure the recorder is running.
            if !get_status().running {
                let _ = start_recorder();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            presets,
            get_status,
            list_clips,
            copy_clip,
            reveal_clip,
            recycle_clip,
            open_clips_folder,
            pick_folder,
            discord_copy,
            clip_info,
            editor_tracks,
            editor_ready,
            export_clip,
            choose_export_path,
            cancel_export,
            app_version,
            audio_devices,
            running_apps,
            start_recorder,
            calibrate_voice,
            test_voice,
            calibration_input,
            stop_calibration,
            read_log,
            open_log_folder,
            monitors,
            gpus,
            check_update,
            install_update,
            open_release_page
        ])
        .run(tauri::generate_context!())
        .expect("Archy window failed to start");
}
