//! Settings saved on disk (login token, mute state, last channel, volumes, ...).
//!
//! Location:
//!   - macOS:   ~/Library/Application Support/goodbyedc/settings.json
//!   - Windows: %APPDATA%\goodbyedc\settings.json
//!
//! (`//!` comments document the *enclosing* item, here the whole module/file.)

use std::collections::HashMap;
use std::path::PathBuf;

// serde is Rust's standard serialization library. Deriving `Serialize` and
// `Deserialize` (below) auto-generates the code that converts the struct to and
// from JSON.
use serde::{Deserialize, Serialize};

/// The server everyone connects to. For development it can be overridden with the
/// `GOODBYEDC_SERVER` environment variable.
pub fn server_url() -> String {
    // `std::env::var` returns `Result<String, _>`; `unwrap_or_else` supplies a
    // fallback when the variable isn't set. `.into()` converts the `&str` literal
    // into an owned `String`.
    std::env::var("GOODBYEDC_SERVER")
        .unwrap_or_else(|_| "https://goodbyedc.my-turn-server.workers.dev".into())
        .trim_end_matches('/')
        .to_string()
}

// `#[derive(...)]` asks the compiler to generate trait implementations:
//   - Serialize/Deserialize: JSON conversion (serde).
//   - Clone: lets us make a full copy with `.clone()`.
//
// `#[serde(default)]` on a field means: if the field is missing in the JSON file
// (e.g. the file was written by an older version of the app), use the type's
// default value instead of failing. `default = "fn_name"` uses a custom function.
#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    /// Login token from the server; `None` means "not logged in".
    /// `Option<T>` is Rust's "maybe a value": either `Some(value)` or `None`.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
    /// Server id -> last opened text channel.
    /// (JSON object keys are always strings, hence `String` instead of an integer.)
    #[serde(default)]
    pub last_channel: HashMap<String, i64>,
    #[serde(default)]
    pub last_server: Option<i64>,
    /// Is the input sensitivity (voice gate) automatic?
    #[serde(default = "yes")]
    pub input_auto: bool,
    /// Manually chosen input threshold in dB.
    #[serde(default = "default_threshold")]
    pub input_threshold_db: f32,
    /// User id -> playback volume (0..2, where 1 = 100%).
    #[serde(default)]
    pub user_volumes: HashMap<String, f32>,
    /// Normalize shared app audio so it reaches everyone at a steady loudness.
    #[serde(default = "yes")]
    pub music_normalize: bool,
}

// `Default` is the standard trait for "a sensible starting value". We implement it
// by hand (instead of deriving it) because some defaults are not zero/false.
impl Default for Settings {
    // `Self` is an alias for the type being implemented (`Settings`).
    fn default() -> Self {
        Self {
            token: None,
            muted: false,
            deafened: false,
            last_channel: HashMap::new(),
            last_server: None,
            input_auto: true,
            input_threshold_db: default_threshold(),
            user_volumes: HashMap::new(),
            music_normalize: true,
        }
    }
}

// Helper functions referenced by `#[serde(default = "...")]` above.
// The last expression of a function (without `;`) is its return value.
fn yes() -> bool {
    true
}

fn default_threshold() -> f32 {
    -45.0
}

/// The folder holding the settings and the log file.
/// `GOODBYEDC_CONFIG_DIR` overrides it (handy for testing with a fresh profile).
pub fn dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("GOODBYEDC_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    // `dirs::config_dir()` returns `Option<PathBuf>`; the `?` returns `None`
    // from this function if the OS has no config dir.
    Some(dirs::config_dir()?.join("goodbyedc"))
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("settings.json"))
}

impl Settings {
    /// Reads the settings file. Any problem (no file, broken JSON) silently falls
    /// back to the defaults: each `.and_then(...)` step yields `None` on failure,
    /// and `unwrap_or_default()` turns a final `None` into `Settings::default()`.
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Writes the settings as pretty-printed JSON. `&self` = we only read the
    /// settings here, so a shared borrow is enough.
    pub fn save(&self) {
        // `let ... else` : if the pattern doesn't match (path() is None), run the
        // `else` block, which must leave the function (here with `return`).
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(data) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, data);
        }
    }
}
