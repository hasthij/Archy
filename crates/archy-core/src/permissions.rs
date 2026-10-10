//! Checks for the Windows switches Archy depends on. Archy only reads these;
//! when one is off it opens the right Settings page so the user can decide.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
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

/// Microsoft's Visual C++ runtime, which the voice engine's DLLs need. Most PCs
/// already have it (games and drivers install it).
pub fn cpp_runtime_installed() -> bool {
    let system = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| r"C:\Windows".into())
        .join("System32");
    ["msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll"]
        .iter()
        .all(|f| system.join(f).exists())
}

/// Microsoft's official Visual C++ runtime installer (x64).
pub fn open_cpp_runtime_download() {
    open("https://aka.ms/vs/17/release/vc_redist.x64.exe");
}
