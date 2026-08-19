//! `vortex-app` — the window.
//!
//! This process is a view and a keyboard. It holds no job, no byte and no setting; every
//! one of those lives in `vortexd`, which outlives it (01 §Process model). Closing this
//! window does not stop a download and never can, which is the property the whole
//! three-process split exists to buy.
//!
//! So the Rust here is deliberately thin. It does four things the webview cannot:
//!
//! 1. speaks the named-pipe protocol to the daemon ([`link`]);
//! 2. owns the frameless window's chrome — backdrop, geometry, theme ([`chrome`]);
//! 3. keeps a tray icon alive so the aggregate readout survives a hidden window;
//! 4. relays exactly one thing in each direction, with no model of its own in between.
//!
//! Everything else — what a row looks like, what a number rounds to, when a sheet opens —
//! is in the frontend, where it can be changed without a compiler.

mod chrome;
mod link;
mod tray;

use std::sync::Arc;

use link::{Link, Role, Sink};
use tauri::{AppHandle, Emitter, Manager, State};
use vortex_proto::{Command, Event, JobId, SubscriptionScope};

/// The channel the webview listens on. One event name for the whole protocol: the payload
/// is a tagged `Event`, and the frontend switches on the tag it already has a type for.
const EVENT: &str = "vortex://event";
/// `{ connected: boolean }`. Separate from [`EVENT`] because it is about the wire rather
/// than about a job, and the window reacts to it differently.
const LINK: &str = "vortex://link";

struct Bridge {
    primary: Arc<Link>,
    detail: Arc<Link>,
}

/// Forwards everything straight to the webview. The app has no opinion about any of it.
struct ToWebview(AppHandle);

impl Sink for ToWebview {
    fn event(&self, event: Event) {
        let _ = self.0.emit(EVENT, event);
    }

    fn connection(&self, up: bool) {
        let _ = self.0.emit(LINK, serde_json::json!({ "connected": up }));
    }
}

/// Sends one command to the daemon.
///
/// A single passthrough rather than one Tauri command per protocol command: `Command` is
/// generated from the Rust definition and shared by both ends, so adding a command in
/// `vortex-proto` makes it available to the UI with no glue to forget. The exception is
/// `Subscribe`, which belongs to the bridge — the frontend asks for a detail view through
/// [`watch`] and lets the bridge decide which connection carries it.
#[tauri::command]
async fn send(bridge: State<'_, Bridge>, command: Command) -> Result<(), String> {
    if let Command::Subscribe { .. } = command {
        return Err("Subscriptions are managed by the app; use `watch`.".into());
    }
    bridge.primary.send(command).await
}

/// Points the detail connection at one job, or at nothing when the row collapses.
///
/// The 20 Hz frames that draw the per-worker segment map only exist while somebody is
/// looking at them (01 §IPC). This is the "somebody is looking" signal, and dropping it is
/// what takes an idle window back to costing the daemon nothing.
#[tauri::command]
async fn watch(bridge: State<'_, Bridge>, job: Option<JobId>) -> Result<(), String> {
    let scope = match job {
        Some(job) => SubscriptionScope::Detail { job },
        None => SubscriptionScope::None,
    };
    bridge.detail.retarget(scope).await
}

/// Whether the daemon is reachable right now.
///
/// The webview may finish mounting after the link has already come up, and it would then
/// wait for an edge that has already passed.
#[tauri::command]
fn connected(bridge: State<'_, Bridge>) -> bool {
    bridge.primary.connected()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        // A download handed over from the browser must reach the window that is already
        // open, not a second copy of it with its own tray icon.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            chrome::reveal(app);
        }));
    }

    builder
        // Size, position and maximised state are restored; VISIBLE deliberately is not.
        // The window is created hidden so the boot splash can paint into it before it
        // appears, and a restored `visible` would undo that on the first frame.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        & !tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished {
                let _ = webview.window().show();
            }
        })
        .plugin(tauri_plugin_dialog::init())
        // A transfer the user walked away from is the one thing this app knows and they
        // cannot see. See `src/lib/notify.ts` for what it does and does not announce.
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            send,
            watch,
            connected,
            tray::tooltip
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let sink: Arc<dyn Sink> = Arc::new(ToWebview(handle.clone()));

            app.manage(Bridge {
                primary: Link::spawn(Role::Primary, SubscriptionScope::Summary, sink.clone()),
                detail: Link::spawn(Role::Detail, SubscriptionScope::None, sink),
            });

            if let Some(window) = app.get_webview_window("main") {
                chrome::dress(&window);
                // The window-state plugin restores a maximised window, and a window that
                // was already maximised when it opened never sends a resize.
                chrome::square_corners(&window.as_ref().window());
            }
            chrome::show_when_loaded(&handle);
            tray::install(&handle)?;
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                // Hide rather than exit: the tray keeps showing the aggregate readout, and
                // reopening is instant because nothing had to be torn down. Quitting is an
                // explicit act, in the tray menu — and it still does not stop a transfer.
                api.prevent_close();
                let _ = window.hide();
            }
            // Maximising, restoring and snapping all arrive as one event, which is exactly
            // the set of moments the corner has to change.
            tauri::WindowEvent::Resized(_) => chrome::square_corners(window),
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("the Vortex window failed to start");
}
