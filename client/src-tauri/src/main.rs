// Windows'ta release modunda ekstra konsol penceresi açılmasın.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    goodbyedc_lib::run()
}
