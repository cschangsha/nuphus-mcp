//! Nuphus Input — IME-style Unicode text input
//!
//! Core design: treat text input as an "IME session" rather than simple key presses.
//! 1. Focus preparation: AttachThreadInput ensures cross-thread focus transfers correctly
//! 2. Encoding conversion: UTF-8 → UTF-16, correctly handling non-BMP characters (surrogate pairs)
//! 3. Per-character sending: KeyDown + KeyUp for each codepoint, KEYEVENTF_UNICODE bypasses keyboard layout
//! 4. Session end: optional Enter commit, with a delay to ensure the app finishes processing

use crate::core::*;

use ::windows::Win32::Foundation::HWND;
use ::windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use ::windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_TYPE, KEYBDINPUT, KEYBD_EVENT_FLAGS, VIRTUAL_KEY,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_UNICODE: u32 = 0x0004;
const KEYEVENTF_KEYUP: u32 = 0x0002;

/// IME session configuration
#[derive(Debug, Clone)]
pub struct InputSession {
    /// Target window handle (None = current focus window)
    pub target_hwnd: Option<isize>,
    /// Whether to press Enter after sending
    pub press_enter: bool,
    /// Delay between characters (milliseconds)
    pub char_delay_ms: u64,
    /// Extra delay after sending (milliseconds)
    pub post_delay_ms: u64,
    /// Whether to force-activate the window (even if already in the foreground)
    pub force_activate: bool,
    /// Whether to verify the target window is in the foreground (refuse sending if not)
    pub verify_foreground: bool,
}

impl Default for InputSession {
    fn default() -> Self {
        Self {
            target_hwnd: None,
            press_enter: false,
            char_delay_ms: 5,
            post_delay_ms: 50,
            force_activate: false,
            verify_foreground: false,
        }
    }
}

/// Nuphus unified input interface — IME philosophy
///
/// Flow: focus preparation → encoding conversion → per-character injection → optional commit → delay wait
pub fn nuphus_input(text: &str, session: &InputSession) -> Result<usize> {
    #[cfg(windows)]
    {
        // ── 1. Focus preparation ──
        let target = prepare_focus(
            session.target_hwnd,
            session.force_activate,
            session.verify_foreground,
        )?;
        if session.verify_foreground && !target.verified {
            return Err(DesktopError::InputFailed(
                "Target window is not in foreground, input rejected".to_string(),
            ));
        }
        // RAII guard: detaches the thread-input attachment on ANY exit (success or
        // error), so a mid-stream SendInput failure can't leak the attachment.
        let _attach_guard = ThreadInputGuard {
            target_tid: target.attached_hwnd,
        };

        // ── 2. Encoding conversion: UTF-8 → UTF-16 codepoint sequence ──
        let codepoints = encode_utf16_codepoints(text);

        // ── 3. Per-character injection ──
        let mut total_sent = 0;
        for cp in &codepoints {
            let inputs = make_unicode_inputs(*cp);
            let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
            // Partial injection (0 < sent < len) leaves a key physically stuck
            // down — treat it as a hard failure, same as a total failure.
            if sent == 0 {
                return Err(DesktopError::InputFailed(format!(
                    "SendInput failed for codepoint U+{:04X}",
                    cp
                )));
            }
            if (sent as usize) < inputs.len() {
                return Err(DesktopError::InputFailed(format!(
                    "SendInput partially injected codepoint U+{:04X} ({sent}/{} events)",
                    cp,
                    inputs.len()
                )));
            }
            total_sent += sent as usize;

            // Delay between characters
            if session.char_delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(session.char_delay_ms));
            }
        }

        // ── 4. Optional commit (Enter) ──
        if session.press_enter {
            let enter_inputs = make_enter_inputs();
            let sent = unsafe { SendInput(&enter_inputs, std::mem::size_of::<INPUT>() as i32) };
            if (sent as usize) < enter_inputs.len() {
                return Err(DesktopError::InputFailed(format!(
                    "SendInput Enter failed ({sent}/{} events)",
                    enter_inputs.len()
                )));
            }
            total_sent += sent as usize;
        }

        // ── 5. Post-send delay, ensure the app finishes processing ──
        if session.post_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(session.post_delay_ms));
        }

        // ── 6. Restore focus (if Attach was used earlier) — handled by
        // `_attach_guard`'s Drop on exit. ──

        Ok(total_sent)
    }

    #[cfg(not(windows))]
    {
        Err(DesktopError::PlatformNotSupported)
    }
}

