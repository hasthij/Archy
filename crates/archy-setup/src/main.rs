//! Archy Setup: a per-user installer (no admin needed).
//!
//! Installs into %LOCALAPPDATA%\Programs\Archy, adds a Start menu shortcut,
//! registers with Settings > Apps (with an Uninstall button) and starts Archy.
//! Run with `--uninstall` (what Settings > Apps does) to remove it again.
//! Clips and settings are never deleted.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use archy_core::settings::Settings;
use windows::core::{w, Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_ALL_ACCESS, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
};

static ARCHY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/archy.exe"));
static ARCHY_UI: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/archy-ui.exe"));
const VERSION: &str = env!("CARGO_PKG_VERSION");
const UNINSTALL_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Archy");

fn ask(text: &str) -> bool {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            w!("Archy Setup"),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

fn tell(text: &str, error: bool) {
    let icon = if error {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("Archy Setup"), MB_OK | icon);
    }
}

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

fn install_dir() -> PathBuf {
    env_dir("LOCALAPPDATA").join("Programs").join("Archy")
}

fn start_menu_link() -> PathBuf {
    env_dir("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs\Archy.lnk")
}

fn desktop_link() -> PathBuf {
    env_dir("USERPROFILE").join("Desktop").join("Archy.lnk")
}

/// Closes running Archy processes so their files can be replaced or removed.
fn stop_archy() {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
            let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_ascii_lowercase();
            if name == "archy.exe" || name == "archy-ui.exe" {
                if let Ok(h) = OpenProcess(
                    PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                    false,
                    e.th32ProcessID,
                ) {
                    let _ = TerminateProcess(h, 0);
                    WaitForSingleObject(h, 3000);
                    let _ = CloseHandle(h);
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
}

fn make_shortcut(link: &Path, target: &Path, description: &str) -> windows::core::Result<()> {
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        shell.SetPath(&HSTRING::from(target.as_os_str()))?;
        if let Some(dir) = target.parent() {
            shell.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        shell.SetDescription(&HSTRING::from(description))?;
        shell.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
        let file: IPersistFile = shell.cast()?;
        file.Save(&HSTRING::from(link.as_os_str()), true)
    }
}

fn reg_string(key: HKEY, name: &str, value: &str) {
    let v: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let bytes = unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2) };
    unsafe {
        let _ = RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes));
    }
}

fn reg_dword(key: HKEY, name: &str, value: u32) {
    unsafe {
        let _ = RegSetValueExW(
            key,
            &HSTRING::from(name),
            None,
            REG_DWORD,
            Some(&value.to_le_bytes()),
        );
    }
}

/// Settings > Apps entry, with Uninstall pointing back at this installer.
fn register(dir: &Path, uninstaller: &Path) {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            UNINSTALL_KEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut key,
            None,
        )
        .is_err()
        {
            return;
        }
        reg_string(key, "DisplayName", "Archy");
        reg_string(key, "DisplayVersion", VERSION);
        reg_string(key, "Publisher", "Archy");
        reg_string(
            key,
            "DisplayIcon",
            &dir.join("archy.exe").display().to_string(),
        );
        reg_string(key, "InstallLocation", &dir.display().to_string());
        reg_string(
            key,
            "UninstallString",
            &format!("\"{}\" --uninstall", uninstaller.display()),
        );
        reg_dword(key, "NoModify", 1);
        reg_dword(key, "NoRepair", 1);
        reg_dword(
            key,
            "EstimatedSize",
            ((ARCHY.len() + ARCHY_UI.len()) / 1024) as u32,
        );
        let _ = RegCloseKey(key);
    }
}

fn install() -> Result<(), String> {
    let dir = install_dir();
    let upgrading = dir.join("archy.exe").exists();
    let question = if upgrading {
        "Update Archy to the latest version?\n\nYour clips and settings are kept."
    } else {
        "Install Archy?\n\nArchy records the last moments of your games and saves them with a hotkey or your voice. Everything stays on this PC.\n\nIt installs just for you (no admin needed)."
    };
    if !ask(question) {
        return Ok(());
    }
    stop_archy();
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    let write = |name: &str, bytes: &[u8]| {
        std::fs::write(dir.join(name), bytes).map_err(|e| format!("couldn't write {name}: {e}"))
    };
    write("archy.exe", ARCHY)?;
    write("archy-ui.exe", ARCHY_UI)?;
    // A copy of this installer does the uninstalling from Settings > Apps.
    let uninstaller = dir.join("uninstall.exe");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if me != uninstaller {
        std::fs::copy(&me, &uninstaller)
            .map_err(|e| format!("couldn't write uninstall.exe: {e}"))?;
    }

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let ui = dir.join("archy-ui.exe");
    make_shortcut(&start_menu_link(), &ui, "Archy: game clips")
        .map_err(|e| format!("Start menu shortcut: {}", e.message()))?;
    if !upgrading && ask("Add an Archy shortcut to your desktop?") {
        let _ = make_shortcut(&desktop_link(), &ui, "Archy: game clips");
    }
    register(&dir, &uninstaller);

    if !upgrading {
        let mut settings = Settings::load().0;
        settings.start_with_windows = ask("Start Archy automatically when you sign in to Windows?\n\nRecommended: then clips are always ready. You can change this later in Archy > General.");
        let _ = settings.save();
    }

    // Start the recorder in the tray, then open the window.
    let _ = std::process::Command::new(dir.join("archy.exe")).spawn();
    let _ = std::process::Command::new(&ui).spawn();
    let key = Settings::load().0.hotkey_save;
    tell(
        &format!("Archy is installed and running in your system tray (bottom-right, next to the clock).\n\nOpen a game and press {key} to save the last seconds."),
        false,
    );
    Ok(())
}

fn uninstall() {
    if !ask("Uninstall Archy?\n\nYour clips and settings are kept.") {
        return;
    }
    stop_archy();
    // Settings has the start-with-Windows switch; turn it off so nothing points at removed files.
    let mut settings = Settings::load().0;
    if settings.start_with_windows {
        settings.start_with_windows = false;
        let _ = settings.save();
    }
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut key,
            None,
        )
        .is_ok()
        {
            let _ = windows::Win32::System::Registry::RegDeleteValueW(key, w!("Archy"));
            let _ = RegCloseKey(key);
        }
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL_KEY);
    }
    let _ = std::fs::remove_file(start_menu_link());
    let _ = std::fs::remove_file(desktop_link());
    let dir = install_dir();
    let _ = std::fs::remove_file(dir.join("archy.exe"));
    let _ = std::fs::remove_file(dir.join("archy-ui.exe"));
    let _ = std::fs::remove_dir_all(dir.join("archy-ui.exe.WebView2"));
    tell(
        "Archy has been uninstalled. Your clips are still in your clips folder.",
        false,
    );
    // This program is uninstall.exe itself: remove it and the folder once it has exited.
    let _ = std::process::Command::new("cmd")
        .args([
            "/C", "timeout", "/T", "2", "/NOBREAK", ">NUL", "&", "rmdir", "/S", "/Q",
        ])
        .arg(&dir)
        .spawn();
}

fn main() {
    if std::env::args().any(|a| a == "--uninstall") {
        uninstall();
    } else if let Err(e) = install() {
        tell(&format!("Archy couldn't be installed:\n\n{e}"), true);
    }
}
