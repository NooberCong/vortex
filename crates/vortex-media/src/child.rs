//! Child processes that do not flash a console at the user (Windows).
//!
//! `vortexd` is windowless on Windows, and a windowless parent does *not* pass that on: a
//! console-subsystem child gets a console of its own, which is a black window on the
//! user's desktop for as long as the child runs. Both of this crate's children are
//! console programs, and one of them runs on a timer nobody asked for — channel 5 probes
//! the page on every navigation, so a YouTube session flashes a terminal over the browser
//! every few seconds. It reads as malware, and it is the first thing anyone reports.
//!
//! `CREATE_NO_WINDOW` is the flag that says "run this, but give it no console". It is
//! deliberately not `DETACHED_PROCESS`: the child keeps the pipes we hand it, which is how
//! its stdout is read and how a mux reports progress.
//!
//! `vortex_ipc::launch` sets the same flag on `vortexd` itself. Two copies rather than one
//! shared helper because the dependency would have to run the wrong way — the media
//! pipeline knows nothing about the IPC layer — and the shared part is a single constant.

/// Spawns without a console window on Windows; a no-op on every other platform.
pub(crate) fn windowless(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    {
        // `windows_sys` is not a dependency of this crate and this is the only constant
        // from it that matters here.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
