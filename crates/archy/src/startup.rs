//! "Start with Windows": a per-user Run entry pointing at this archy.exe.
//! Only touched when the setting changes (or at start-up, to follow the exe if it moved).

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const NAME: PCWSTR = w!("Archy");

pub fn set(enabled: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .is_err()
        {
            return;
        }
        if enabled {
            if let Ok(exe) = std::env::current_exe() {
                let value: Vec<u16> = format!("\"{}\"", exe.display())
                    .encode_utf16()
                    .chain([0])
                    .collect();
                let bytes =
                    std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
                let _ = RegSetValueExW(key, NAME, None, REG_SZ, Some(bytes));
            }
        } else {
            let _ = RegDeleteValueW(key, NAME);
        }
        let _ = RegCloseKey(key);
    }
}
