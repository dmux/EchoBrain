// Windows would otherwise open a console window behind the app. Harmless on
// macOS, and kept so the binary stays portable.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    echobrain_lib::run()
}
