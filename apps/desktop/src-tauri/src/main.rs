// A console window behind a GUI app is a Windows tell that something is a script. The
// attribute is release-only so `cargo run` still prints tracing output during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    vortex_app_lib::run();
}
