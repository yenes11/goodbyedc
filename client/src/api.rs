//! Sunucuyla (Cloudflare Worker) konuşulan WebSocket mesajları.

use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct User {
    pub id: i64,
    pub username: String,
}

#[derive(Deserialize, Clone, Debug)]
#[allow(dead_code)]
pub struct Server {
    pub id: i64,
    pub name: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: i64,
    pub server_id: i64,
    pub name: String,
    pub kind: String,
}

impl Channel {
    pub fn is_voice(&self) -> bool {
        self.kind == "voice"
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub channel_id: i64,
    pub user_id: i64,
    pub username: String,
    pub content: String,
    pub created_at: i64,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct VoiceUser {
    pub user_id: i64,
    pub username: String,
    pub channel_id: i64,
    pub session_id: String,
    #[serde(default)]
    pub tracks: Vec<String>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
}

impl VoiceUser {
    pub fn sharing_screen(&self) -> bool {
        self.tracks.iter().any(|t| t == "screen")
    }
}

#[derive(Deserialize, Clone, Copy, Debug, Default)]
pub struct Usage {
    #[serde(rename = "usedGB")]
    pub used_gb: f64,
    #[serde(rename = "limitGB")]
    pub limit_gb: f64,
    /// Bir sonraki gece yarısında (UTC) hesaptan düşecek trafik (eski sunucuda yok).
    #[serde(rename = "nextDropGB", default)]
    pub next_drop_gb: Option<f64>,
    /// Düşüşün olacağı an (Unix ms).
    #[serde(rename = "nextDropAt", default)]
    pub next_drop_at: Option<i64>,
    #[serde(rename = "windowDays", default)]
    pub window_days: Option<u32>,
}

/// Sunucudan gelen mesajlar ("t" alanına göre).
#[derive(Deserialize, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Ready {
        me: User,
        servers: Vec<Server>,
        channels: Vec<Channel>,
        voice: Vec<VoiceUser>,
        #[serde(default)]
        usage: Option<Usage>,
    },
    Message {
        message: Message,
    },
    History {
        #[serde(rename = "channelId")]
        channel_id: i64,
        messages: Vec<Message>,
        more: bool,
    },
    ServerCreated {
        server: Server,
    },
    ServerDeleted {
        #[serde(rename = "serverId")]
        server_id: i64,
    },
    ChannelCreated {
        channel: Channel,
    },
    ChannelDeleted {
        #[serde(rename = "channelId")]
        channel_id: i64,
    },
    Voice {
        voice: Vec<VoiceUser>,
        #[serde(default)]
        usage: Option<Usage>,
    },
    VoiceDisabled {
        error: String,
    },
    Presence {},
    Error {
        error: String,
    },
}
