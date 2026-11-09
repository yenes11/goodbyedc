//! Data types for the WebSocket messages exchanged with our server (a Cloudflare
//! Worker). The server sends JSON; serde turns it into these Rust structs.
//!
//! Only `Deserialize` (JSON -> Rust) is derived because we only *receive* these
//! types. Messages we send are built ad hoc with the `json!` macro elsewhere.

use serde::Deserialize;

// Common derives used below:
//   - Deserialize: build the struct from JSON.
//   - Clone: allow `.clone()` (a deep copy).
//   - Debug: allow printing with `{:?}` (useful in logs).
#[derive(Deserialize, Clone, Debug)]
pub struct User {
    // `i64` = signed 64-bit integer (database ids).
    pub id: i64,
    pub username: String,
}

// `#[allow(dead_code)]` silences the compiler warning about fields we never read.
#[derive(Deserialize, Clone, Debug)]
#[allow(dead_code)]
pub struct Server {
    pub id: i64,
    pub name: String,
}

// The server uses JavaScript-style `camelCase` names (`serverId`) while Rust uses
// `snake_case` (`server_id`). `rename_all` maps between the two automatically.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: i64,
    pub server_id: i64,
    pub name: String,
    /// "text" or "voice".
    pub kind: String,
}

// `impl Type { ... }` adds methods to a type.
impl Channel {
    /// `&self` = the method borrows the channel read-only.
    pub fn is_voice(&self) -> bool {
        self.kind == "voice"
    }
}

/// A chat message.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub channel_id: i64,
    pub user_id: i64,
    pub username: String,
    pub content: String,
    /// Unix time in milliseconds.
    pub created_at: i64,
}

/// Someone who is currently in a voice channel.
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct VoiceUser {
    pub user_id: i64,
    pub username: String,
    pub channel_id: i64,
    /// Their WebRTC session id on Cloudflare's SFU (used to pull their audio).
    pub session_id: String,
    /// What they publish: "mic", "screen" and/or "music".
    /// `Vec<T>` is a growable array. `#[serde(default)]` = empty if missing.
    #[serde(default)]
    pub tracks: Vec<String>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
}

impl VoiceUser {
    pub fn sharing_screen(&self) -> bool {
        // `.iter()` walks the vector; `.any(closure)` is true if the closure
        // returns true for at least one element. `|t| ...` is a closure taking `t`.
        self.tracks.iter().any(|t| t == "screen")
    }
}

/// Voice/video traffic usage (to stay inside Cloudflare's free quota).
/// `Copy` means it's small and copied implicitly (like an integer) instead of moved.
#[derive(Deserialize, Clone, Copy, Debug, Default)]
pub struct Usage {
    // Field-level rename for names that `rename_all` wouldn't produce ("GB").
    #[serde(rename = "usedGB")]
    pub used_gb: f64,
    #[serde(rename = "limitGB")]
    pub limit_gb: f64,
    /// Traffic that will drop out of the rolling window at the next midnight (UTC).
    /// Older servers don't send it, hence `Option` + `default`.
    #[serde(rename = "nextDropGB", default)]
    pub next_drop_gb: Option<f64>,
    /// When that drop happens (Unix ms).
    #[serde(rename = "nextDropAt", default)]
    pub next_drop_at: Option<i64>,
    #[serde(rename = "windowDays", default)]
    pub window_days: Option<u32>,
}

/// Every message the server can send, chosen by its `"t"` (type) field.
///
/// This is an `enum` whose variants carry data — Rust's way of saying "one of
/// these shapes". With `tag = "t"`, the JSON `{"t": "message", "message": {...}}`
/// becomes `ServerMsg::Message { message: ... }`. Variant names are converted to
/// `snake_case` (`ChannelCreated` <-> `"channel_created"`).
/// Code that receives a `ServerMsg` uses `match` to handle each variant.
#[derive(Deserialize, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Sent once right after connecting: everything needed to draw the app.
    Ready {
        me: User,
        servers: Vec<Server>,
        channels: Vec<Channel>,
        voice: Vec<VoiceUser>,
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// A new chat message.
    Message {
        message: Message,
    },
    /// A page of older messages for a channel (`more` = there are even older ones).
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
    /// The full list of who is in which voice channel (sent on every change).
    Voice {
        voice: Vec<VoiceUser>,
        #[serde(default)]
        usage: Option<Usage>,
    },
    /// Voice was switched off because the traffic quota is used up.
    VoiceDisabled {
        error: String,
    },
    /// Online presence changed (currently unused by the UI, so no fields).
    Presence {},
    Error {
        error: String,
    },
}
