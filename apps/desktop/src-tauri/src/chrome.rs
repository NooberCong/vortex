//! The window itself — the parts of "frameless but still a real window" that only the
//! native side can do (05 §Platform polish).
//!
//! Geometry restore is the `window-state` plugin's job and is configured in `lib.rs`; what
//! is here is the backdrop and the show/focus path, both of which have enough
//! platform-specific failure modes to be worth naming.

use tauri::{AppHandle, Manager, WebviewWindow};

/// Applies the platform backdrop.
///
/// Failure is not an error. Mica needs Windows 11 build 22000 or newer, and on anything
/// older the window simply keeps the solid `--bg` the stylesheet already painted. A
/// download manager that refuses to open because it could not blur its own chrome would
/// be a worse product than one that looks slightly flatter on Windows 10.
pub fn dress(window: &WebviewWindow) {
    #[cfg(windows)]
    {
        // Mica follows the system theme by itself, which is the right behaviour for the
        // two thirds of users who never touch the theme setting. An explicit light/dark
        // choice in Settings is applied by the frontend through `setTheme`, and the
        // backdrop follows that too.
        if window_vibrancy::apply_mica(window, None).is_err() {
            tracing::debug!("no Mica backdrop on this Windows build");
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// Squares the window's corners while it is maximised, and rounds them again when it is
/// not.
///
/// Windows 11 rounds a top-level window in the compositor. That happens outside the webview
/// and there is no stylesheet that reaches it — the shell's own `--shell-radius` handles
/// the pixels it draws, and this handles the ones DWM cuts away.
///
/// Floating, the rounding is right and matches what the shell paints. Maximised it is not:
/// a borderless window is sized to the work area exactly, so rounding it takes four notches
/// out of the screen's own corners and the desktop shows through them.
///
/// Pre-11 the attribute does not exist, the call fails, and there was no rounding to undo.
pub fn square_corners(window: &tauri::Window) {
    #[cfg(windows)]
    {
        use windows::Win32::Graphics::Dwm::{
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND,
            DwmSetWindowAttribute,
        };

        let Ok(handle) = window.hwnd() else { return };
        let corner = if window.is_maximized().unwrap_or(false) {
            DWMWCP_DONOTROUND
        } else {
            DWMWCP_ROUND
        };
        // SAFETY: `handle` is this process's live window, and the attribute is passed by
        // pointer with its own size, which is the contract `DwmSetWindowAttribute` states.
        let set = unsafe {
            DwmSetWindowAttribute(
                handle,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                std::ptr::from_ref(&corner).cast(),
                std::mem::size_of_val(&corner) as u32,
            )
        };
        if set.is_err() {
            tracing::debug!("no corner preference on this Windows build");
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// How long the window will wait for its own page before showing itself anyway.
///
/// The splash is the whole reason the window starts hidden, and a webview that never
/// finishes loading — a dead dev server, a broken install — must not mean an application
/// that is running with nothing on screen and no way to reach it.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(8);

/// Shows the window once its page has painted, and no later than [`PATIENCE`].
///
/// The window is created hidden (`visible: false` in `tauri.conf.json`) because a cold
/// WebView2 start otherwise flashes an empty frame for a few hundred milliseconds before
/// anything is drawn into it. `show()` on an already-visible window is a no-op, so the two
/// paths cannot fight.
pub fn show_when_loaded(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    std::thread::spawn(move || {
        std::thread::sleep(PATIENCE);
        let _ = window.show();
    });
}

/// Brings the window back — from the tray, from a second launch, from the taskbar.
///
/// `show` then `unminimize` then `set_focus`, in that order: a hidden window cannot be
/// unminimized, and a minimized window cannot take focus.
pub fn reveal(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
