//! Arka plan işleri: giriş, WebSocket bağlantısı, ses/ekran ve güncelleme.
//!
//! Arayüz `Cmd` gönderir, arka plan `Event` döner. Arayüz her karede gelen
//! olayları okur; yeni olay gelince ekran yeniden çizdirilir.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use libwebrtc::prelude::PeerConnectionFactory;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{self, Message as WsMessage};

use crate::api::ServerMsg;
use crate::screen::{self, SourceInfo};
use crate::sfu::Sfu;
use crate::update;
use crate::voice::{Call, Participant, Shared};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conn {
    Connecting,
    Connected,
    Reconnecting,
}

pub enum Cmd {
    Login { username: String, password: String },
    Register { username: String, password: String, invite: String },
    /// Kayıtlı token ile bağlan.
    Connect { token: String },
    Logout,
    /// Sunucuya ham WebSocket mesajı.
    Send(Value),
    Voice(VoiceCmd),
    ScreenSources,
    CheckUpdate { manual: bool },
}

pub enum VoiceCmd {
    Join { channel_id: i64, muted: bool, deafened: bool },
    Leave,
    Sync { participants: Vec<Participant>, watch: Option<String> },
    SetMuted(bool),
    SetDeafened(bool),
    ScreenStart { id: u64, kind: String },
    ScreenStop,
    ApplyUpdate,
}

pub enum Event {
    LoggedIn { token: String },
    AuthError(String),
    /// Token geçersiz: giriş ekranına dön.
    AuthExpired,
    Conn(Conn),
    Server(ServerMsg),
    VoiceJoined { channel_id: i64, session_id: String, tracks: Vec<String> },
    VoiceJoinFailed,
    VoiceLeft,
    Tracks(Vec<String>),
    Sources(Vec<SourceInfo>),
    Update { check: update::Check, manual: bool },
    UpdateFailed,
    Toast { text: String, error: bool },
}

/// Olayları arayüze iletir ve ekranı yeniden çizdirir.
#[derive(Clone)]
struct Sink {
    tx: mpsc::Sender<Event>,
    shared: Arc<Shared>,
}

impl Sink {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        self.shared.request_repaint();
    }
    fn toast(&self, text: impl Into<String>, error: bool) {
        self.send(Event::Toast { text: text.into(), error });
    }
}

pub struct Backend {
    tx: UnboundedSender<Cmd>,
    pub events: mpsc::Receiver<Event>,
    pub shared: Arc<Shared>,
}

impl Backend {
    pub fn start(rt: &tokio::runtime::Runtime, base: String) -> Self {
        crate::voice::set_runtime(rt.handle().clone());
        let shared = Arc::new(Shared::default());
        let (ev_tx, ev_rx) = mpsc::channel();
        let (tx, rx) = unbounded_channel();
        let sink = Sink { tx: ev_tx, shared: shared.clone() };
        rt.spawn(main_loop(base, rx, sink));
        Self { tx, events: ev_rx, shared }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn ws(&self, msg: Value) {
        self.send(Cmd::Send(msg));
    }

    pub fn voice(&self, cmd: VoiceCmd) {
        self.send(Cmd::Voice(cmd));
    }
}

async fn main_loop(base: String, mut rx: UnboundedReceiver<Cmd>, sink: Sink) {
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).build().unwrap();
    let mut token: Option<String> = None;
    let mut ws: Option<(UnboundedSender<String>, JoinHandle<()>)> = None;

    // Ses işlemleri sırayla çalışsın diye ayrı bir görevde.
    let (voice_tx, voice_rx) = unbounded_channel::<(String, VoiceCmd)>();
    tokio::spawn(voice_loop(base.clone(), voice_rx, sink.clone()));

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Login { username, password } => {
                let body = json!({ "username": username, "password": password });
                auth(&http, &base, "login", body, &sink).await;
            }
            Cmd::Register { username, password, invite } => {
                let body = json!({ "username": username, "password": password, "invite": invite });
                auth(&http, &base, "register", body, &sink).await;
            }
            Cmd::Connect { token: t } => {
                if let Some((_, h)) = ws.take() {
                    h.abort();
                }
                let (out_tx, out_rx) = unbounded_channel();
                let h = tokio::spawn(ws_loop(base.clone(), t.clone(), out_rx, sink.clone()));
                ws = Some((out_tx, h));
                token = Some(t);
            }
            Cmd::Logout => {
                let _ = voice_tx.send((String::new(), VoiceCmd::Leave));
                if let Some((_, h)) = ws.take() {
                    h.abort();
                }
                token = None;
            }
            Cmd::Send(msg) => {
                if let Some((out, _)) = &ws {
                    let _ = out.send(msg.to_string());
                }
            }
            Cmd::Voice(v) => {
                let _ = voice_tx.send((token.clone().unwrap_or_default(), v));
            }
            Cmd::ScreenSources => {
                let sink = sink.clone();
                tokio::spawn(async move {
                    match tokio::task::spawn_blocking(screen::list_sources).await {
                        Ok(list) => sink.send(Event::Sources(list)),
                        Err(e) => sink.toast(format!("Ekranlar listelenemedi: {e}"), true),
                    }
                });
            }
            Cmd::CheckUpdate { manual } => {
                let sink = sink.clone();
                tokio::spawn(async move {
                    match update::check().await {
                        Ok(check) => sink.send(Event::Update { check, manual }),
                        Err(e) if manual => sink.toast(format!("Güncelleme denetlenemedi: {e:#}"), true),
                        Err(e) => log::warn!("güncelleme kontrolü: {e:#}"),
                    }
                });
            }
        }
    }
}

