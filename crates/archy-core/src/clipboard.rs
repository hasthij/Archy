//! Putting a file on the clipboard (like Ctrl+C in Explorer), so Ctrl+V in
//! Discord or any chat attaches it.

use std::path::Path;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::DROPFILES;

/// Copies the file itself (like Ctrl+C in Explorer), so Ctrl+V in Discord attaches it.
pub fn copy_file(path: &Path) -> bool {
    let wide: Vec<u16> = path.as_os_str().encode_wide_null();
    let header = std::mem::size_of::<DROPFILES>();
    // File list: one path, then an extra null ending the list.
    let size = header + (wide.len() + 1) * 2;
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, size) else {
            return false;
        };
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        std::ptr::write_bytes(ptr, 0, size);
        let drop = ptr as *mut DROPFILES;
        (*drop).pFiles = header as u32;
        (*drop).fWide = true.into();
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr.add(header) as *mut u16, wide.len());
        let _ = GlobalUnlock(mem);
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        let _ = EmptyClipboard();
        // On success the clipboard owns the memory.
        let ok = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(mem.0))).is_ok();
        if !ok {
            let _ = GlobalFree(Some(HGLOBAL(mem.0)));
        }
        let _ = CloseClipboard();
        ok
    }
}

trait EncodeWideNull {
    fn encode_wide_null(&self) -> Vec<u16>;
}

impl EncodeWideNull for std::ffi::OsStr {
    fn encode_wide_null(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}