/// Convenience function: send to the specified window
pub fn input_to_window(text: &str, hwnd: isize, press_enter: bool) -> Result<usize> {
    nuphus_input(
        text,
        &InputSession {
            target_hwnd: Some(hwnd),
            press_enter,
            ..Default::default()
        },
    )
}

/// Convenience function: send to the current focus window
pub fn input_to_focus(text: &str, press_enter: bool) -> Result<usize> {
    nuphus_input(
        text,
        &InputSession {
            press_enter,
            ..Default::default()
        },
    )
}

// ============================================================================
// Internal implementation
// ============================================================================

struct FocusResult {
    attached_hwnd: Option<u32>,
    verified: bool,
}

/// RAII guard that detaches a thread-input attachment on drop, so it is released
/// on every exit path (success, error, early return) instead of only the happy path.
struct ThreadInputGuard {
    target_tid: Option<u32>,
}

impl Drop for ThreadInputGuard {
    fn drop(&mut self) {
        if let Some(tid) = self.target_tid {
            unsafe {
                let _ = AttachThreadInput(GetCurrentThreadId(), tid, false);
            }
        }
    }
}

/// Prepare the window focus, returning the thread ID to detach
fn prepare_focus(target_hwnd: Option<isize>, force: bool, verify: bool) -> Result<FocusResult> {
    unsafe {
        if let Some(hwnd_val) = target_hwnd {
            let hwnd = HWND(hwnd_val);

            // Restore if minimized
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            // Verify mode: check whether the window is in the foreground, return immediately if not
            if verify {
                let is_fg = is_foreground_window(hwnd_val);
                return Ok(FocusResult {
                    attached_hwnd: None,
                    verified: is_fg,
                });
            }

            // Cross-thread focus: AttachThreadInput
            let target_tid = GetWindowThreadProcessId(hwnd, None);
            let current_tid = GetCurrentThreadId();
            let mut attached = None;

            if target_tid != current_tid {
                let _ = AttachThreadInput(current_tid, target_tid, true);
                attached = Some(target_tid);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            // Activate the window
            if force || !is_foreground_window(hwnd_val) {
                let _ = SetForegroundWindow(hwnd);
                std::thread::sleep(std::time::Duration::from_millis(150));
            }

            Ok(FocusResult {
                attached_hwnd: attached,
                verified: true,
            })
        } else {
            // No target window, use the current focus
            Ok(FocusResult {
                attached_hwnd: None,
                verified: true,
            })
        }
    }
}

/// Check whether the specified window is in the foreground
unsafe fn is_foreground_window(hwnd: isize) -> bool {
    use ::windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let fg = GetForegroundWindow();
    fg.0 == hwnd
}

/// UTF-8 → UTF-16 codepoint sequence (correctly handles surrogate pairs)
fn encode_utf16_codepoints(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Create the KeyDown + KeyUp input pair for a single Unicode codepoint
fn make_unicode_inputs(ch: u16) -> [INPUT; 2] {
    [
        // KeyDown
        INPUT {
            r#type: INPUT_TYPE(INPUT_KEYBOARD),
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0), // must be 0 in KEYEVENTF_UNICODE mode
                    wScan: ch,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        // KeyUp
        INPUT {
            r#type: INPUT_TYPE(INPUT_KEYBOARD),
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0), // must be 0 in KEYEVENTF_UNICODE mode
                    wScan: ch,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ]
}

/// Create the KeyDown + KeyUp input pair for the Enter key
fn make_enter_inputs() -> [INPUT; 2] {
    const VK_RETURN: u16 = 0x0D;
    [
        // KeyDown
        INPUT {
            r#type: INPUT_TYPE(INPUT_KEYBOARD),
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(VK_RETURN),
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        // KeyUp
        INPUT {
            r#type: INPUT_TYPE(INPUT_KEYBOARD),
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(VK_RETURN),
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_KEYUP),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ]
}

// ============================================================================
// Legacy interface compatibility (deprecated)
// ============================================================================

/// Send Unicode text to the current focus window (legacy interface, prefer nuphus_input)
pub fn send_unicode_text(text: &str) -> Result<usize> {
    nuphus_input(text, &InputSession::default())
}

/// Send character by character with an interval (legacy interface, prefer nuphus_input)
pub fn send_unicode_chars(text: &str, interval_ms: u64) -> Result<usize> {
    nuphus_input(
        text,
        &InputSession {
            char_delay_ms: interval_ms,
            ..Default::default()
        },
    )
}
