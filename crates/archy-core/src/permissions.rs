//! Checks for the Windows switches Archy depends on. Archy only reads these;
//! when one is off it opens the right Settings page so the user can decide.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn read_string(root: HKEY, key: &str, value: PCWSTR) -> Option<String> {
    let key = HSTRING::from(key);
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            root,
            &key,
            value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// "Microphone access" and "Let desktop apps access your microphone" are both on.
/// Unknown (never set) counts as allowed: that's Windows' default.
pub fn microphone_allowed() -> bool {
    let base = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
    let global = read_string(HKEY_CURRENT_USER, base, w!("Value"));
    let desktop = read_string(
        HKEY_CURRENT_USER,
        &format!(r"{base}\NonPackaged"),
        w!("Value"),
    );
    let denied = |v: &Option<String>| v.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("Deny"));
    !denied(&global) && !denied(&desktop)
}

/// The WebView2 runtime the Archy window uses (built into Windows 11).
pub fn webview2_installed() -> bool {
    const CLIENT: &str = r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
    let found = |root: HKEY, key: String| {
        read_string(root, &key, w!("pv")).is_some_and(|v| !v.is_empty() && v != "0.0.0.0")
    };
    found(
        HKEY_LOCAL_MACHINE,
        format!(r"SOFTWARE\WOW6432Node\{CLIENT}"),
    ) || found(HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{CLIENT}"))
        || found(HKEY_CURRENT_USER, format!(r"Software\{CLIENT}"))
}

fn open(uri: &str) {
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(uri),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Settings > Privacy & security > Microphone.
pub fn open_microphone_settings() {
    open("ms-settings:privacy-microphone");
}

/// Microsoft's WebView2 runtime download page.
pub fn open_webview2_download() {
    open("https://go.microsoft.com/fwlink/p/?LinkId=2124703");
}

/// Oldest Visual C++ runtime (14.<this>) the voice engine loads on. An older one
/// has the same DLL names but the engine fails with "DLL initialization routine
/// failed" (seen on Windows 10 PCs that only had a games-era runtime).
const MIN_CPP_RUNTIME_MINOR: u32 = 38;

fn read_dword(root: HKEY, key: &str, value: PCWSTR) -> Option<u32> {
    let key = HSTRING::from(key);
    let mut out = 0u32;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(root, &key, value, RRF_RT_REG_DWORD, None, Some(&mut out as *mut u32 as *mut _), Some(&mut size))
    };
    r.is_ok().then_some(out)
}

/// Microsoft's Visual C++ runtime, which the voice engine's DLLs need: present
/// and new enough. Most PCs already have it (games and drivers install it), but
/// often an old one.
pub fn cpp_runtime_installed() -> bool {
    let system = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| r"C:\Windows".into())
        .join("System32");
    let files = ["msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll"]
        .iter()
        .all(|f| system.join(f).exists());
    let minor = read_dword(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64",
        w!("Minor"),
    );
    files && minor.is_some_and(|m| m >= MIN_CPP_RUNTIME_MINOR)
}

const CPP_RUNTIME_URL: &str = "https://aka.ms/vs/17/release/vc_redist.x64.exe";

/// Downloads Microsoft's official Visual C++ runtime installer (x64) and runs it
/// (Windows asks the user to approve it; it shows its own progress). Falls back to
/// opening the download link in the browser if the download fails. Returns at once.
pub fn open_cpp_runtime_download() {
    use std::os::windows::process::CommandExt;
    std::thread::spawn(|| {
        let file = std::env::temp_dir().join("Archy-vc_redist.x64.exe");
        let curl = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap_or(r"C:\Windows".into()))
            .join("System32")
            .join("curl.exe");
        let downloaded = std::process::Command::new(curl)
            .args(["-fsSL", "--retry", "2", "--connect-timeout", "20", "-o"])
            .arg(&file)
            .arg(CPP_RUNTIME_URL)
            .creation_flags(0x0800_0000) // no console window
            .status()
            .is_ok_and(|s| s.success());
        if !downloaded {
            open(CPP_RUNTIME_URL);
            return;
        }
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(file.as_os_str()),
                w!("/install /passive /norestart"),
                None,
                SW_SHOWNORMAL,
            );
        }
    });
}
