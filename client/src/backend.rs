//! Background work: login, the WebSocket connection, voice/screen and updates.
//!
//! The UI and the background talk through two channels (queues):
//!   - UI -> background: the UI sends a `Cmd` (command), e.g. "log in", "join voice".
//!   - background -> UI: the background sends back an `Event`, e.g. "logged in",
//!     "server message arrived".
//! The UI reads the waiting events at the start of every frame; whenever an event
//! is sent we also ask egui to repaint, so the UI reacts immediately.
//!
//! The background runs as async *tasks* on the tokio runtime. An async function
//! (`async fn`) doesn't block a thread while waiting (for the network, a timer...):
//! at every `.await` the task pauses and tokio runs other tasks meanwhile.
//! `tokio::spawn(future)` starts a task that runs independently.

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

/// WebSocket connection state, shown in the UI.
/// `#[derive(...)]`: Debug (printable), Clone/Copy (cheap copies), PartialEq/Eq (`==`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conn {
    Connecting,
    Connected,
    Reconnecting,
}

/// Commands from the UI to the background.
///
/// An enum where each variant can carry its own data — like a tagged union. The
/// receiver uses `match` to find out which command it got and to unpack the data.
pub enum Cmd {
    Login { username: String, password: String },
    Register { username: String, password: String, invite: String },
    /// Connect the WebSocket with a saved login token.
    Connect { token: String },
    Logout,
    /// A raw JSON message to send to the server over the WebSocket.
    Send(Value),
    Voice(VoiceCmd),
    ScreenSources,
    /// List apps that are playing audio (for app-audio sharing).
    AudioApps,
    /// Fetch the preview of a link posted in chat.
    Preview(String),
    CheckUpdate { manual: bool },
}

/// Voice-related commands. They are processed one at a time, in order, by a
/// single task (`voice_loop`), because WebRTC negotiation steps must not overlap.
pub enum VoiceCmd {
    Join { channel_id: i64, muted: bool, deafened: bool },
    Leave,
    Sync { participants: Vec<Participant>, watch: Option<String> },
    SetMuted(bool),
    SetDeafened(bool),
    ScreenStart { id: u64, kind: String },
    ScreenStop,
    MusicStart { pid: u32 },
    MusicStop,
    ApplyUpdate,
}

/// Events from the background to the UI.
pub enum Event {
    LoggedIn { token: String },
    AuthError(String),
    /// The login token is no longer valid: go back to the login screen.
    AuthExpired,
    Conn(Conn),
    Server(ServerMsg),
    VoiceJoined { channel_id: i64, session_id: String, tracks: Vec<String> },
    VoiceJoinFailed,
    VoiceLeft,
    Tracks(Vec<String>),
    Sources(Vec<SourceInfo>),
    AudioApps(Vec<crate::appaudio::AppAudio>),
    Preview { url: String, preview: Option<crate::preview::Preview> },
    Update { check: update::Check, manual: bool },
    UpdateFailed,
    Toast { text: String, error: bool },
}

/// Delivers events to the UI and makes the UI repaint.
///
/// Each background task gets its own clone of the Sink (`#[derive(Clone)]`).
/// `mpsc::Sender` = "multi-producer, single-consumer" channel sender: many senders,
/// one receiver (the UI).
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

/// The UI's handle to the background: send commands, receive events.
/// `pub` fields are readable from other modules (the UI reads `events` and `shared`).
pub struct Backend {
    tx: UnboundedSender<Cmd>,
    pub events: mpsc::Receiver<Event>,
    pub shared: Arc<Shared>,
}

impl Backend {
    /// Starts the background worker on the given tokio runtime.
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

/// The main background task: receives commands from the UI and dispatches them.
/// Long operations are spawned as separate tasks so the loop stays responsive.
async fn main_loop(base: String, mut rx: UnboundedReceiver<Cmd>, sink: Sink) {
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).build().unwrap();
    let mut token: Option<String> = None;
    let mut ws: Option<(UnboundedSender<String>, JoinHandle<()>)> = None;

    // Voice operations run in their own task, strictly one after another.
    // Each voice command is sent there together with the current login token.
    let (voice_tx, voice_rx) = unbounded_channel::<(String, VoiceCmd)>();
    tokio::spawn(voice_loop(base.clone(), voice_rx, sink.clone()));

