// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    if intelligent_hz_changer_lib::run_overlay_if_requested() {
        return;
    }
    intelligent_hz_changer_lib::run()
}
