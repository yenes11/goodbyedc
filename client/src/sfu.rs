//! Client for Cloudflare's Realtime SFU (Selective Forwarding Unit).
//!
//! An SFU is a media server: each participant sends ("pushes") their audio/video
//! to it once, and it forwards copies to everyone who asks for them ("pulls").
//! That's much cheaper for each client than sending a copy to every other person.
//!
//! Requests don't go to Cloudflare directly but to our own Worker (`/api/rtc/...`).
//! The Worker checks that the user is logged in, adds the secret App Token and
//! forwards the request, so the token never ships inside the app.
//!
//! WebRTC terms used here:
//!   - SDP: a text description of a media connection (codecs, tracks, network info).
//!     Connecting is an "offer"/"answer" exchange of SDPs.
//!   - Track: one audio or video stream ("mic", "screen", "music").
//!   - mid: the id of a track's slot ("m-line") inside the connection.
//!   - Renegotiation: exchanging new SDPs when tracks are added/removed.
//!   - ICE servers: STUN/TURN servers that help connect through firewalls/NAT.

use anyhow::{anyhow, bail, Result};
use libwebrtc::prelude::IceServer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A handle for talking to the SFU on behalf of one logged-in user.
/// `Clone` is cheap: `reqwest::Client` is internally reference-counted.
#[derive(Clone)]
pub struct Sfu {
    http: reqwest::Client,
    /// Our server's base URL.
    base: String,
    /// The user's login token (sent as `Authorization: Bearer ...`).
    token: String,
}

/// An SDP offer or answer, in the JSON shape the SFU uses.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sdp {
    /// `type` is a reserved keyword in Rust, so the field is named `kind` and
    /// renamed to "type" in JSON.
    #[serde(rename = "type")]
    pub kind: String,
    pub sdp: String,
}

/// Per-track result of a tracks/new or tracks/close call.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrackResult {
    pub mid: Option<String>,
    pub error_code: Option<String>,
    pub error_description: Option<String>,
}

/// Response of tracks/new and tracks/close.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TracksResponse {
    /// True when the SFU sent a new offer that we must answer right away.
    #[serde(default)]
    pub requires_immediate_renegotiation: bool,
    pub session_description: Option<Sdp>,
    #[serde(default)]
    pub tracks: Vec<TrackResult>,
    pub error_code: Option<String>,
    pub error_description: Option<String>,
}

/// ICE server URLs can be a single string or a list in the JSON.
/// `untagged` makes serde try each variant until one fits.
#[derive(Deserialize)]
#[serde(untagged)]
enum Urls {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct IceEntry {
    urls: Urls,
    username: Option<String>,
    credential: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IceResponse {
    ice_servers: Vec<IceEntry>,
}

impl Sfu {
    /// `&str` parameters borrow the strings; we copy them into owned `String`s
    /// because the struct must own its data.
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    /// Makes one authenticated HTTP request and returns the JSON response.
    ///
    /// `async fn` returns a *future*: nothing runs until someone `.await`s it. While
    /// waiting for the network, `.await` lets tokio run other tasks on the same thread.
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut req = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await?;
        let status = res.status();
        let text = res.text().await?;
        if !status.is_success() {
            bail!("{path} -> HTTP {status}: {text}");
        }
        // An empty or non-JSON body becomes `Value::Null` instead of an error.
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Gets STUN/TURN servers (with short-lived TURN credentials) from our server.
    pub async fn ice_servers(&self) -> Result<Vec<IceServer>> {
        let v = self.call(reqwest::Method::GET, "/api/ice", None).await?;
        // `from_value` turns generic JSON (`Value`) into our typed struct.
        let r: IceResponse = serde_json::from_value(v)?;
        // `into_iter()` consumes the Vec (takes ownership of each element), `map`
        // converts each entry, and `collect()` builds the result Vec.
        Ok(r.ice_servers
            .into_iter()
            .map(|e| IceServer {
                urls: match e.urls {
                    Urls::One(u) => vec![u],
                    Urls::Many(u) => u,
                },
                username: e.username.unwrap_or_default(),
                password: e.credential.unwrap_or_default(),
            })
            .collect())
    }

    /// Creates a new SFU session and returns its id.
    pub async fn new_session(&self) -> Result<String> {
        let v = self.call(reqwest::Method::POST, "/api/rtc/sessions/new", None).await?;
        // Walk the JSON safely: each step returns an Option, and `ok_or_else`
        // turns a final `None` into an error.
        v.get("sessionId")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("SFU oturumu oluşturulamadı: {v}"))
    }

    /// Adds tracks to our session: either pushes our local tracks (body contains
    /// our SDP offer) or pulls remote ones (body names a remote session + track).
    pub async fn tracks_new(&self, session: &str, body: Value) -> Result<TracksResponse> {
        let path = format!("/api/rtc/sessions/{session}/tracks/new");
        Self::check(serde_json::from_value(self.call(reqwest::Method::POST, &path, Some(body)).await?)?)
    }

    /// Removes tracks (by mid) from our session.
    pub async fn tracks_close(&self, session: &str, body: Value) -> Result<TracksResponse> {
        let path = format!("/api/rtc/sessions/{session}/tracks/close");
        Self::check(serde_json::from_value(self.call(reqwest::Method::PUT, &path, Some(body)).await?)?)
    }

    /// Sends our SDP answer to an offer the SFU made.
    pub async fn renegotiate(&self, session: &str, answer: Sdp) -> Result<()> {
        let path = format!("/api/rtc/sessions/{session}/renegotiate");
        let v = self
            .call(reqwest::Method::PUT, &path, Some(json!({ "sessionDescription": answer })))
            .await?;
        if let Some(e) = v.get("errorCode") {
            bail!("renegotiate hatası: {e} {}", v.get("errorDescription").unwrap_or(&Value::Null));
        }
        Ok(())
    }

    /// The SFU reports errors inside a successful (HTTP 200) response, either for
    /// the whole call or per track. Turn those into real errors.
    fn check(r: TracksResponse) -> Result<TracksResponse> {
        if let Some(code) = &r.error_code {
            bail!("SFU hatası: {code} {}", r.error_description.clone().unwrap_or_default());
        }
        for t in &r.tracks {
            if let Some(code) = &t.error_code {
                bail!("SFU track hatası: {code} {}", t.error_description.clone().unwrap_or_default());
            }
        }
        Ok(r)
    }
}
