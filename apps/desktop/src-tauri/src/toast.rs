//! The finished-transfer notification, and the click on it (05 §Platform polish).
//!
//! `tauri-plugin-notification` can show a toast but not hear one: its desktop path spawns
//! `notification.show()` and drops the handle the activation would arrive on, so a
//! notification sent through it is a dead end. Clicking it does nothing — not even raise
//! the window — which is worse than not notifying at all, because the toast is a promise
//! that there is something to go and look at.
//!
//! So this sends its own. `notify-rust` is already compiled in underneath the plugin; all
//! that is added here is keeping the handle and waiting on it. The plugin stays for the
//! permission prompt, which is a macOS and mobile concern and is where it belongs.
//!
//! What a click does is deliberately small: bring the window back, and name the job. The
//! frontend decides what "show me that one" means — which filter has to change, whether
//! the row opens, where the list scrolls — because those are questions about the list, and
//! the list lives there.

use tauri::{AppHandle, Emitter};
use vortex_proto::JobId;

/// A `JobId`, sent when the user clicks a finished transfer's notification.
pub const ACTIVATED: &str = "vortex://notification";

/// Shows one notification and routes the click on it.
///
/// Fire and forget from the webview's side: there is no answer worth waiting for, and a
/// desktop with no notification service at all is not an error condition — the download
/// finished either way.
#[tauri::command]
pub fn notify(app: AppHandle, job: JobId, title: String, body: String) {
    #[cfg(desktop)]
    {
        // A thread rather than a task: the wait is a blocking receive on a channel the
        // platform's own event handler writes to, and it ends when the toast is clicked,
        // dismissed or times out — seconds, not the life of the process.
        std::thread::spawn(move || {
            let mut notification = notify_rust::Notification::new();
            notification.summary(&title).body(&body).auto_icon();
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                // On XDG the body is only clickable if the special `default` action was
                // registered. It is not drawn as a button; every other key would be.
                notification.action("default", "Show");
            }
            identify(&mut notification, &app);

            let Ok(handle) = notification.show() else {
                tracing::debug!("no notification service on this desktop");
                return;
            };
            let _ = handle.wait_for_response(|response: &notify_rust::NotificationResponse| {
                // The body, not a button — of which there are none. A dismissal arrives
                // here too and must not raise the window: closing a toast is how somebody
                // says they are not interested.
                if !response.is_default_action() {
                    return;
                }
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    crate::chrome::reveal(&handle);
                    let _ = handle.emit(ACTIVATED, job);
                });
            });
        });
    }
    #[cfg(not(desktop))]
    {
        let _ = (app, job, title, body);
    }
}

/// Names the sender, where Windows will accept the name.
///
/// A toast is attributed to an `AppUserModelID`, and the only ones Windows honours belong
/// to an installed application with a Start-menu entry. A development build has no such
/// entry, so claiming its identifier there is a toast that never appears at all; the
/// unregistered default is what makes one show up during development, attributed to the
/// shell.
///
/// Same rule the notification plugin uses, and one layout wider: the plugin looks only for
/// `target/debug` and `target/release`, and `tauri build` puts the executable in
/// `target/<triple>/release`, which is the directory a developer most often runs it from.
#[cfg(desktop)]
fn identify(notification: &mut notify_rust::Notification, app: &AppHandle) {
    #[cfg(windows)]
    {
        let Ok(exe) = tauri::utils::platform::current_exe() else {
            return;
        };
        if !exe.components().any(|part| part.as_os_str() == "target") {
            notification.app_id(&app.config().identifier);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (notification, app);
    }
}