async fn auth(http: &reqwest::Client, base: &str, path: &str, body: Value, sink: &Sink) {
    let res = async {
        let r = http.post(format!("{base}/api/{path}")).json(&body).send().await?;
        let ok = r.status().is_success();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if ok {
            if let Some(t) = v["token"].as_str() {
                return Ok(t.to_string());
            }
        }
        Err(anyhow::anyhow!(v["error"].as_str().unwrap_or("Beklenmeyen yanıt").to_string()))
    }
    .await;
    match res {
        Ok(token) => sink.send(Event::LoggedIn { token }),
        Err(e) => {
            let msg = if e.downcast_ref::<reqwest::Error>().is_some() {
                "Sunucuya ulaşılamadı".to_string()
            } else {
                e.to_string()
            };
            sink.send(Event::AuthError(msg));
        }
    }
}

/// WebSocket bağlantısını açık tutar; koparsa artan aralıklarla yeniden bağlanır.
async fn ws_loop(base: String, token: String, mut out_rx: UnboundedReceiver<String>, sink: Sink) {
    let url = format!("{}/api/ws?token={}", base.replacen("http", "ws", 1), token);
    let mut retry: u32 = 0;
    loop {
        sink.send(Event::Conn(if retry == 0 { Conn::Connecting } else { Conn::Reconnecting }));
        match tokio_tungstenite::connect_async(&url).await {
            Ok((stream, _)) => {
                retry = 0;
                sink.send(Event::Conn(Conn::Connected));
                let (mut write, mut read) = stream.split();
                let mut ping = tokio::time::interval(Duration::from_secs(30));
                ping.tick().await;
                loop {
                    tokio::select! {
                        msg = read.next() => match msg {
                            Some(Ok(WsMessage::Text(t))) => {
                                if t.as_str() == "pong" {
                                    continue;
                                }
                                match serde_json::from_str::<ServerMsg>(t.as_str()) {
                                    Ok(m) => sink.send(Event::Server(m)),
                                    Err(e) => log::debug!("bilinmeyen mesaj: {e}"),
                                }
                            }
                            Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                            _ => {}
                        },
                        out = out_rx.recv() => match out {
                            Some(text) => {
                                if write.send(WsMessage::Text(text.into())).await.is_err() {
                                    break;
                                }
                            }
                            None => return,
                        },
                        _ = ping.tick() => {
                            let _ = write.send(WsMessage::Text("ping".into())).await;
                        }
                    }
                }
            }
            Err(tungstenite::Error::Http(resp)) if resp.status() == 401 => {
                sink.send(Event::AuthExpired);
                return;
            }
            Err(e) => log::warn!("WebSocket bağlanamadı: {e}"),
        }
        retry += 1;
        sink.send(Event::Conn(Conn::Reconnecting));
        let delay = (500u64 << retry.min(5)).min(10_000);
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
}

/// Ses kanalı işlemleri: hepsi sırayla, tek görevde.
async fn voice_loop(base: String, mut rx: UnboundedReceiver<(String, VoiceCmd)>, sink: Sink) {
    let factory = PeerConnectionFactory::default();
    let shared = sink.shared.clone();
    let mut call: Option<Call> = None;

    while let Some((token, cmd)) = rx.recv().await {
        match cmd {
            VoiceCmd::Join { channel_id, muted, deafened } => {
                if let Some(old) = call.take() {
                    old.leave();
                }
                match Call::join(factory.clone(), Sfu::new(&base, &token), shared.clone(), muted).await {
                    Ok(c) => {
                        c.set_deafened(deafened);
                        let tracks = c.tracks().into_iter().map(String::from).collect();
                        sink.send(Event::VoiceJoined { channel_id, session_id: c.session_id.clone(), tracks });
                        call = Some(c);
                    }
                    Err(e) => {
                        sink.toast(format!("Ses kanalına bağlanılamadı: {e:#}"), true);
                        sink.send(Event::VoiceJoinFailed);
                    }
                }
            }
            VoiceCmd::Leave => {
                if let Some(c) = call.take() {
                    c.leave();
                    sink.send(Event::VoiceLeft);
                }
            }
            VoiceCmd::Sync { participants, watch } => {
                if let Some(c) = call.as_mut() {
                    c.sync(&participants, watch.as_deref()).await;
                }
            }
            VoiceCmd::SetMuted(m) => {
                if let Some(c) = &call {
                    c.set_muted(m);
                }
            }
            VoiceCmd::SetDeafened(d) => {
                if let Some(c) = &call {
                    c.set_deafened(d);
                }
            }
            VoiceCmd::ScreenStart { id, kind } => {
                let Some(c) = call.as_mut() else { continue };
                match c.start_screen(id, kind).await {
                    Ok(()) => sink.send(Event::Tracks(c.tracks().into_iter().map(String::from).collect())),
                    Err(e) => sink.toast(format!("Ekran paylaşılamadı: {e:#}"), true),
                }
            }
            VoiceCmd::ScreenStop => {
                let Some(c) = call.as_mut() else { continue };
                if let Err(e) = c.stop_screen().await {
                    sink.toast(format!("Paylaşım durdurulamadı: {e:#}"), true);
                }
                sink.send(Event::Tracks(c.tracks().into_iter().map(String::from).collect()));
            }
            VoiceCmd::ApplyUpdate => match update::apply().await {
                Ok(()) => {
                    if let Some(c) = call.take() {
                        c.leave();
                    }
                    restart();
                }
                Err(e) => {
                    sink.toast(format!("Güncellenemedi: {e:#}"), true);
                    sink.send(Event::UpdateFailed);
                }
            },
        }
    }
}

/// Yeni sürümü başlatıp bu süreci kapatır.
fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}
