//! Mouse control — native Win32 / enigo on macOS / PlatformNotSupported on Linux

use crate::core::*;

#[cfg(not(windows))]
use enigo::Mouse as _;

/// Create a fresh enigo instance (macOS / Linux).
///
/// macOS's enigo backend holds a `CGEventSource` that is not `Send`, so an
/// instance can't live behind a shared `OnceLock<Mutex<_>>` (that would need
/// `Enigo: Send`). Reconstructing per call sidesteps the `Send` bound and the
/// `MutexGuard` temporary-lifetime traps entirely; input ops are low-frequency
/// so the per-call init cost is negligible.
#[cfg(not(windows))]
fn enigo() -> Result<enigo::Enigo> {
    enigo::Enigo::new(&enigo::Settings::default())
        .map_err(|e| DesktopError::InputFailed(e.to_string()))
}

/// Move the mouse to the given coordinates
pub async fn move_to(x: i32, y: i32) -> Result<()> {
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        unsafe {
            let _ = SetCursorPos(x, y);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        enigo()?
            .move_mouse(x, y, enigo::Coordinate::Abs)
            .map_err(|e| DesktopError::InputFailed(e.to_string()))
    }
    #[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
    {
        Err(DesktopError::PlatformNotSupported)
    }
}

/// Get the current mouse position
pub async fn position() -> Result<Point> {
    #[cfg(windows)]
    {
        use ::windows::Win32::Foundation::POINT;
        use ::windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut pt = POINT::default();
        unsafe { GetCursorPos(&mut pt) }
            .map_err(|e| DesktopError::InputFailed(format!("GetCursorPos failed: {e}")))?;
        Ok(Point { x: pt.x, y: pt.y })
    }
    #[cfg(not(windows))]
    {
        let pos = enigo()?
            .location()
            .map_err(|e| DesktopError::InputFailed(e.to_string()))?;
        Ok(Point { x: pos.0, y: pos.1 })
    }
    #[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
    {
        Err(DesktopError::PlatformNotSupported)
    }
}

/// Click the mouse
pub async fn click(x: i32, y: i32) -> Result<()> {
    move_to(x, y).await?;
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{
            mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        };
        unsafe {
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        unsafe {
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        enigo()?
            .button(enigo::Button::Left, enigo::Direction::Click)
            .map_err(|e| DesktopError::InputFailed(e.to_string()))
    }
    #[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
    {
        Err(DesktopError::PlatformNotSupported)
    }
}

/// Right-click the mouse
pub async fn right_click(x: i32, y: i32) -> Result<()> {
    move_to(x, y).await?;
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{
            mouse_event, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        };
        unsafe {
            mouse_event(MOUSEEVENTF_RIGHTDOWN, 0, 0, 0, 0);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        unsafe {
            mouse_event(MOUSEEVENTF_RIGHTUP, 0, 0, 0, 0);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        enigo()?
            .button(enigo::Button::Right, enigo::Direction::Click)
            .map_err(|e| DesktopError::InputFailed(e.to_string()))
    }
    #[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
    {
        Err(DesktopError::PlatformNotSupported)
    }
}

/// Scroll the mouse wheel
///
/// `direction`: "up" / "down"; `amount`: number of wheel notches (each notch = 120 delta).
pub async fn scroll(direction: &str, amount: i32) -> Result<()> {
    // Validate before touching any platform API: an unknown direction must be an
    // error, not a silent "down".
    let delta: i32 = match direction {
        "up" => 120,
        "down" => -120,
        other => {
            return Err(DesktopError::InputFailed(format!(
                "unknown scroll direction: {other} (expected \"up\" or \"down\")"
            )));
        }
    };
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_WHEEL};
        for _ in 0..amount.max(0) {
            unsafe {
                mouse_event(MOUSEEVENTF_WHEEL, 0, 0, delta, 0);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (delta, amount);
        Err(DesktopError::PlatformNotSupported)
    }
}

/// Middle-click the mouse (mouse_event MIDDLEDOWN/UP on Windows).
pub async fn middle_click(x: i32, y: i32) -> Result<()> {
    move_to(x, y).await?;
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{
            mouse_event, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
        };
        unsafe {
            mouse_event(MOUSEEVENTF_MIDDLEDOWN, 0, 0, 0, 0);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        unsafe {
            mouse_event(MOUSEEVENTF_MIDDLEUP, 0, 0, 0, 0);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        enigo()?
            .button(enigo::Button::Middle, enigo::Direction::Click)
            .map_err(|e| DesktopError::InputFailed(e.to_string()))
    }
    #[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
    {
        Err(DesktopError::PlatformNotSupported)
    }
}

/// Drag the mouse
pub async fn drag(start: Point, end: Point) -> Result<()> {
    move_to(start.x, start.y).await?;
    #[cfg(windows)]
    {
        use ::windows::Win32::UI::Input::KeyboardAndMouse::{
            mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        };
        unsafe {
            mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0);
        }
        let steps = 20;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let x = (start.x as f32 + (end.x as f32 - start.x as f32) * t) as i32;
            let y = (start.y as f32 + (end.y as f32 - start.y as f32) * t) as i32;
            let _ = move_to(x, y).await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        unsafe {
            mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let mut e = enigo()?;
        e.button(enigo::Button::Left, enigo::Direction::Press)
            .map_err(|e| DesktopError::InputFailed(e.to_string()))?;
        drop(e);
        for i in 1..=20 {
            let t = i as f32 / 20.0;
            let x = (start.x as f32 + (end.x as f32 - start.x as f32) * t) as i32;
            let y = (start.y as f32 + (end.y as f32 - start.y as f32) * t) as i32;
            let _ = move_to(x, y).await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut e = enigo()?;
        e.button(enigo::Button::Left, enigo::Direction::Release)
            .map_err(|e| DesktopError::InputFailed(e.to_string()))
    }
    #[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
    {
        Err(DesktopError::PlatformNotSupported)
    }
}
