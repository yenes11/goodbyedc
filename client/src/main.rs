// Entry point of the desktop app.
//
// A quick map of the code (each `mod x;` below pulls in the file `src/x.rs`):
//   - main.rs      : starts logging, the async runtime, the backend and the window.
//   - app.rs       : the whole user interface (drawn every frame with egui).
//   - backend.rs   : background work: login, WebSocket to the server, voice calls.
//   - voice.rs     : WebRTC voice/screen connection to Cloudflare's SFU.
//   - sfu.rs       : small HTTP client for the SFU API (proxied by our server).
//   - api.rs       : data types exchanged with the server (JSON <-> Rust structs).
//   - config.rs    : settings saved on disk (login token, volumes, ...).
//   - theme.rs / widgets.rs : colors, fonts and custom-drawn UI pieces.
//   - screen.rs, appaudio.rs, meter.rs, mictest.rs, normalize.rs, ducking.rs :
//     screen capture, app-audio capture and other audio helpers.
//   - preview.rs   : link previews in chat. update.rs : self-update from GitHub.
//
// The line below is an *attribute* on the whole crate (`#!` = applies to the
// enclosing item, here the crate). In Windows release builds it marks the exe as
// a GUI program, so no black console window pops up behind the app.
// `debug_assertions` is on for `cargo run` (debug) builds and off for `--release`.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// Rust doesn't discover source files automatically: every module must be
// declared. `mod api;` means "compile src/api.rs as the module `api`".
mod api;
mod appaudio;
mod app;
mod backend;
mod config;
mod ducking;
mod meter;
mod mictest;
mod normalize;
mod preview;
mod screen;
mod sfu;
mod theme;
mod update;
mod voice;
mod widgets;
// This module only exists when compiling for macOS.
#[cfg(target_os = "macos")]
mod macos;

// `use` brings a name into scope so we can write `egui::...` instead of
// `eframe::egui::...`. (eframe is the windowing layer, egui is the UI library.)
use eframe::egui;

// `main` returns `eframe::Result`, i.e. `Result<(), eframe::Error>`. Rust has no
// exceptions: functions that can fail return `Result<OkValue, ErrorValue>` and the
// caller must handle both cases.
fn main() -> eframe::Result {
    init_logging();
    // `log::info!` is a macro (macros end with `!`). `{}` placeholders are filled
    // from the following arguments, like printf. `env!(...)` reads a value at
    // *compile time*: here the version from Cargo.toml.
    log::info!("goodbyedc {} başladı ({})", env!("CARGO_PKG_VERSION"), std::env::consts::OS);
    // Clean up files left over from a previous self-update.
    update::startup();

    // Developer helper (only in debug builds): `GOODBYEDC_METER_TEST=1 cargo run`
    // prints microphone levels for a few seconds and exits.
    #[cfg(debug_assertions)]
    if std::env::var("GOODBYEDC_METER_TEST").is_ok() {
        let m = meter::Meter::start().expect("ölçer açılamadı");
        // `0..20` is a range: 0, 1, ..., 19. `_` means "I don't need the value".
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(150));
            let l = m.take_level();
            // `{l:.4}` = print variable `l` with 4 decimals.
            println!("seviye {l:.4} ({:.0} dB)", voice::level_db(l));
        }
        return Ok(());
    }

    // Tokio is the async runtime: it runs many "tasks" (lightweight async
    // functions) on a small pool of threads. Networking, WebRTC signalling etc.
    // run there, while the UI runs on the main thread.
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio başlatılamadı");
    // Start the background worker. `&rt` passes a *reference* (a borrow) to the
    // runtime: `start` may use it but doesn't take ownership, so `rt` stays ours.
    let backend = backend::Backend::start(&rt, config::server_url());

    // Window icon: raw RGBA pixels embedded in the binary at compile time.
    // `include_bytes!` gives a `&'static [u8]`; `.to_vec()` copies it into a Vec.
    let icon = egui::IconData {
        rgba: include_bytes!("../icons/icon-256.rgba").to_vec(),
        width: 256,
        height: 256,
    };
    // "Builder pattern": each `.with_x(...)` returns a modified copy, so calls can
    // be chained. `mut` is required because we reassign `viewport` below
    // (variables are immutable by default in Rust).
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("goodbyedc")
        .with_app_id("goodbyedc")
        .with_inner_size([1200.0, 760.0])
        .with_min_inner_size([860.0, 520.0])
        .with_icon(icon);
    if cfg!(target_os = "macos") {
        // macOS: keep the system traffic-light buttons but let our content extend
        // under the (now transparent) title bar, like modern Mac apps.
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    } else {
        // Windows/Linux: frameless window; we draw minimize/maximize/close ourselves.
        viewport = viewport.with_decorations(false);
    }
    // `..Default::default()` = "all other fields take their default values".
    let options = eframe::NativeOptions { viewport, ..Default::default() };

    // Make the tokio runtime "current" for this thread, so code running on the UI
    // thread can call `tokio::spawn` too. The guard undoes it when dropped (at the
    // end of `main`); the leading `_` only silences the "unused variable" warning
    // while still keeping the guard alive.
    let _guard = rt.enter();
    // Open the window and run the UI loop until the window closes.
    // `Box::new(move |cc| ...)` is a closure (anonymous function) stored on the
    // heap; `move` means it takes ownership of the variables it uses (`backend`).
    eframe::run_native(
        "goodbyedc",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, backend)))),
    )
}

