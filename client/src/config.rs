//! Kalıcı ayarlar (giriş token'ı, susturma durumu, son kanal).
//! macOS: ~/Library/Application Support/goodbyedc/settings.json
//! Windows: %APPDATA%\goodbyedc\settings.json

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Herkesin bağlandığı sunucu. Geliştirirken GOODBYEDC_SERVER ile değiştirilebilir.
pub fn server_url() -> String {
    std::env::var("GOODBYEDC_SERVER")
        .unwrap_or_else(|_| "https://goodbyedc.my-turn-server.workers.dev".into())
        .trim_end_matches('/')
        .to_string()
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
    /// sunucu id'si -> son açılan yazı kanalı
    #[serde(default)]
    pub last_channel: HashMap<String, i64>,
    #[serde(default)]
    pub last_server: Option<i64>,
}

fn path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("GOODBYEDC_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("settings.json"));
    }
    Some(dirs::config_dir()?.join("goodbyedc").join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(data) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, data);
        }
    }
}
