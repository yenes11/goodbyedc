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

#[derive(Serialize, Deserialize, Clone)]
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
    /// Giriş hassasiyeti otomatik mi?
    #[serde(default = "yes")]
    pub input_auto: bool,
    /// Elle ayarlanan giriş eşiği (dB).
    #[serde(default = "default_threshold")]
    pub input_threshold_db: f32,
    /// Kullanıcı id'si -> ses yüksekliği (0..2, 1 = %100).
    #[serde(default)]
    pub user_volumes: HashMap<String, f32>,
}

impl Default for Settings {
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
        }
    }
}

fn yes() -> bool {
    true
}

fn default_threshold() -> f32 {
    -45.0
}

/// Ayarların ve log dosyasının bulunduğu klasör.
pub fn dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("GOODBYEDC_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(dirs::config_dir()?.join("goodbyedc"))
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("settings.json"))
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