/// Sends log lines both to the console and to `goodbyedc.log` in the settings
/// folder (Windows release builds have no console, so the file is the only way to
/// read logs there). The previous run's log is kept as `goodbyedc.old.log`.
///
/// (Comments starting with `///` are *doc comments*: they document the item that
/// follows and show up in generated documentation and editor tooltips.)
fn init_logging() {
    // Bring the `Write` trait into scope: it provides `writeln!`/`write_all` on
    // writers. In Rust, trait methods are only callable when the trait is in scope.
    use std::io::Write;
    // Default log filter: our own crate at "info" level (overridable with RUST_LOG).
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("goodbyedc=info"));
    // Line format: "HH:MM:SS.mmm LEVEL message".
    builder.format(|buf, rec| {
        writeln!(buf, "{} {:5} {}", chrono::Local::now().format("%H:%M:%S%.3f"), rec.level(), rec.args())
    });
    // `config::dir()` returns `Option<PathBuf>` (maybe a path, maybe nothing).
    // `and_then` runs the closure only if there is a value. Inside, `?` on an Option
    // means "if this is None, return None from the closure right away".
    let file = config::dir().and_then(|d| {
        std::fs::create_dir_all(&d).ok()?;
        let path = d.join("goodbyedc.log");
        // `let _ = ...` deliberately ignores the result (the old file may not exist).
        let _ = std::fs::rename(&path, d.join("goodbyedc.old.log"));
        std::fs::File::create(path).ok()
    });
    // `if let Some(x) = opt` runs the block only when the Option has a value.
    if let Some(file) = file {
        builder.target(env_logger::Target::Pipe(Box::new(Tee(file))));
    }
    builder.init();
}

/// Writes everything both to a file and to standard error (the console).
///
/// This is a *tuple struct*: a struct with one unnamed field, accessed as `self.0`.
struct Tee(std::fs::File);

// Implementing the standard `Write` trait makes `Tee` usable anywhere a writer is
// expected (here: as the logger's output). A trait is like an interface.
impl std::io::Write for Tee {
    // `&mut self` = this method may modify the Tee (it writes to its file).
    // `&[u8]` is a borrowed slice of bytes.
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(b);
        // `?` = if writing to the file fails, return that error from `write`.
        self.0.write_all(b)?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
