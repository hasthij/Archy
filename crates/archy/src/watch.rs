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
    /// Minimized (a picked game can be; the foreground window never is).
    pub minimized: bool,
}

/// Desktop/shell windows and apps that are often fullscreen but aren't games.
const NOT_GAMES: &[&str] = &[
    "explorer.exe",
    // Browsers
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "opera.exe",
    "opera_gx.exe",
    "brave.exe",
    "vivaldi.exe",
    "arc.exe",
    "zen.exe",
    "librewolf.exe",
    "waterfox.exe",
    "iexplore.exe",
    // Video and music players
    "vlc.exe",
    "mpv.exe",
    "PotPlayerMini64.exe",
    "PotPlayerMini.exe",
    "mpc-hc64.exe",
    "mpc-hc.exe",
    "mpc-be64.exe",
    "wmplayer.exe",
    "Microsoft.Media.Player.exe",
    "Video.UI.exe",
    "Spotify.exe",
    "iTunes.exe",
    "Netflix.exe",
    "Plex.exe",
    "Kodi.exe",
    "stremio.exe",
    // Calls, chat, streaming and slides
    "Discord.exe",
    "Teams.exe",
    "ms-teams.exe",
    "Zoom.exe",
    "slack.exe",
    "obs64.exe",
    "obs32.exe",
    "Streamlabs OBS.exe",
    "POWERPNT.EXE",
    "WINWORD.EXE",
    "EXCEL.EXE",
    "Photos.exe",
    "Microsoft.Photos.exe",
    "PhotosApp.exe",
    // Game launchers (their fullscreen modes aren't games)
    "steamwebhelper.exe",
    "steam.exe",
    "EpicGamesLauncher.exe",
    "Battle.net.exe",
    "RiotClientServices.exe",
    "EADesktop.exe",
    // Chat, AI and work apps (recorded by mistake when maximized)
    "Telegram.exe",
    "WhatsApp.exe",
    "Signal.exe",
    "Messenger.exe",
    "Skype.exe",
    "claude.exe",
    "ChatGPT.exe",
    "Code.exe",
    "Cursor.exe",
    "devenv.exe",
    "Notion.exe",
    "Figma.exe",
    "Obsidian.exe",
    "OUTLOOK.EXE",
    "olk.exe",
    "thunderbird.exe",
    "notepad.exe",
    "Notepad++.exe",
    "WindowsTerminal.exe",
    "AcroRd32.exe",
    "Acrobat.exe",
    // Windows itself
    "TextInputHost.exe",
    "ShellExperienceHost.exe",
    "ScreenClippingHost.exe",
    "SnippingTool.exe",
    "Magnify.exe",
    "mstsc.exe",
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
        minimized: false,
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

/// The main window of a running program (`exe`, e.g. "Client-Win64-Shipping.exe"),
/// for a game the user picked: its largest visible top-level window.
pub fn find(exe: &str) -> Option<Foreground> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowLongW, GetWindowTextLengthW, IsWindowVisible, GWL_EXSTYLE, GW_OWNER,
        WS_EX_TOOLWINDOW,
    };
    struct Search<'a> {
        exe: &'a str,
        best: Option<(i64, HWND, u32)>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> BOOL {
        let s = unsafe { &mut *(lp.0 as *mut Search) };
        // Some games' windows have no title: allowed, but a titled window wins.
        let shown = unsafe { IsWindowVisible(hwnd).as_bool() };
        let titled = unsafe { GetWindowTextLengthW(hwnd) > 0 };
        let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.is_ok_and(|o| !o.0.is_null());
        let tool = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW.0 != 0;
        if shown && !owned && !tool {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if archy_core::process::exe_name(pid).is_some_and(|n| n.eq_ignore_ascii_case(s.exe)) {
                let mut r = RECT::default();
                let _ = unsafe { GetWindowRect(hwnd, &mut r) };
                // Minimized windows count, but any open one wins.
                let area = if unsafe { IsIconic(hwnd) }.as_bool() { 0 } else { (r.right - r.left) as i64 * (r.bottom - r.top) as i64 + 1 };
                // Titled windows rank above any untitled one.
                let area = if titled { area + (1i64 << 40) } else { area };
                if s.best.is_none_or(|(a, _, _)| area > a) {
                    s.best = Some((area, hwnd, pid));
                }
            }
        }
        true.into()
    }
    let mut search = Search { exe, best: None };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut _ as isize));
    }
    let (_, hwnd, pid) = search.best?;
    let minimized = unsafe { IsIconic(hwnd) }.as_bool();
    Some(Foreground { hwnd, exe: exe.to_string(), pid, fullscreen: covers_monitor(hwnd), minimized })
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
        let fg = Foreground { hwnd: HWND::default(), exe: "Game.exe".into(), pid: 1, fullscreen: true, minimized: false };
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
