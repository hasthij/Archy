//! Which window is the game.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
};

pub struct Foreground {
    pub hwnd: HWND,
    /// Executable file name, e.g. "RobloxPlayerBeta.exe".
    pub exe: String,
    pub pid: u32,
    pub fullscreen: bool,
}

/// Desktop/shell windows and apps that are often fullscreen but aren't games.
const NOT_GAMES: &[&str] = &[
    "explorer.exe",
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "opera.exe",
    "brave.exe",
    "vlc.exe",
    "Discord.exe",
    "Spotify.exe",
    "Archy.exe",
    // Archy's own window: maximized, it looked like a fullscreen game.
    "archy-ui.exe",
    "ApplicationFrameHost.exe",
    "SearchHost.exe",
    "StartMenuExperienceHost.exe",
    "LockApp.exe",
];
const SHELL_CLASSES: &[&str] = &["Progman", "WorkerW", "Shell_TrayWnd"];

pub fn foreground() -> Option<Foreground> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() || unsafe { IsIconic(hwnd) }.as_bool() {
        return None;
    }
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    let class = String::from_utf16_lossy(&class[..n]);
    if SHELL_CLASSES.contains(&class.as_str()) {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let exe = exe_name(hwnd)?;
    Some(Foreground {
        hwnd,
        pid,
        fullscreen: covers_monitor(hwnd),
        exe,
    })
}

impl Foreground {
    /// `never`: apps the user excluded; they win over everything else.
    pub fn is_game(&self, games: &[String], any_fullscreen: bool, never: &[String]) -> bool {
        if never.iter().any(|g| g.eq_ignore_ascii_case(&self.exe)) {
            return false;
        }
        if games.iter().any(|g| g.eq_ignore_ascii_case(&self.exe)) {
            return true;
        }
        any_fullscreen
            && self.fullscreen
            && !NOT_GAMES.iter().any(|n| n.eq_ignore_ascii_case(&self.exe))
    }
}

pub fn alive(hwnd: HWND) -> bool {
    unsafe { IsWindow(Some(hwnd)) }.as_bool()
}

fn exe_name(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    archy_core::process::exe_name(pid)
}

fn covers_monitor(hwnd: HWND) -> bool {
    let mut win = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut win) }.is_err() {
        return false;
    }
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return false;
    }
    let m = info.rcMonitor;
    win.left <= m.left && win.top <= m.top && win.right >= m.right && win.bottom >= m.bottom
}

/// Friendly name for clip file names: "RobloxPlayerBeta.exe" -> "Roblox".
pub fn game_name(exe: &str) -> String {
    let stem = exe
        .strip_suffix(".exe")
        .or_else(|| exe.strip_suffix(".EXE"))
        .unwrap_or(exe);
    match stem.to_ascii_lowercase().as_str() {
        "robloxplayerbeta" => "Roblox".into(),
        "valorant-win64-shipping" => "Valorant".into(),
        "fortniteclient-win64-shipping" => "Fortnite".into(),
        "cs2" => "Counter-Strike 2".into(),
        "javaw" => "Minecraft".into(),
        _ => stem.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_record_wins() {
        let fg = Foreground { hwnd: HWND::default(), exe: "Game.exe".into(), pid: 1, fullscreen: true };
        let games = vec!["game.exe".to_string()];
        assert!(fg.is_game(&games, true, &[]));
        assert!(!fg.is_game(&games, true, &["GAME.EXE".to_string()]));
        assert!(fg.is_game(&[], true, &[]), "fullscreen apps count");
        assert!(!fg.is_game(&[], true, &["game.exe".to_string()]));
    }

    #[test]
    fn friendly_names() {
        assert_eq!(game_name("RobloxPlayerBeta.exe"), "Roblox");
        assert_eq!(game_name("SomeGame.exe"), "SomeGame");
    }
}
