// Windows'ta yayın sürümünde arkada konsol penceresi açılmasın.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod app;
mod backend;
mod config;
mod screen;
mod sfu;
mod theme;
mod update;
mod voice;
mod widgets;
#[cfg(target_os = "macos")]
mod macos;

use eframe::egui;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("goodbyedc=info")).init();
    update::startup();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio başlatılamadı");
    let backend = backend::Backend::start(&rt, config::server_url());

    let icon = egui::IconData {
        rgba: include_bytes!("../icons/icon-256.rgba").to_vec(),
        width: 256,
        height: 256,
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("goodbyedc")
        .with_app_id("goodbyedc")
        .with_inner_size([1200.0, 760.0])
        .with_min_inner_size([860.0, 520.0])
        .with_icon(icon);
    if cfg!(target_os = "macos") {
        // Sistem düğmeleri kalır, başlık çubuğu içeriğin üstüne biner.
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    } else {
        // Windows/Linux: çerçevesiz pencere, düğmeleri kendimiz çizeriz.
        viewport = viewport.with_decorations(false);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };

    let _guard = rt.enter();
    eframe::run_native(
        "goodbyedc",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, backend)))),
    )
}
