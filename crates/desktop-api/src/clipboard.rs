//! Clipboard control — native Win32 / arboard on non-Windows (macOS + Linux)

use crate::core::*;

#[cfg(windows)]
use ::windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
#[cfg(windows)]
use ::windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
#[cfg(windows)]
use ::windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GHND};

/// Decode a UTF-16 clipboard buffer as text, stopping at the first NUL **or the
/// end of the allocation** — a malformed clipboard without a NUL terminator must
/// never be scanned out of bounds (P1: unbounded `(0..)` walk was a potential AV).
#[cfg(windows)]
fn utf16_bounded_to_string(slice: &[u16]) -> String {
    let len = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    String::from_utf16_lossy(&slice[..len])
}

/// Read clipboard text
pub fn read_text() -> Result<String> {
    #[cfg(windows)]
    {
        unsafe {
            if OpenClipboard(HWND::default()).is_err() {
                return Err(DesktopError::InputFailed(
                    "clipboard open failed".to_string(),
                ));
            }
            // CF_UNICODETEXT = 13
            let result = match GetClipboardData(13) {
                Ok(h) => {
                    let h_global = HGLOBAL(h.0 as *mut core::ffi::c_void);
                    // Lock before dereferencing: an HGLOBAL is a handle, not a
                    // usable pointer (P1: raw handle was read as a pointer).
                    let ptr = GlobalLock(h_global) as *const u16;
                    if ptr.is_null() {
                        Err(DesktopError::InputFailed(
                            "clipboard lock failed".to_string(),
                        ))
                    } else {
                        let size = GlobalSize(h_global);
                        let text = if size == 0 {
                            Err(DesktopError::InputFailed(
                                "clipboard size query failed".to_string(),
                            ))
                        } else {
                            let slice =
                                std::slice::from_raw_parts(ptr, size / std::mem::size_of::<u16>());
                            Ok(utf16_bounded_to_string(slice))
                        };
                        let _ = GlobalUnlock(h_global);
                        text
                    }
                }
                Err(_) => Err(DesktopError::InputFailed(
                    "clipboard read failed".to_string(),
                )),
            };
            let _ = CloseClipboard();
            result
        }
    }
    #[cfg(not(windows))]
    {
        arboard::Clipboard::new()
            .map_err(|e| DesktopError::InputFailed(e.to_string()))?
            .get_text()
            .map_err(|e| DesktopError::InputFailed(e.to_string()))
            .map(|t| t.to_string())
    }
}

/// Write clipboard text
pub fn write_text(text: &str) -> Result<()> {
    #[cfg(windows)]
    {
        unsafe {
            if OpenClipboard(HWND::default()).is_err() {
                return Err(DesktopError::InputFailed(
                    "clipboard open failed".to_string(),
                ));
            }
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let size = (utf16.len() + 1) * 2;
            let h_global = match GlobalAlloc(GHND, size) {
                Ok(h) => h,
                Err(_) => {
                    let _ = CloseClipboard();
                    return Err(DesktopError::InputFailed(
                        "clipboard alloc failed".to_string(),
                    ));
                }
            };
            let dest = GlobalLock(h_global) as *mut u16;
            if dest.is_null() {
                let _ = GlobalFree(h_global);
                let _ = CloseClipboard();
                return Err(DesktopError::InputFailed(
                    "clipboard lock failed".to_string(),
                ));
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), dest, utf16.len());
            *dest.add(utf16.len()) = 0;
            let _ = GlobalUnlock(h_global);
            let _ = EmptyClipboard();
            // CF_UNICODETEXT = 13. On success the clipboard owns the handle; on
            // failure ownership stays with us — free it or the allocation leaks
            // (and the error must surface instead of being swallowed).
            if SetClipboardData(13, HANDLE(h_global.0 as isize)).is_err() {
                let _ = GlobalFree(h_global);
                let _ = CloseClipboard();
                return Err(DesktopError::InputFailed(
                    "clipboard set failed".to_string(),
                ));
            }
            let _ = CloseClipboard();
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        arboard::Clipboard::new()
            .map_err(|e| DesktopError::InputFailed(e.to_string()))?
            .set_text(text.to_owned())
            .map_err(|e| DesktopError::InputFailed(e.to_string()))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The bounded decoder must stop at the allocation end when there is no NUL
    /// terminator (the malformed-clipboard case that used to walk out of bounds).
    #[test]
    fn bounded_decode_stops_without_nul() {
        let data: Vec<u16> = "abc".encode_utf16().collect(); // no NUL terminator
        assert_eq!(utf16_bounded_to_string(&data), "abc");
    }

    #[test]
    fn bounded_decode_stops_at_nul() {
        let data: Vec<u16> = "ab\0cd".encode_utf16().collect();
        assert_eq!(utf16_bounded_to_string(&data), "ab");
        let empty: [u16; 0] = [];
        assert_eq!(utf16_bounded_to_string(&empty), "");
    }

    /// Write → read roundtrip through the real system clipboard, restoring the
    /// previous text afterwards so the test is not a clipboard vandal.
    #[test]
    fn clipboard_roundtrip() {
        let prev = read_text().ok();
        let payload = "nuphus-desktop-api roundtrip ✓ 中文";
        write_text(payload).expect("write clipboard");
        let read = read_text().expect("read clipboard");
        assert_eq!(read, payload);
        if let Some(prev) = prev {
            let _ = write_text(&prev);
        }
    }
}