    // `rx.recv().await` waits for the next command; it returns `None` when the
    // UI side is gone (app closing), which ends the loop.
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
            Cmd::AudioApps => {
                let sink = sink.clone();
                tokio::spawn(async move {
                    let list = tokio::task::spawn_blocking(crate::appaudio::list).await.unwrap_or_default();
                    sink.send(Event::AudioApps(list));
                });
            }
            Cmd::Preview(url) => {
                let sink = sink.clone();
                let http = http.clone();
                tokio::spawn(async move {
                    let preview = match crate::preview::fetch(&http, &url).await {
                        Ok(p) => Some(p),
                        Err(e) => {
                            log::debug!("önizleme yok ({url}): {e:#}");
                            None
                        }
                    };
                    sink.send(Event::Preview { url, preview });
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

/// Login or register: POST the body to /api/<path>; on success we get a token.
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

/// Keeps the WebSocket connection open; if it drops, reconnects with growing
/// delays (0.5 s, 1 s, 2 s, ... up to 10 s).
async fn ws_loop(base: String, token: String, mut out_rx: UnboundedReceiver<String>, sink: Sink) {
    let url = format!("{}/api/ws?token={}", base.replacen("http", "ws", 1), token);
    let mut retry: u32 = 0;
    loop {
        sink.send(Event::Conn(if retry == 0 { Conn::Connecting } else { Conn::Reconnecting }));
        // At most 10 s for the connection to open (otherwise retry with backoff).
        let attempt = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(&url))
            .await
            .unwrap_or_else(|_| Err(tungstenite::Error::Io(std::io::ErrorKind::TimedOut.into())));
        match attempt {
            Ok((stream, _)) => {
                retry = 0;
                sink.send(Event::Conn(Conn::Connected));
                // Split the socket into a writing half and a reading half so both can be used in the loop.
                let (mut write, mut read) = stream.split();
                let mut ping = tokio::time::interval(Duration::from_secs(15));
                // Send "ping" every 15 s so proxies don't close an idle connection.
                // (The first tick of an interval fires immediately; consume it here.)
                ping.tick().await;
                // When we last heard anything from the server. The server answers
                // every ping with "pong", so silence for 45 s means the connection
                // is dead even if the operating system hasn't noticed (e.g. the
                // Wi-Fi dropped for a moment). Without this check such a dead
                // connection could linger for minutes: the server and the others
                // no longer saw our voice state, so audio stopped until rejoining.
                let mut last_heard = std::time::Instant::now();
                loop {
                    // `select!` waits on several futures at once and runs the branch of whichever
                    // finishes first: an incoming message, an outgoing message from the UI, or the
                    // ping timer.
                    tokio::select! {
                        msg = read.next() => match msg {
                            Some(Ok(WsMessage::Text(t))) => {
                                last_heard = std::time::Instant::now();
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
                            if last_heard.elapsed() > Duration::from_secs(45) {
                                log::warn!("sunucudan 45 sn'dir yanıt yok, yeniden bağlanılıyor");
                                break;
                            }
                            if write.send(WsMessage::Text("ping".into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            // HTTP 401 = our token was rejected: log out instead of retrying forever.
            Err(tungstenite::Error::Http(resp)) if resp.status() == 401 => {
                sink.send(Event::AuthExpired);
                return;
            }
            Err(e) => log::warn!("WebSocket bağlanamadı: {e}"),
        }
        retry += 1;
        sink.send(Event::Conn(Conn::Reconnecting));
        // Exponential backoff: 500 ms << n = 500 · 2^n ms, capped at 10 s.
        let delay = (500u64 << retry.min(5)).min(10_000);
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
}

/// Voice channel operations: all processed in order, in a single task.
/// Every 3 seconds it also runs maintenance: reconnect if the WebRTC connection
/// broke, otherwise `heal()` (retry failed pulls, replace stalled audio).
async fn voice_loop(base: String, mut rx: UnboundedReceiver<(String, VoiceCmd)>, sink: Sink) {
    let factory = PeerConnectionFactory::default();
    let shared = sink.shared.clone();
    let mut call: Option<Call> = None;
    // Remembered so we can rejoin the same channel if the connection breaks.
    let mut current: Option<(i64, String)> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // While the call is being rebuilt after a break: when we started trying.
    // We keep retrying (every tick) for up to `GIVE_UP`, because the network is
    // often down for a while (Wi-Fi reconnecting); before, a single failed
    // attempt dropped us out of the channel.
    let mut rejoining: Option<std::time::Instant> = None;
    const GIVE_UP: Duration = Duration::from_secs(120);
    // Tracks whose failing pulls already caused one rebuild (see step 1 below).
    let mut blamed: std::collections::HashSet<String> = Default::default();

    loop {
        let (token, cmd) = tokio::select! {
            msg = rx.recv() => match msg {
                Some(m) => m,
                None => break,
            },
            _ = tick.tick() => {
                // 1) Is the current call broken? Then tear it down; step 2 rebuilds it.
                let mut reason = call.as_ref().and_then(|c| c.broken_reason());
                // Someone in the channel can't be pulled, again and again: our
                // session is probably stuck, so rebuild. Only once per track: if
                // the fault is on their side, rebuilding again wouldn't help.
                if let (None, Some(c)) = (reason, &call) {
                    let failing = c.failing_pulls();
                    if failing.iter().any(|k| !blamed.contains(k)) {
                        blamed.extend(failing);
                        reason = Some("kanaldaki birinin sesi alınamıyor");
                    }
                }
                if let Some(reason) = reason {
                    log::warn!("ses bağlantısı koptu ({reason}), yeniden katılınıyor");
                    if let Some(old) = call.take() {
                        old.leave();
                    }
                    rejoining = Some(std::time::Instant::now());
                }
                // 2) No call but we should be in a channel: (re)build it.
                if call.is_none() {
                    if let (Some(started), Some((channel_id, token))) = (rejoining, current.clone()) {
                        let muted = shared.muted.load(std::sync::atomic::Ordering::Relaxed);
                        let deafened = shared.deafened.load(std::sync::atomic::Ordering::Relaxed);
                        match Call::join(factory.clone(), Sfu::new(&base, &token), shared.clone(), muted).await {
                            Ok(c) => {
                                c.set_deafened(deafened);
                                let tracks = c.tracks().into_iter().map(String::from).collect();
                                sink.send(Event::VoiceJoined { channel_id, session_id: c.session_id.clone(), tracks });
                                sink.toast("Ses bağlantısı koptu, yeniden bağlanıldı", false);
                                call = Some(c);
                                rejoining = None;
                            }
                            Err(e) if started.elapsed() < GIVE_UP => {
                                log::warn!("yeniden katılınamadı, tekrar denenecek: {e:#}");
                            }
                            Err(e) => {
                                log::warn!("yeniden katılınamadı, vazgeçiliyor: {e:#}");
                                current = None;
                                rejoining = None;
                                sink.toast("Ses bağlantısı koptu, yeniden bağlanılamadı", true);
                                sink.send(Event::VoiceLeft);
                            }
                        }
                    }
                } else if let Some(c) = call.as_mut() {
                    c.heal().await;
                }
                continue;
            }
        };
        match cmd {
            VoiceCmd::Join { channel_id, muted, deafened } => {
                rejoining = None;
                blamed.clear();
                if let Some(old) = call.take() {
                    old.leave();
                }
                match Call::join(factory.clone(), Sfu::new(&base, &token), shared.clone(), muted).await {
                    Ok(c) => {
                        c.set_deafened(deafened);
                        let tracks = c.tracks().into_iter().map(String::from).collect();
                        sink.send(Event::VoiceJoined { channel_id, session_id: c.session_id.clone(), tracks });
                        call = Some(c);
                        current = Some((channel_id, token));
                    }
                    Err(e) => {
                        current = None;
                        sink.toast(format!("Ses kanalına bağlanılamadı: {e:#}"), true);
                        sink.send(Event::VoiceJoinFailed);
                    }
                }
            }
            VoiceCmd::Leave => {
                current = None;
                rejoining = None;
                blamed.clear();
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
                    Err(e) => {
                        sink.toast(format!("Ekran paylaşılamadı: {e:#}"), true);
                        sink.send(Event::Tracks(c.tracks().into_iter().map(String::from).collect()));
                    }
                }
            }
            VoiceCmd::ScreenStop => {
                let Some(c) = call.as_mut() else { continue };
                if let Err(e) = c.stop_screen().await {
                    sink.toast(format!("Paylaşım durdurulamadı: {e:#}"), true);
                }
                sink.send(Event::Tracks(c.tracks().into_iter().map(String::from).collect()));
            }
            VoiceCmd::MusicStart { pid } => {
                let Some(c) = call.as_mut() else { continue };
                if let Err(e) = c.start_music(crate::appaudio::Target::App(pid)).await {
                    sink.toast(format!("Ses paylaşılamadı: {e:#}"), true);
                }
                sink.send(Event::Tracks(c.tracks().into_iter().map(String::from).collect()));
            }
            VoiceCmd::MusicStop => {
                let Some(c) = call.as_mut() else { continue };
                if let Err(e) = c.stop_music().await {
                    sink.toast(format!("Ses paylaşımı durdurulamadı: {e:#}"), true);
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

/// Starts the new version of the app and exits this process.
fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
}
