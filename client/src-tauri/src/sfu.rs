//! Cloudflare Realtime SFU istemcisi.
//!
//! İstekler doğrudan Cloudflare'e değil, kendi Worker'ımıza (`/api/rtc/...`)
//! gider. Worker kullanıcıyı doğrular ve gizli App Token'ı ekleyip iletir.

use anyhow::{anyhow, bail, Result};
use libwebrtc::prelude::IceServer;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Sfu {
    http: reqwest::Client,
    base: String,
    token: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sdp {
    #[serde(rename = "type")]
    pub kind: String,
    pub sdp: String,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrackResult {
    pub mid: Option<String>,
    pub error_code: Option<String>,
    pub error_description: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TracksResponse {
    #[serde(default)]
    pub requires_immediate_renegotiation: bool,
    pub session_description: Option<Sdp>,
    #[serde(default)]
    pub tracks: Vec<TrackResult>,
    pub error_code: Option<String>,
    pub error_description: Option<String>,
}

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
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

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
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn ice_servers(&self) -> Result<Vec<IceServer>> {
        let v = self.call(reqwest::Method::GET, "/api/ice", None).await?;
        let r: IceResponse = serde_json::from_value(v)?;
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

    pub async fn new_session(&self) -> Result<String> {
        let v = self.call(reqwest::Method::POST, "/api/rtc/sessions/new", None).await?;
        v.get("sessionId")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("SFU oturumu oluşturulamadı: {v}"))
    }

    pub async fn tracks_new(&self, session: &str, body: Value) -> Result<TracksResponse> {
        let path = format!("/api/rtc/sessions/{session}/tracks/new");
        Self::check(serde_json::from_value(self.call(reqwest::Method::POST, &path, Some(body)).await?)?)
    }

    pub async fn tracks_close(&self, session: &str, body: Value) -> Result<TracksResponse> {
        let path = format!("/api/rtc/sessions/{session}/tracks/close");
        Self::check(serde_json::from_value(self.call(reqwest::Method::PUT, &path, Some(body)).await?)?)
    }

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
