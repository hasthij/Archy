//! Push-to-talk: the mic is silent in clips except while a key is held.
//!
//! A small thread checks the key ~60 times a second (GetAsyncKeyState: no hook,
//! the key still works in the game and in chat) and sets the mic's volume to its
//! level or 0. The mic stays open, so holding the key is heard at once. Voice
//! clipping has its own mic feed and keeps hearing you.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

pub struct PushToTalk {
    pub key: String,
    stop: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
}

/// The virtual key for `text`: a keyboard key ("V", "F13", "Ctrl+Space": only the
/// main key counts) or a mouse side button ("Mouse4", "Mouse5").
pub fn virtual_key(text: &str) -> Option<i32> {
    match text.trim().to_ascii_lowercase().as_str() {
        "mouse4" | "mouse 4" | "xbutton1" => Some(0x05),
        "mouse5" | "mouse 5" | "xbutton2" => Some(0x06),
        "mouse3" | "middle mouse" => Some(0x04),
        _ => crate::hotkey::parse(text).map(|h| h.vk as i32),
    }
}

impl PushToTalk {
    /// `mic`: the volume the recorder reads for the mic (track and mix).
    pub fn start(key: &str, mic: archy_audio::Volume, level: u32) -> Option<Self> {
        let vk = virtual_key(key)?;
        let stop = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(level));
        let (flag, lvl) = (stop.clone(), level.clone());
        std::thread::Builder::new()
            .name("archy-ptt".into())
            .spawn(move || {
                let mut last = u32::MAX;
                while !flag.load(Ordering::Acquire) {
                    // The high bit is set while the key is down.
                    let held = unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0;
                    let v = if held { lvl.load(Ordering::Relaxed) } else { 0 };
                    if v != last {
                        mic.store(v, Ordering::Relaxed);
                        last = v;
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
            })
            .ok()?;
        Some(Self { key: key.to_string(), stop, level })
    }

    /// The mic's volume while the key is held (follows the settings).
    pub fn set_level(&self, level: u32) {
        self.level.store(level, Ordering::Relaxed);
    }
}

impl Drop for PushToTalk {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn keys_and_mouse_buttons() {
        assert_eq!(super::virtual_key("Mouse4"), Some(0x05));
        assert_eq!(super::virtual_key("mouse5"), Some(0x06));
        assert_eq!(super::virtual_key("V"), Some(b'V' as i32));
        assert_eq!(super::virtual_key("Ctrl+Space"), Some(0x20));
        assert_eq!(super::virtual_key(""), None);
    }
}
