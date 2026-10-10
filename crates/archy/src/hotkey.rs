//! Parsing hotkey strings like "F8", "Shift+F8", "Ctrl+Alt+K".

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

pub struct Hotkey {
    pub modifiers: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

pub fn parse(text: &str) -> Option<Hotkey> {
    // Holding the key down must not save a clip per auto-repeat.
    let mut modifiers = MOD_NOREPEAT;
    let mut vk = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "shift" => modifiers |= MOD_SHIFT,
            "alt" => modifiers |= MOD_ALT,
            "win" => modifiers |= MOD_WIN,
            key => vk = Some(key_code(key)?),
        }
    }
    Some(Hotkey { modifiers, vk: vk? })
}

fn key_code(key: &str) -> Option<u32> {
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1); // VK_F1..VK_F24
    }
    let bytes = key.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_alphanumeric() {
        return Some(bytes[0].to_ascii_uppercase() as u32);
    }
    Some(match key {
        "insert" => 0x2D,
        "delete" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "pause" => 0x13,
        "scrolllock" => 0x91,
        "printscreen" => 0x2C,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_keys() {
        let h = parse("F8").unwrap();
        assert_eq!(h.vk, 0x77);
        let h = parse("Shift+F8").unwrap();
        assert!(h.modifiers.contains(MOD_SHIFT));
        assert_eq!(parse("ctrl+alt+k").unwrap().vk, b'K' as u32);
        assert!(parse("Shift+").is_none());
        assert!(parse("F99").is_none());
    }
}
