// Thin binary entry point. Hides the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    league_tools_lib::run();
}
