//! The tray icon (05 §Platform polish).
//!
//! It exists because the window is hidden rather than closed, and a background app with no
//! tray presence is an app users think they have quit. Two things live here and nothing
//! else: the aggregate throughput in the tooltip, and the two actions worth taking without
//! opening the window.
//!
//! The menu items do not talk to the daemon. Pausing everything means knowing what
//! everything is, and the job list lives in the frontend — so the menu emits an intent and
//! the window acts on it. Keeping one model of the queue instead of two is worth the
//! extra hop, and the hop is a message inside one process.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter};

const TRAY: &str = "vortex";
/// `"pauseAll" | "resumeAll"`, handled by the window.
const INTENT: &str = "vortex://tray";

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Vortex", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pauseAll", "Pause all", true, None::<&str>)?;
    let resume = MenuItem::with_id(app, "resumeAll", "Resume all", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Vortex", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &pause,
            &resume,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY)
        .menu(&menu)
        // The menu is for the menu button. A left click opens the window, because that is
        // what every user tries first.
        .show_menu_on_left_click(false)
        .tooltip("Vortex")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => crate::chrome::reveal(app),
            "quit" => {
                // Transfers are `vortexd`'s, not ours. Quitting the window stops nothing.
                app.exit(0);
            }
            intent => {
                let _ = app.emit(INTENT, intent);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                crate::chrome::reveal(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Sets the tray tooltip.
///
/// The frontend already computes the aggregate for the titlebar; asking it for the string
/// costs one call at 2 Hz and saves the backend from keeping a second copy of the queue
/// just to add up eight numbers.
#[tauri::command]
pub fn tooltip(app: AppHandle, text: String) {
    if let Some(tray) = app.tray_by_id(TRAY) {
        let _ = tray.set_tooltip(Some(text));
    }
}
