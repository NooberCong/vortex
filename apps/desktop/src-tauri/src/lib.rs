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
//! 3. keeps a tray icon alive so the aggregate readout survives a hidden window — and, when
//!    the login entry starts it with `--tray`, a window that never opened;
//! 4. relays exactly one thing in each direction, with no model of its own in between.
//!
//! Everything else — what a row looks like, what a number rounds to, when a sheet opens —
//! is in the frontend, where it can be changed without a compiler.

mod chrome;
mod link;
mod toast;
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
/// A `JobId` somebody outside the window asked to see.
///
/// Two things raise it and they mean the same sentence: a click on a finished transfer's
/// notification ([`toast`]), and a `Reveal` the daemon turned into [`REVEAL_FLAG`] on this
/// process's command line — which is how the browser extension reaches a window it cannot
/// see. One name for one intent, so the frontend has one listener rather than two doing
/// identical work.
pub const REVEAL: &str = "vortex://reveal";
/// How the login entry asks for a tray icon and no window.
///
/// The same spelling lives in `vortex_setup::autostart::TRAY_FLAG`, which is the code that
/// writes the entry. Neither crate depends on the other — this one is a view over the
/// daemon and that one writes registry keys — so the string is in both places, and each
/// says so.
const TRAY_FLAG: &str = "--tray";
/// How the daemon points this process at one job: `vortex-app --reveal 42`.
///
/// The same spelling lives in `crates/vortexd/src/daemon.rs`, which is the code that
/// writes it. Neither crate depends on the other — that one is a daemon and this one is a
/// window — so the string is in both places, and each side says so.
const REVEAL_FLAG: &str = "--reveal";

struct Bridge {
    primary: Arc<Link>,
    detail: Arc<Link>,
}

/// A job named on the command line, held until the webview is there to be told.
///
/// A cold `vortex-app --reveal 42` finishes parsing its arguments long before the webview
/// has mounted a listener, so emitting [`REVEAL`] then would be shouting into an empty
/// room. The frontend asks for this on mount instead — the same shape as [`connected`],
/// and for the same reason: an edge that has already passed has to be readable as state.
///
/// Taken rather than read. A window that has already acted on it must not act again when
/// the webview reloads.
#[derive(Default)]
struct Requested(std::sync::Mutex<Option<JobId>>);

/// The job this process was started to show, if it was started to show one. Clears on read.
#[tauri::command]
fn requested(pending: State<'_, Requested>) -> Option<JobId> {
    pending.0.lock().ok().and_then(|mut slot| slot.take())
}

/// The job id in `--reveal <id>`, if the arguments carry one.
///
/// Total by construction: no flag, no value after it, a value that is not a `u64` — every
/// one of them is `None`. These arguments come from a daemon this process did not start
/// and cannot vouch for, and the worst case has to be a window that opens without moving
/// the list, never one that refuses to open.
///
/// Deliberately untested here, which is unusual for this repository and worth the
/// sentence: this crate's `cargo test` binary does not load on Windows under a workspace
/// build (`STATUS_ENTRYPOINT_NOT_FOUND`, before `main`), so a `#[cfg(test)] mod tests`
/// added anywhere in `vortex-app` turns `cargo test --workspace` red for reasons that have
/// nothing to do with the code in it. That is why the function above is written to have no
/// failing branch rather than to have its failing branches covered.
fn requested_job(args: &[String]) -> Option<JobId> {
    let at = args.iter().position(|arg| arg == REVEAL_FLAG)?;
    args.get(at + 1)?.parse::<u64>().ok().map(JobId)
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

    // A person who starts Vortex means to look at it. The login entry does not: it wants
    // the daemon up and an icon in the tray, and a window unfolding by itself while someone
    // is still signing in is exactly what nobody asked for. Everything else — the webview,
    // the link to the daemon, the tray — is identical either way, so this is one bool and
    // the two places that would otherwise show the window.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let into_the_tray = args.iter().any(|arg| arg == TRAY_FLAG);
    // A `--reveal` is somebody asking to be shown something, which is the opposite of what
    // the login entry asks for. It wins.
    let asked_for = requested_job(&args);

    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        // A download handed over from the browser must reach the window that is already
        // open, not a second copy of it with its own tray icon. The second copy's
        // arguments are the message: `--reveal 42` from the daemon arrives here, and the
        // window that was already running is the one that acts on it.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            chrome::reveal(app);
            if let Some(job) = requested_job(&args) {
                // Emitted rather than stashed: this instance already has a webview with a
                // listener on it, which is the case `Requested` exists to cover the
                // absence of.
                let _ = app.emit(REVEAL, job);
            }
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
        .on_page_load(move |webview, payload| {
            if (!into_the_tray || asked_for.is_some())
                && payload.event() == tauri::webview::PageLoadEvent::Finished
            {
                let _ = webview.window().show();
            }
        })
        .plugin(tauri_plugin_dialog::init())
        // Only for the permission prompt. The notifications themselves are sent by
        // `toast`, because the plugin's desktop path cannot report a click on one.
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            send,
            watch,
            connected,
            requested,
            toast::notify,
            tray::tooltip
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let sink: Arc<dyn Sink> = Arc::new(ToWebview(handle.clone()));

            app.manage(Requested(std::sync::Mutex::new(asked_for)));
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
            // The page still loads, hidden — a tray start pays the webview's cold start at
            // sign-in so that opening the window later is instant, which is the same trade
            // the hide-on-close path already makes.
            if !into_the_tray || asked_for.is_some() {
                chrome::show_when_loaded(&handle);
            }
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
