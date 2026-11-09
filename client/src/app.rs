//! App state and the user interface (egui).
//!
//! How this file is organized:
//!   - Small helper types (ChannelMsgs, MyVoice, Modal, ...) and the big `App`
//!     struct that holds *all* UI state.
//!   - "Events": `handle_events` applies what the background sent (server messages,
//!     voice joined, previews...), `handle_server` handles each server message.
//!   - "Actions": methods that change state and send commands (join voice, mute...).
//!   - "Drawing": `impl eframe::App for App` — eframe calls `ui()` every frame, and
//!     it draws the whole window from the current state (immediate mode).
//!   - Free functions at the bottom for repeated pieces (channel row, message row...).
//!
//! A key Rust idea used everywhere here: `&mut self` methods get exclusive
//! (mutable) access to the App while they run. The borrow checker makes sure no
//! other code reads or writes the App at the same time, so there are no data
//! races — the UI state is only ever touched on the UI thread.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Id, Key, Layout, Margin, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, TextEdit, TextureHandle, TextureOptions, Ui, Vec2,
};
use egui_phosphor::regular as ph;
use serde_json::json;

use crate::api::{Channel, Message, Server, ServerMsg, Usage, User, VoiceUser};
use crate::backend::{Backend, Cmd, Conn, Event, VoiceCmd};
use crate::config::Settings;
use crate::appaudio::{self, AppAudio};
use crate::screen::SourceInfo;
use crate::theme::*;
use crate::voice::{level_db, music_owner, Gate, Levels, Participant};
use crate::widgets::{self, Tone};

/// Height of the top bars (the macOS window buttons are centered in it).
const BAR: f32 = 46.0;
/// Sidebar width.
const SIDEBAR_W: f32 = 248.0;
/// Audio level above which someone counts as "speaking" (green ring).
const SPEAK_LEVEL: f32 = 0.02;
/// How often to check for updates automatically.
const UPDATE_EVERY: Duration = Duration::from_secs(30 * 60);
/// `cfg!(...)` is evaluated at compile time to a plain `true`/`false`.
const IS_MAC: bool = cfg!(target_os = "macos");

/// Messages loaded for one channel.
#[derive(Default)]
struct ChannelMsgs {
    list: Vec<Message>,
    more: bool,
    loading: bool,
}

/// Our own voice state while in a voice channel.
struct MyVoice {
    channel_id: i64,
    session_id: String,
    tracks: Vec<String>,
}

/// Tabs of the screen-share window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceTab {
    Screens,
    Windows,
}

/// Preview state of a link posted in chat.
enum PreviewState {
    Loading,
    Ready(PreviewView),
    /// No preview (the page gave no info or couldn't be opened).
    Empty,
}

/// A ready preview: texts plus the image uploaded to the GPU as a texture.
struct PreviewView {
    site: Option<String>,
    title: Option<String>,
    desc: Option<String>,
    image: Option<TextureHandle>,
}

/// Link color that's readable on the dark background.
const LINK: Color32 = Color32::from_rgb(0x5b, 0x9b, 0xff);

/// Which modal (dialog window) is open, with the state it needs.
/// `App.modal` is an `Option<Modal>`: `None` = no modal open.
enum Modal {
    CreateChannel { kind: &'static str, name: String },
    DeleteChannel { id: i64, name: String },
    ScreenShare { sources: Option<Vec<SourceInfo>>, tab: SourceTab, selected: Option<String> },
    Settings,
    /// Share app audio (e.g. Spotify).
    AudioShare { apps: Option<Vec<AppAudio>>, selected: Option<u32> },
}

/// Contents of the login / register form.
struct AuthForm {
    username: String,
    password: String,
    invite: String,
    registering: bool,
    error: String,
    busy: bool,
}

/// All of the app's state. eframe keeps one `App` alive for the whole run and
/// calls its `ui` method every frame.
pub struct App {
    backend: Backend,
    settings: Settings,
    logo: TextureHandle,

    auth: AuthForm,
    logged_in: bool,
    conn: Conn,

    me: Option<User>,
    servers: Vec<Server>,
    channels: Vec<Channel>,
    voice: Vec<VoiceUser>,
    usage: Option<Usage>,
    server_id: Option<i64>,
    channel_id: Option<i64>,
    messages: HashMap<i64, ChannelMsgs>,
    /// Content height before older messages were prepended (to keep the scroll position).
    scroll_fix: Option<f32>,
    /// Scroll the chat to the bottom for this many frames (when a channel opens / first loads).
    scroll_bottom: u8,
    last_content_h: f32,
    composer: String,

    my_voice: Option<MyVoice>,
    /// Voice channel we're currently joining.
    voice_joining: Option<i64>,
    /// Screen sharing is starting/stopping.
    screen_busy: bool,
    /// App-audio sharing is starting/stopping.
    music_busy: bool,
    watching: Option<String>,
    screen_tex: Option<(String, TextureHandle)>,
    speaking: HashSet<String>,
    /// Link previews (url -> state).
    previews: HashMap<String, PreviewState>,
    /// Last measured audio levels (for the meter in the settings window).
    levels: Levels,
    /// The microphone test in settings ("hear yourself"), while running.
    mic_test: Option<crate::mictest::MicTest>,
    /// Level last measured by the microphone test (0..1).
    mic_test_level: f32,

    modal: Option<Modal>,
    modal_focus: bool,
    toast: Option<(String, bool, Instant)>,

    update_available: bool,
    update_busy: bool,
    update_checking: bool,
    last_update_check: Option<Instant>,

    #[cfg(debug_assertions)]
    screenshot: Option<(String, u32)>,
}

impl App {
    /// Called once when the window opens. `cc` gives access to the egui context
    /// (fonts, textures, ...).
    pub fn new(cc: &eframe::CreationContext<'_>, backend: Backend) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_style(&cc.egui_ctx);
        *backend.shared.repaint.lock() = Some(cc.egui_ctx.clone());

        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([128, 128], include_bytes!("../icons/logo-128.rgba")),
            TextureOptions::LINEAR,
        );

        let settings = Settings::load();
        let logged_in = settings.token.is_some()
            && !(cfg!(debug_assertions) && std::env::var("GOODBYEDC_DEMO").as_deref() == Ok("auth"));
        if let Some(t) = &settings.token {
            backend.send(Cmd::Connect { token: t.clone() });
        }

        Self {
            backend,
            settings,
            logo,
            auth: AuthForm {
                username: String::new(),
                password: String::new(),
                invite: String::new(),
                registering: false,
                error: String::new(),
                busy: false,
            },
            logged_in,
            conn: Conn::Connecting,
            me: None,
            servers: vec![],
            channels: vec![],
            voice: vec![],
            usage: None,
            server_id: None,
            channel_id: None,
            messages: HashMap::new(),
            scroll_fix: None,
            scroll_bottom: 0,
            last_content_h: 0.0,
            composer: String::new(),
            my_voice: None,
            voice_joining: None,
            screen_busy: false,
            music_busy: false,
            watching: None,
            screen_tex: None,
            speaking: HashSet::new(),
            previews: HashMap::new(),
            levels: Levels::default(),
            mic_test: None,
            mic_test_level: 0.0,
            modal: None,
            modal_focus: false,
            toast: None,
            update_available: false,
            update_busy: false,
            update_checking: false,
            last_update_check: None,
            #[cfg(debug_assertions)]
            screenshot: std::env::var("GOODBYEDC_SCREENSHOT").ok().map(|p| (p, 0)),
        }
    }

    // ------------------------------------------------------------ helpers

    /// Shows a short message at the bottom of the window for a few seconds.
    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some((text.into(), error, Instant::now()));
    }

    /// Sends a JSON message to the server over the WebSocket.
    fn ws(&self, v: serde_json::Value) {
        self.backend.ws(v);
    }

    /// Looks up a channel by id. Returns a *borrowed* `&Channel` (no copy).
    fn channel(&self, id: i64) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    // --------------------------------------------------------------- events

    /// Development: open a specific window at startup with GOODBYEDC_DEMO (for screenshot tests).
    #[cfg(debug_assertions)]
    fn demo(&mut self) {
        let Ok(d) = std::env::var("GOODBYEDC_DEMO") else { return };
        if self.me.is_none() || self.modal.is_some() || self.toast.is_some() {
            return;
        }
        match d.as_str() {
            "create" => {
                self.modal = Some(Modal::CreateChannel { kind: "voice", name: "Oyun Odası".into() });
            }
            "share" => {
                let src = |id, title: &str, kind: &str| SourceInfo { id, title: title.into(), kind: kind.into() };
                self.modal = Some(Modal::ScreenShare {
                    sources: Some(vec![src(1, "Ekran 1", "screen"), src(2, "Ekran 2", "screen"), src(3, "Safari", "window")]),
                    tab: SourceTab::Screens,
                    selected: Some("1".into()),
                });
            }
            "toast" => self.toast("Ses kanalına bağlanılıyor…", false),
            d if d.starts_with("channel:") => {
                let name = &d["channel:".len()..];
                if let Some(id) = self.channels.iter().find(|c| c.name == name).map(|c| c.id) {
                    if self.channel_id != Some(id) {
                        self.select_channel(id);
                    }
                }
            }
            "fill" => {
                // Fill the channel with fake messages for scroll testing.
                if let Some(ch) = self.channel_id {
                    let c = self.messages.entry(ch).or_default();
                    if !c.loading && c.list.len() < 50 {
                        let base = 1_700_000_000_000i64;
                        for i in 0..60i64 {
                            let (uid, name) = if (i / 4) % 2 == 0 { (1, "Venovis") } else { (2, "jadaya2") };
                            c.list.push(Message {
                                id: 10_000 + i,
                                channel_id: ch,
                                user_id: uid,
                                username: name.into(),
                                content: format!("deneme mesajı {i}"),
                                created_at: base + i * 1000,
                            });
                        }
                        c.more = false;
                        self.scroll_bottom = 3;
                    }
                }
            }
            "music" => {
                let a = |pid, name: &str| AppAudio { pid, name: name.into() };
                self.modal = Some(Modal::AudioShare {
                    apps: Some(vec![a(1, "Chrome"), a(2, "Spotify"), a(3, "Steam")]),
                    selected: Some(2),
                });
            }
            "settings" => self.modal = Some(Modal::Settings),
            _ => {}
        }
    }

    /// Applies everything the background sent since the last frame, then updates
    /// derived state (who is speaking, new screen frame, volumes for the voice loop).
    /// `try_recv` takes one waiting event without blocking; `while let` repeats until
    /// the queue is empty.
    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.backend.events.try_recv() {
            match ev {
                Event::LoggedIn { token } => {
                    self.auth.busy = false;
                    self.auth.password.clear();
                    self.settings.token = Some(token.clone());
                    self.settings.save();
                    self.logged_in = true;
                    self.backend.send(Cmd::Connect { token });
                }
                Event::AuthError(e) => {
                    self.auth.busy = false;
                    self.auth.error = e;
                }
                Event::AuthExpired => self.logout(),
                Event::Conn(c) => self.conn = c,
                Event::Server(m) => self.handle_server(m),
                Event::VoiceJoined { channel_id, session_id, tracks } => {
                    self.voice_joining = None;
                    self.ws(json!({ "t": "voice_join", "channelId": channel_id, "sessionId": session_id,
                        "tracks": tracks, "muted": self.settings.muted, "deafened": self.settings.deafened }));
                    self.my_voice = Some(MyVoice { channel_id, session_id, tracks });
                }
                Event::VoiceJoinFailed => self.voice_joining = None,
                Event::VoiceLeft => {
                    // The background left voice by itself (rejoining failed): update the UI too.
                    if self.my_voice.take().is_some() {
                        self.ws(json!({ "t": "voice_leave" }));
                        self.watching = None;
                        self.screen_tex = None;
                        self.screen_busy = false;
                    }
                }
                Event::Tracks(tracks) => {
                    self.screen_busy = false;
                    self.music_busy = false;
                    if let Some(v) = &mut self.my_voice {
                        v.tracks = tracks.clone();
                        self.ws(json!({ "t": "voice_update", "tracks": tracks }));
                    }
                }
                Event::Preview { url, preview } => {
                    let state = match preview {
                        Some(p) => {
                            let image = p.image.map(|(w, h, rgba)| {
                                let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
                                ctx.load_texture(format!("onizleme:{url}"), img, TextureOptions::LINEAR)
                            });
                            PreviewState::Ready(PreviewView { site: p.site, title: p.title, desc: p.desc, image })
                        }
                        None => PreviewState::Empty,
                    };
                    self.previews.insert(url, state);
                }
                Event::AudioApps(list) => {
                    if let Some(Modal::AudioShare { apps, selected }) = &mut self.modal {
                        // Pick Spotify if it's there, otherwise the first app.
                        *selected = list
                            .iter()
                            .find(|a| a.name.to_lowercase().contains("spotify"))
                            .or(list.first())
                            .map(|a| a.pid);
                        *apps = Some(list);
                    }
                }
                Event::Sources(list) => {
                    if list.is_empty() {
                        self.modal = None;
                        self.toast(
                            "Paylaşılabilecek ekran bulunamadı. macOS'ta Sistem Ayarları > Gizlilik > Ekran Kaydı iznini kontrol et.",
                            true,
                        );
                    } else if let Some(Modal::ScreenShare { sources, tab, selected }) = &mut self.modal {
                        if !list.iter().any(|s| s.kind == "screen") {
                            *tab = SourceTab::Windows;
                        }
                        let want = if *tab == SourceTab::Screens { "screen" } else { "window" };
                        *selected = list.iter().find(|s| s.kind == want).map(|s| s.id.to_string());
                        *sources = Some(list);
                    }
                }
                Event::Update { check, manual } => {
                    self.update_checking = false;
                    self.update_available = check.available;
                    if manual {
                        if !check.supported {
                            self.toast("Bu sürüm elle derlendiği için otomatik güncellenmiyor", false);
                        } else if !check.available {
                            self.toast("Uygulama güncel", false);
                        }
                    }
                }
                Event::UpdateFailed => self.update_busy = false,
                Event::Toast { text, error } => {
                    if text.contains("güncelleme") || text.contains("Güncelleme") {
                        self.update_checking = false;
                    }
                    self.toast(text, error)
                }
            }
        }

        // Pass audio settings to the voice loop (sensitivity and per-user volumes).
        *self.backend.shared.gate.lock() =
            Gate { auto: self.settings.input_auto, threshold_db: self.settings.input_threshold_db };
        self.backend.shared.music_normalize.store(self.settings.music_normalize, Ordering::Relaxed);

        // The microphone test only runs while the settings window is open.
        if !matches!(self.modal, Some(Modal::Settings)) {
            self.mic_test = None;
        }
        if let Some(t) = &self.mic_test {
            // The measurement is a peak value; let the meter fall smoothly.
            self.mic_test_level = t.take_level().max(self.mic_test_level * 0.8);
            ctx.request_repaint_after(Duration::from_millis(50));
        } else {
            self.mic_test_level = 0.0;
        }
        {
            let mut vols: HashMap<String, f32> = self
                .voice
                .iter()
                .filter_map(|v| self.settings.user_volumes.get(&v.user_id.to_string()).map(|x| (v.session_id.clone(), *x)))
                .collect();
            for v in &self.voice {
                if let Some(x) = self.settings.user_volumes.get(&format!("music:{}", v.user_id)) {
                    vols.insert(music_owner(&v.session_id), *x);
                }
            }
            *self.backend.shared.volumes.lock() = vols;
        }

        // Who is speaking
        let levels = self.backend.shared.levels.lock().clone();
        self.levels = levels.clone();
        self.speaking.clear();
        if let Some(v) = &self.my_voice {
            if levels.transmitting && levels.me > SPEAK_LEVEL {
                self.speaking.insert(v.session_id.clone());
            }
            for (sid, l) in levels.peers {
                if l > SPEAK_LEVEL {
                    self.speaking.insert(sid);
                }
            }
        }

        // New frame of the watched screen
        if let Some(frame) = self.backend.shared.frame.lock().take() {
            if Some(&frame.owner) == self.watching.as_ref() {
                let img = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
                match &mut self.screen_tex {
                    Some((owner, tex)) if *owner == frame.owner => tex.set(img, TextureOptions::LINEAR),
                    _ => self.screen_tex = Some((frame.owner, ctx.load_texture("screen", img, TextureOptions::LINEAR))),
                }
            }
        }

        // Periodic update check
        if self.me.is_some() && self.last_update_check.is_none_or(|t| t.elapsed() > UPDATE_EVERY) {
            self.check_update(false);
        }
    }

    /// Handles one message from the server. `match` must cover every variant of `ServerMsg`.
    fn handle_server(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Ready { me, servers, channels, voice, usage } => {
                self.me = Some(me);
                self.servers = servers;
                self.channels = channels;
                self.voice = voice;
                self.usage = usage;
                // If we reconnected, tell the server our voice state again.
                if let Some(v) = &self.my_voice {
                    self.ws(json!({ "t": "voice_join", "channelId": v.channel_id, "sessionId": v.session_id,
                        "tracks": v.tracks, "muted": self.settings.muted, "deafened": self.settings.deafened }));
                    if let Some(w) = &self.watching {
                        self.ws(json!({ "t": "voice_update", "watching": w }));
                    }
                }
                self.messages.clear();
                let sid = self
                    .settings
                    .last_server
                    .filter(|id| self.servers.iter().any(|s| s.id == *id))
                    .or_else(|| self.servers.first().map(|s| s.id));
                self.select_server(sid);
            }
            ServerMsg::Message { message } => {
                if let Some(c) = self.messages.get_mut(&message.channel_id) {
                    c.list.push(message);
                }
            }
            ServerMsg::History { channel_id, messages, more } => {
                let c = self.messages.entry(channel_id).or_default();
                let older = !c.list.is_empty();
                let known: HashSet<i64> = c.list.iter().map(|m| m.id).collect();
                let mut fresh: Vec<Message> = messages.into_iter().filter(|m| !known.contains(&m.id)).collect();
                fresh.append(&mut c.list);
                c.list = fresh;
                c.more = more;
                c.loading = false;
                if older && Some(channel_id) == self.channel_id {
                    self.scroll_fix = Some(self.last_content_h);
                } else if Some(channel_id) == self.channel_id {
                    self.scroll_bottom = 3;
                }
            }
            ServerMsg::ServerCreated { server } => self.servers.push(server),
            ServerMsg::ServerDeleted { server_id } => {
                self.servers.retain(|s| s.id != server_id);
                self.channels.retain(|c| c.server_id != server_id);
                if self.server_id == Some(server_id) {
                    self.select_server(self.servers.first().map(|s| s.id));
                }
            }
            ServerMsg::ChannelCreated { channel } => self.channels.push(channel),
            ServerMsg::ChannelDeleted { channel_id } => {
                if self.my_voice.as_ref().is_some_and(|v| v.channel_id == channel_id) {
                    self.leave_voice();
                }
                self.channels.retain(|c| c.id != channel_id);
                if self.channel_id == Some(channel_id) {
                    self.channel_id = None;
                }
            }
            ServerMsg::Voice { voice, usage } => {
                self.voice = voice;
                if usage.is_some() {
                    self.usage = usage;
                }
                self.sync_voice();
            }
            ServerMsg::VoiceDisabled { error } => {
                self.toast(error, true);
                self.leave_voice();
            }
            ServerMsg::Presence {} => {}
            ServerMsg::Error { error } => self.toast(error, true),
        }
    }

    // -------------------------------------------------------------- actions

    fn logout(&mut self) {
        self.leave_voice();
        self.backend.send(Cmd::Logout);
        self.settings.token = None;
        self.settings.save();
        self.logged_in = false;
        self.me = None;
        self.messages.clear();
        self.modal = None;
    }

    fn select_server(&mut self, id: Option<i64>) {
        self.server_id = id;
        self.settings.last_server = id;
        let saved = id.and_then(|id| self.settings.last_channel.get(&id.to_string()).copied());
        let texts: Vec<i64> = self
            .channels
            .iter()
            .filter(|c| Some(c.server_id) == id && !c.is_voice())
            .map(|c| c.id)
            .collect();
        self.channel_id = saved.filter(|s| texts.contains(s)).or(texts.first().copied());
        self.settings.save();
    }

    /// Opens a text channel (and remembers it in the settings).
    fn select_channel(&mut self, id: i64) {
        self.channel_id = Some(id);
        self.scroll_bottom = 3;
        if let Some(sid) = self.server_id {
            self.settings.last_channel.insert(sid.to_string(), id);
            self.settings.save();
        }
        if self.watching.is_some() {
            self.watch(None);
        }
    }

    /// Joins a voice channel. The actual work happens in the background; we get `Event::VoiceJoined` back.
    fn join_voice(&mut self, channel_id: i64) {
        if self.voice_joining.is_some() || self.my_voice.as_ref().is_some_and(|v| v.channel_id == channel_id) {
            return;
        }
        if self.my_voice.is_some() {
            self.leave_voice();
        }
        self.voice_joining = Some(channel_id);
        self.backend.voice(VoiceCmd::Join {
            channel_id,
            muted: self.settings.muted,
            deafened: self.settings.deafened,
        });
    }

    fn leave_voice(&mut self) {
        if self.my_voice.take().is_some() {
            self.backend.voice(VoiceCmd::Leave);
            self.ws(json!({ "t": "voice_leave" }));
        }
        self.watching = None;
        self.screen_tex = None;
        self.screen_busy = false;
        self.music_busy = false;
        if matches!(self.modal, Some(Modal::ScreenShare { .. } | Modal::AudioShare { .. })) {
            self.modal = None;
        }
    }

    /// Tells the voice loop who is in our voice channel (and whose screen we watch),
    /// so it can pull/unpull the right tracks.
    fn sync_voice(&mut self) {
        let Some(my) = &self.my_voice else { return };
        // If the person we're watching stopped sharing, stop watching.
        if let Some(w) = &self.watching {
            let still = self
                .voice
                .iter()
                .any(|v| &v.session_id == w && v.sharing_screen() && v.channel_id == my.channel_id);
            if !still {
                self.watching = None;
                self.screen_tex = None;
            }
        }
        let participants = self
            .voice
            .iter()
            .filter(|v| v.channel_id == my.channel_id)
            .map(|v| Participant { session_id: v.session_id.clone(), tracks: v.tracks.clone() })
            .collect();
        self.backend.voice(VoiceCmd::Sync { participants, watch: self.watching.clone() });
    }

    fn watch(&mut self, session: Option<String>) {
        if let Some(sid) = &session {
            let same = self.voice.iter().find(|v| &v.session_id == sid).map(|v| v.channel_id);
            if self.my_voice.as_ref().map(|m| m.channel_id) != same {
                self.toast("İzlemek için önce o ses kanalına katıl", false);
                return;
            }
        }
        self.watching = session.clone();
        self.screen_tex = None;
        self.ws(json!({ "t": "voice_update", "watching": session }));
        self.sync_voice();
    }

    fn set_muted(&mut self, muted: bool) {
        self.settings.muted = muted;
        // Unmuting while deafened also undeafens.
        if !muted && self.settings.deafened {
            self.settings.deafened = false;
            self.backend.voice(VoiceCmd::SetDeafened(false));
        }
        self.after_mute_change();
    }

    fn set_deafened(&mut self, d: bool) {
        self.settings.deafened = d;
        if d {
            self.settings.muted = true;
        }
        self.backend.voice(VoiceCmd::SetDeafened(d));
        self.after_mute_change();
    }

    fn after_mute_change(&mut self) {
        self.settings.save();
        self.backend.voice(VoiceCmd::SetMuted(self.settings.muted));
        if self.my_voice.is_some() {
            self.ws(json!({ "t": "voice_update", "muted": self.settings.muted, "deafened": self.settings.deafened }));
        }
    }

    fn toggle_screen_share(&mut self) {
        let Some(v) = &self.my_voice else { return };
        if self.screen_busy {
            return;
        }
        if v.tracks.iter().any(|t| t == "screen") {
            self.screen_busy = true;
            self.backend.voice(VoiceCmd::ScreenStop);
        } else {
            self.modal = Some(Modal::ScreenShare { sources: None, tab: SourceTab::Screens, selected: None });
            self.backend.send(Cmd::ScreenSources);
        }
    }

    fn toggle_music(&mut self) {
        let Some(v) = &self.my_voice else { return };
        if self.music_busy {
            return;
        }
        if v.tracks.iter().any(|t| t == "music") {
            self.music_busy = true;
            self.backend.voice(VoiceCmd::MusicStop);
        } else if !appaudio::supported() {
            self.toast("Uygulama sesi paylaşımı şimdilik yalnızca Windows'ta çalışıyor", false);
        } else {
            self.modal = Some(Modal::AudioShare { apps: None, selected: None });
            self.backend.send(Cmd::AudioApps);
        }
    }

    fn check_update(&mut self, manual: bool) {
        self.last_update_check = Some(Instant::now());
        self.update_checking = true;
        self.backend.send(Cmd::CheckUpdate { manual });
    }

    fn send_message(&mut self) {
        let content = self.composer.trim().to_string();
        let Some(ch) = self.channel_id else { return };
        if content.is_empty() {
            return;
        }
        self.ws(json!({ "t": "send", "channelId": ch, "content": content }));
        self.composer.clear();
    }
}

// =====================================================================
// Drawing
// =====================================================================

// `eframe::App` is the trait eframe uses to drive our app. Implementing its `ui`
// method is all eframe needs: it calls it every frame (when something changed or
// an animation is running) and shows whatever we draw.
impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);
        #[cfg(debug_assertions)]
        self.demo();

        #[cfg(target_os = "macos")]
        crate::macos::place_traffic_lights(frame, 16.0, BAR as f64);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = frame;
            widgets::resize_edges(&ctx);
        }

        if !self.logged_in {
            self.auth_screen(ui);
        } else {
            self.main_screen(ui);
        }
        self.modals(&ctx);
        self.toast_overlay(&ctx);

        #[cfg(debug_assertions)]
        self.debug_screenshot(&ctx);
    }
}

impl App {
    // ---------------------------------------------------------- login screen

    fn auth_screen(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().frame(Frame::new().fill(BG_MAIN)).show(ui, |ui| {
            let full = ui.max_rect();
            widgets::drag_region(ui, egui::Rect::from_min_size(full.min, egui::vec2(full.width(), BAR)), "auth-drag");
            if !IS_MAC {
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
                        egui::pos2(full.right() - 126.0, full.top()),
                        egui::pos2(full.right(), full.top() + BAR),
                    )),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            widgets::window_controls(ui, BAR)
                        })
                    },
                );
            }

            let card_w = 380.0;
            let card_h = if self.auth.registering { 470.0 } else { 410.0 };
            let card = egui::Rect::from_center_size(full.center(), egui::vec2(card_w, card_h));
            ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
                Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, Color32::from_white_alpha(14)))
                    .corner_radius(28)
                    .inner_margin(Margin::same(28))
                    .shadow(egui::Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha(160) })
                    .show(ui, |ui| {
                        ui.set_width(card_w - 56.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(Vec2::splat(56.0)));
                            ui.add_space(6.0);
                            ui.label(RichText::new("goodbyedc").font(bold_font(24.0)).color(TEXT_STRONG));
                            let sub = if self.auth.registering { "Hesap oluştur" } else { "Tekrar hoş geldin!" };
                            ui.label(RichText::new(sub).color(MUTED));
                        });
                        ui.add_space(14.0);

                        let mut submit = false;
                        widgets::field_label(ui, "Kullanıcı adı");
                        let r = ui.add(TextEdit::singleline(&mut self.auth.username).desired_width(f32::INFINITY).margin(Margin::symmetric(16, 9)));
                        submit |= widgets::enter_pressed(ui, &r);
                        ui.add_space(4.0);
                        widgets::field_label(ui, "Şifre");
                        let r = ui.add(TextEdit::singleline(&mut self.auth.password).password(true).desired_width(f32::INFINITY).margin(Margin::symmetric(16, 9)));
                        submit |= widgets::enter_pressed(ui, &r);
                        if self.auth.registering {
                            ui.add_space(4.0);
                            widgets::field_label(ui, "Davet kodu");
                            let r = ui.add(TextEdit::singleline(&mut self.auth.invite).desired_width(f32::INFINITY).margin(Margin::symmetric(16, 9)));
                            submit |= widgets::enter_pressed(ui, &r);
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new(&self.auth.error).color(RED).size(13.0));
                        ui.add_space(4.0);
                        let label = match (self.auth.busy, self.auth.registering) {
                            (true, true) => "Kayıt olunuyor…",
                            (true, false) => "Giriş yapılıyor…",
                            (false, true) => "Kayıt ol",
                            (false, false) => "Giriş yap",
                        };
                        let w = ui.available_width();
                        if widgets::accent_button(ui, label, w, true, self.auth.busy).clicked() {
                            submit = true;
                        }
                        ui.add_space(8.0);
                        ui.vertical_centered(|ui| {
                            ui.horizontal(|ui| {
                                let (q, a) = if self.auth.registering {
                                    ("Zaten hesabın var mı?", "Giriş yap")
                                } else {
                                    ("Hesabın yok mu?", "Kayıt ol")
                                };
                                let total = ui.painter().layout_no_wrap(format!("{q} {a}"), font(13.0), MUTED).size().x;
                                ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
                                ui.label(RichText::new(q).color(MUTED).size(13.0));
                                if ui.link(RichText::new(a).size(13.0)).clicked() {
                                    self.auth.registering = !self.auth.registering;
                                    self.auth.error.clear();
                                }
                            });
                        });

                        if submit && !self.auth.busy {
                            self.auth.error.clear();
                            let username = self.auth.username.trim().to_string();
                            let password = self.auth.password.clone();
                            if username.is_empty() || password.is_empty() {
                                self.auth.error = "Kullanıcı adı ve şifre gerekli".into();
                            } else {
                                self.auth.busy = true;
                                if self.auth.registering {
                                    let invite = self.auth.invite.trim().to_string();
                                    self.backend.send(Cmd::Register { username, password, invite });
                                } else {
                                    self.backend.send(Cmd::Login { username, password });
                                }
                            }
                        }
                    });
            });
        });
    }

    // ------------------------------------------------------------ main screen

    /// The logged-in layout: sidebar on the left, header + chat (or screen view) on
    /// the right. egui "panels" take space from the window's edges: a left panel
    /// first, then the `CentralPanel` fills whatever is left.
    fn main_screen(&mut self, ui: &mut Ui) {
        egui::Panel::left("sidebar")
            .exact_size(SIDEBAR_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_SIDEBAR))
            .show(ui, |ui| self.sidebar(ui));

        // The line between the sidebar and the main area
        let r = ui.max_rect();
        ui.painter().vline(r.left() + SIDEBAR_W, r.y_range(), Stroke::new(1.0, BORDER));

        egui::CentralPanel::default().frame(Frame::new().fill(BG_MAIN)).show(ui, |ui| {
            // No gaps between the top bar, the chat and the composer: messages should scroll
            // all the way to the edges. The needed spacing is padding *inside* the scrolled content.
            ui.spacing_mut().item_spacing.y = 0.0;
            self.header(ui);
            let watching = self.watching.clone().and_then(|w| self.voice.iter().find(|v| v.session_id == w).cloned());
            if let Some(user) = watching {
                self.stage(ui, &user);
            } else if self.channel_id.and_then(|id| self.channel(id)).is_some() {
                self.chat(ui);
            } else {
                if self.me.is_none() {
                    widgets::loading_center(ui, "Sunucuya bağlanılıyor…");
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label(RichText::new("Bir kanal seç.").color(MUTED));
                    });
                }
            }
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        // Top part: window buttons (macOS) and the window drag area
        let top = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(SIDEBAR_W, BAR));
        widgets::drag_region(ui, top, "sidebar-drag");
        ui.painter().hline(top.x_range(), top.bottom() - 0.5, Stroke::new(1.0, BORDER));
        ui.add_space(BAR);

        // Bottom parts are laid out first; the channel list uses the remaining space.
        // The sections' top separator lines span the sidebar's full width.
        let line = |ui: &Ui, r: egui::Rect| {
            ui.painter().hline(r.x_range(), r.top() + 0.5, Stroke::new(1.0, BORDER));
        };
        let r = egui::Panel::bottom("user-panel")
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_SIDEBAR).inner_margin(Margin { left: 10, right: 8, top: 10, bottom: 10 }))
            .show(ui, |ui| self.user_panel(ui));
        line(ui, r.response.rect);
        if self.my_voice.is_some() {
            let r = egui::Panel::bottom("voice-panel")
                .show_separator_line(false)
                .frame(Frame::new().fill(BG_SIDEBAR).inner_margin(Margin { left: 12, right: 8, top: 10, bottom: 10 }))
                .show(ui, |ui| self.voice_panel(ui));
            line(ui, r.response.rect);
        }
        let usage = self.usage;
        egui::Panel::bottom("usage")
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_SIDEBAR).inner_margin(Margin { left: 12, right: 12, top: 6, bottom: 8 }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(u) = usage {
                        let warn = u.used_gb >= u.limit_gb * 0.8;
                        ui.label(
                            RichText::new(format!("Trafik {:.1} / {:.0} GB", u.used_gb, u.limit_gb))
                                .size(11.5)
                                .color(if warn { RED } else { MUTED }),
                        )
                        .on_hover_text(usage_tooltip(&u));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(crate::update::version_label()).size(11.5).color(MUTED));
                    });
                });
            });

        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(8.0);
            Frame::new().inner_margin(Margin::symmetric(8, 0)).show(ui, |ui| self.channel_list(ui));
        });
    }

    fn channel_list(&mut self, ui: &mut Ui) {
        let sid = self.server_id;
        let texts: Vec<Channel> = self.channels.iter().filter(|c| Some(c.server_id) == sid && !c.is_voice()).cloned().collect();
        let voices: Vec<Channel> = self.channels.iter().filter(|c| Some(c.server_id) == sid && c.is_voice()).cloned().collect();

        self.category(ui, "Metin kanalları", "text");
        for c in &texts {
            let active = self.channel_id == Some(c.id) && self.watching.is_none();
            let (clicked, delete) = channel_row(ui, ph::CHAT_CIRCLE, &c.name, active, false);
            if clicked {
                self.select_channel(c.id);
            }
            if delete {
                self.modal = Some(Modal::DeleteChannel { id: c.id, name: c.name.clone() });
            }
        }
        ui.add_space(10.0);
        self.category(ui, "Ses kanalları", "voice");
        for c in &voices {
            let mine = self.my_voice.as_ref().is_some_and(|v| v.channel_id == c.id);
            let joining = self.voice_joining == Some(c.id);
            let (clicked, delete) = channel_row(ui, ph::WAVEFORM, &c.name, mine || joining, joining);
            if clicked {
                self.join_voice(c.id);
            }
            if delete {
                self.modal = Some(Modal::DeleteChannel { id: c.id, name: c.name.clone() });
            }
            let users: Vec<VoiceUser> = self.voice.iter().filter(|v| v.channel_id == c.id).cloned().collect();
            for u in users {
                self.voice_user_row(ui, &u);
            }
        }
        ui.add_space(8.0);
    }

    fn category(&mut self, ui: &mut Ui, title: &str, kind: &'static str) {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            widgets::section_label(ui, title);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                if widgets::small_icon_button(ui, ph::PLUS, "Kanal ekle").clicked() {
                    self.modal = Some(Modal::CreateChannel { kind, name: String::new() });
                    self.modal_focus = true;
                }
            });
        });
        ui.add_space(2.0);
    }

    fn voice_user_row(&mut self, ui: &mut Ui, u: &VoiceUser) {
        let (rect, row) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::click());

        let is_me = self.me.as_ref().is_some_and(|m| m.id == u.user_id);
        if !is_me {
            let key = u.user_id.to_string();
            row.context_menu(|ui| {
                ui.set_min_width(220.0);
                ui.add_space(4.0);
                ui.label(RichText::new(&u.username).font(bold_font(13.5)).color(TEXT_STRONG));
                ui.add_space(6.0);
                widgets::field_label(ui, "Kullanıcı ses seviyesi");
                let mut v = self.settings.user_volumes.get(&key).copied().unwrap_or(1.0) * 100.0;
                let r = ui.add(egui::Slider::new(&mut v, 0.0..=200.0).step_by(1.0).suffix("%").trailing_fill(true));
                if r.changed() {
                    if (v - 100.0).abs() < 0.5 {
                        self.settings.user_volumes.remove(&key);
                    } else {
                        self.settings.user_volumes.insert(key.clone(), v / 100.0);
                    }
                }
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    self.settings.save();
                }
                if u.tracks.iter().any(|t| t == "music") {
                    ui.add_space(8.0);
                    widgets::field_label(ui, "Paylaşılan ses (müzik)");
                    let mkey = format!("music:{}", u.user_id);
                    let mut mv = self.settings.user_volumes.get(&mkey).copied().unwrap_or(1.0) * 100.0;
                    let r = ui.add(egui::Slider::new(&mut mv, 0.0..=200.0).step_by(1.0).suffix("%").trailing_fill(true));
                    if r.changed() {
                        if (mv - 100.0).abs() < 0.5 {
                            self.settings.user_volumes.remove(&mkey);
                        } else {
                            self.settings.user_volumes.insert(mkey.clone(), mv / 100.0);
                        }
                    }
                    if r.drag_stopped() || (r.changed() && !r.dragged()) {
                        self.settings.save();
                    }
                }
                ui.add_space(4.0);
                if ui.button("Varsayılana döndür (%100)").clicked() {
                    self.settings.user_volumes.remove(&key);
                    self.settings.user_volumes.remove(&format!("music:{}", u.user_id));
                    self.settings.save();
                    ui.close();
                }
            });
            if row.hovered() {
                ui.painter().rect_filled(rect.shrink2(egui::vec2(4.0, 1.0)), CornerRadius::same(6), Color32::from_white_alpha(5));
            }
        }
        let mut x = rect.left() + 30.0;
        let av = egui::Rect::from_center_size(egui::pos2(x + 10.0, rect.center().y), Vec2::splat(20.0));
        widgets::paint_avatar(ui, av, &u.username, self.speaking.contains(&u.session_id));
        x += 28.0;
        let speaking = self.speaking.contains(&u.session_id);
        ui.painter().text(
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            &u.username,
            font(13.5),
            if speaking { TEXT_STRONG } else { MUTED },
        );

        // Right side: the LIVE badge and status icons
        let mut right = rect.right() - 6.0;
        if u.sharing_screen() {
            let is_me = self.my_voice.as_ref().is_some_and(|v| v.session_id == u.session_id);
            let watching = self.watching.as_deref() == Some(u.session_id.as_str());
            let galley = ui.painter().layout_no_wrap("CANLI".into(), bold_font(10.0), Color32::WHITE);
            let badge = egui::Rect::from_min_max(
                egui::pos2(right - galley.size().x - 12.0, rect.center().y - 9.0),
                egui::pos2(right, rect.center().y + 9.0),
            );
            let resp = ui.interact(badge, ui.id().with(("live", &u.session_id)), Sense::click());
            ui.painter().rect_filled(badge, CornerRadius::same(5), if watching { GREEN } else { RED });
            ui.painter().galley(badge.center() - galley.size() / 2.0, galley, Color32::WHITE);
            let resp = resp.on_hover_text(if is_me { "Ekranını paylaşıyorsun" } else { "İzlemek için tıkla" });
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                if is_me {
                    self.toast("Kendi ekranını paylaşıyorsun", false);
                } else {
                    self.watch(if watching { None } else { Some(u.session_id.clone()) });
                }
            }
            right = badge.left() - 6.0;
        }
        if u.tracks.iter().any(|t| t == "music") {
            let r = egui::Rect::from_center_size(egui::pos2(right - 7.0, rect.center().y), Vec2::splat(16.0));
            ui.painter().text(r.center(), Align2::CENTER_CENTER, ph::MUSIC_NOTES, icon_font(14.0), GREEN);
            ui.interact(r, ui.id().with(("music", &u.session_id)), Sense::hover())
                .on_hover_text("Uygulama sesi paylaşıyor. Sesini ayarlamak için adına sağ tıkla.");
            right -= 20.0;
        }
        let flag = if u.deafened {
            Some(ph::EAR_SLASH)
        } else if u.muted {
            Some(ph::MICROPHONE_SLASH)
        } else {
            None
        };
        if let Some(icon) = flag {
            ui.painter().text(egui::pos2(right, rect.center().y), Align2::RIGHT_CENTER, icon, icon_font(14.0), RED);
            right -= 20.0;
        }
        // Someone we've turned down/muted
        if let Some(v) = self.settings.user_volumes.get(&u.user_id.to_string()) {
            if *v < 0.995 {
                let icon = if *v <= 0.0 { ph::SPEAKER_SLASH } else { ph::SPEAKER_LOW };
                let r = egui::Rect::from_center_size(egui::pos2(right - 7.0, rect.center().y), Vec2::splat(16.0));
                ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, icon_font(14.0), MUTED);
                ui.interact(r, ui.id().with(("vol", u.user_id)), Sense::hover())
                    .on_hover_text(format!("Ses seviyesi %{:.0}", v * 100.0));
            }
        }
    }

    fn voice_panel(&mut self, ui: &mut Ui) {
        let Some(v) = &self.my_voice else { return };
        let sharing = v.tracks.iter().any(|t| t == "screen");
        let music = v.tracks.iter().any(|t| t == "music");
        let ch_name = self.channel(v.channel_id).map(|c| c.name.clone()).unwrap_or_default();
        // Ping: color and signal icon by connection quality.
        let rtt = self.levels.rtt_ms;
        let sig_color = match rtt {
            Some(r) if r < 80.0 => GREEN,
            Some(r) if r < 160.0 => Color32::from_rgb(0xe0, 0xa8, 0x3a),
            Some(_) => RED,
            None => MUTED,
        };
        let w = ui.available_width();

        // Top row: signal, status and channel / ping; disconnect on the right.
        ui.allocate_ui_with_layout(egui::vec2(w, 34.0), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let r = ui.label(RichText::new(ph::BROADCAST).font(icon_font(20.0)).color(sig_color));
            r.on_hover_text(match rtt {
                Some(ms) => format!("Ping: {ms:.0} ms (ses sunucusuna gidiş-dönüş)"),
                None => "Ping ölçülüyor…".to_string(),
            });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new("Ses bağlandı").font(bold_font(13.5)).color(GREEN));
                let detail = match rtt {
                    Some(ms) => format!("{} · {ms:.0} ms", truncate(&ch_name, 16)),
                    None => truncate(&ch_name, 22),
                };
                ui.label(RichText::new(detail).size(12.0).color(MUTED));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::danger_circle_button(ui, ph::PHONE_X, "Bağlantıyı kes").clicked() {
                    self.leave_voice();
                }
            });
        });
        ui.add_space(8.0);

        // Bottom row: labelled pill buttons (to look different from the icon buttons below).
        let gap = 8.0;
        let bw = (w - gap) / 2.0;
        ui.allocate_ui_with_layout(egui::vec2(w, 32.0), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let label = if sharing { "Durdur" } else { "Ekran" };
            let r = widgets::action_pill(ui, ph::SCREENCAST, label, bw, sharing, self.screen_busy)
                .on_hover_text(if sharing { "Ekran paylaşımını durdur" } else { "Ekran paylaş" });
            if r.clicked() {
                self.toggle_screen_share();
            }
            let label = if music { "Durdur" } else { "Ses" };
            let r = widgets::action_pill(ui, ph::MUSIC_NOTES, label, bw, music, self.music_busy)
                .on_hover_text(if music { "Ses paylaşımını durdur" } else { "Uygulama sesi paylaş (Spotify vb.)" });
            if r.clicked() {
                self.toggle_music();
            }
        });
    }

    fn user_panel(&mut self, ui: &mut Ui) {
        let name = self.me.as_ref().map(|m| m.username.clone()).unwrap_or_default();
        let my_sid = self.my_voice.as_ref().map(|v| v.session_id.clone()).unwrap_or_default();
        let status = if self.settings.deafened {
            ("Sağırlaştırıldı", RED)
        } else if self.settings.muted {
            ("Susturuldu", RED)
        } else if self.my_voice.is_some() {
            ("Seste", GREEN)
        } else {
            ("Çevrimiçi", MUTED)
        };
        let w = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(w, 36.0), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            widgets::avatar(ui, &name, 34.0, self.speaking.contains(&my_sid));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(RichText::new(truncate(&name, 14)).font(bold_font(14.0)).color(TEXT_STRONG));
                ui.label(RichText::new(status.0).size(12.0).color(status.1));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if widgets::icon_button(ui, ph::GEAR_SIX, "Ayarlar", Tone::Normal).clicked() {
                    self.modal = Some(Modal::Settings);
                }
                let d = self.settings.deafened;
                let icon = if d { ph::EAR_SLASH } else { ph::HEADPHONES };
                if widgets::icon_button(ui, icon, if d { "Sağırlığı kaldır" } else { "Sağırlaştır" }, if d { Tone::Off } else { Tone::Normal }).clicked() {
                    self.set_deafened(!d);
                }
                let m = self.settings.muted;
                let icon = if m { ph::MICROPHONE_SLASH } else { ph::MICROPHONE };
                if widgets::icon_button(ui, icon, if m { "Susturmayı kaldır" } else { "Sustur" }, if m { Tone::Off } else { Tone::Normal }).clicked() {
                    self.set_muted(!m);
                }
            });
        });
    }

    // ------------------------------------------------------------ top bar

    fn header(&mut self, ui: &mut Ui) {
        let rect = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(ui.max_rect().width(), BAR));
        widgets::drag_region(ui, rect, "header-drag");
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, BORDER));

        let watching = self.watching.clone().and_then(|w| self.voice.iter().find(|v| v.session_id == w).cloned());
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.set_min_height(BAR);
            ui.horizontal_centered(|ui| {
                ui.add_space(16.0);
                if let Some(u) = &watching {
                    ui.label(RichText::new(ph::SCREENCAST).font(icon_font(18.0)).color(MUTED));
                    ui.label(RichText::new(format!("{} ekranını paylaşıyor", u.username)).font(bold_font(15.0)).color(TEXT_STRONG));
                } else if let Some(c) = self.channel_id.and_then(|id| self.channel(id)) {
                    ui.label(RichText::new(ph::CHAT_CIRCLE).font(icon_font(18.0)).color(MUTED));
                    ui.label(RichText::new(&c.name).font(bold_font(15.0)).color(TEXT_STRONG));
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if !IS_MAC {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        widgets::window_controls(ui, BAR);
                        ui.add_space(6.0);
                        ui.spacing_mut().item_spacing.x = 8.0;
                    } else {
                        ui.add_space(12.0);
                    }
                    if self.update_checking {
                        widgets::spinner_slot(ui, 32.0, "Güncellemeler denetleniyor…");
                    } else if widgets::icon_button(ui, ph::ARROWS_CLOCKWISE, "Güncellemeleri denetle", Tone::Normal).clicked() {
                        self.check_update(true);
                    }
                    if self.update_available {
                        let label = if self.update_busy { "İndiriliyor…" } else { "Güncelle" };
                        let resp = widgets::pill_button(ui, label, self.update_busy).on_hover_text("Yeni sürüm hazır");
                        if resp.clicked() && !self.update_busy {
                            self.update_busy = true;
                            self.backend.voice(VoiceCmd::ApplyUpdate);
                        }
                    }
                    let (text, bad) = match self.conn {
                        Conn::Connected => ("", false),
                        Conn::Connecting => ("Bağlanıyor…", false),
                        Conn::Reconnecting => ("Bağlantı koptu, yeniden deneniyor…", true),
                    };
                    if !text.is_empty() {
                        ui.label(RichText::new(text).size(12.0).color(if bad { RED } else { MUTED }));
                        widgets::spinner(ui, 13.0, if bad { RED } else { MUTED });
                    }
                });
            });
        });
    }

    // --------------------------------------------------------------- chat

    fn chat(&mut self, ui: &mut Ui) {
        let Some(ch_id) = self.channel_id else { return };
        let ch_name = self.channel(ch_id).map(|c| c.name.clone()).unwrap_or_default();
        if !self.messages.contains_key(&ch_id) {
            self.messages.insert(ch_id, ChannelMsgs { list: vec![], more: true, loading: true });
            self.ws(json!({ "t": "history", "channelId": ch_id }));
        }

        // The message composer at the bottom
        egui::Panel::bottom("composer")
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_MAIN).inner_margin(Margin { left: 16, right: 16, top: 0, bottom: 18 }))
            .show(ui, |ui| {
                Frame::new()
                    .fill(BG_ELEV)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(0x33, 0x34, 0x35)))
                    .corner_radius(22)
                    .inner_margin(Margin::symmetric(18, 12))
                    .show(ui, |ui| {
                        let resp = ui.add(
                            TextEdit::singleline(&mut self.composer)
                                .hint_text(RichText::new(format!("#{ch_name} kanalına mesaj gönder")).color(MUTED))
                                .frame(Frame::NONE)
                                .desired_width(f32::INFINITY)
                                .font(font(14.5)),
                        );
                        if widgets::enter_pressed(ui, &resp) {
                            self.send_message();
                            resp.request_focus();
                        }
                        if self.modal.is_none() && !resp.has_focus() && ui.ctx().memory(|m| m.focused().is_none()) {
                            resp.request_focus();
                        }
                    });
            });

        let scroll_fix = self.scroll_fix.take();
        let me_id = self.me.as_ref().map(|m| m.id);
        let previews = &self.previews;
        let mut wanted: Vec<String> = Vec::new();
        let Some(c) = self.messages.get(&ch_id) else { return };
        let (more, loading) = (c.more, c.loading);
        let mut area = ScrollArea::vertical().id_salt(("messages", ch_id)).stick_to_bottom(true).auto_shrink(false);
        if self.scroll_bottom > 0 {
            self.scroll_bottom -= 1;
            // A huge value (f32::MAX) makes egui produce NaN and crash; content height
            // plus a margin is enough to reach the bottom.
            area = area.vertical_scroll_offset(self.last_content_h.max(0.0) + 100_000.0);
            ui.ctx().request_repaint();
        }
        let out = area.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if c.list.is_empty() && loading {
                    widgets::loading_center(ui, "Mesajlar yükleniyor…");
                    return;
                }
                ui.add_space(4.0);
                if loading {
                    ui.vertical_centered(|ui| widgets::spinner(ui, 18.0, MUTED));
                } else if more && !c.list.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Yukarı kaydırarak eski mesajları yükle").size(12.0).color(MUTED));
                    });
                }
                if c.list.is_empty() && !loading {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(format!("#{ch_name} kanalının başlangıcı")).font(bold_font(18.0)).color(TEXT_STRONG));
                        ui.label(RichText::new("İlk mesajı sen yaz.").color(MUTED));
                    });
                }
                let mut prev: Option<&Message> = None;
                for m in &c.list {
                    let grouped = prev.is_some_and(|p| p.user_id == m.user_id && m.created_at - p.created_at < 5 * 60 * 1000);
                    message_row(ui, m, grouped, Some(m.user_id) == me_id, previews, &mut wanted);
                    prev = Some(m);
                }
                // Bottom padding: between the last message and the composer.
                ui.add_space(16.0);
            });

        // Ask for previews of the links in the visible messages. (Done after the
        // scroll area closure because inside it `self.messages` is borrowed, and Rust
        // doesn't allow mutating `self.previews` through `self` at the same time.)
        for url in wanted {
            if !self.previews.contains_key(&url) {
                self.previews.insert(url.clone(), PreviewState::Loading);
                self.backend.send(Cmd::Preview(url));
            }
        }

        // If older messages were prepended, keep the visible position.
        if let Some(old_h) = scroll_fix {
            let mut state = out.state;
            state.offset.y += out.content_size.y - old_h;
            state.store(ui.ctx(), out.id);
            ui.ctx().request_repaint();
        }
        self.last_content_h = out.content_size.y;

        // Close to the top: load older messages.
        if out.state.offset.y < 40.0 && more && !loading && out.content_size.y > out.inner_rect.height() {
            if let Some(c) = self.messages.get_mut(&ch_id) {
                if let Some(first) = c.list.first() {
                    c.loading = true;
                    let before = first.id;
                    self.ws(json!({ "t": "history", "channelId": ch_id, "before": before }));
                }
            }
        }
    }

    // ------------------------------------------------------------ screen viewing

    fn stage(&mut self, ui: &mut Ui, user: &VoiceUser) {
        egui::Panel::top("stage-top")
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_MAIN).inner_margin(Margin::symmetric(16, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::avatar(ui, &user.username, 22.0, self.speaking.contains(&user.session_id));
                    ui.label(RichText::new(&user.username).font(bold_font(14.0)).color(TEXT_STRONG));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::secondary_button(ui, "İzlemeyi bırak", 130.0).clicked() {
                            self.watch(None);
                        }
                    });
                });
            });
        let rect = ui.available_rect_before_wrap();
        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
        match &self.screen_tex {
            Some((_, tex)) => {
                let size = tex.size_vec2();
                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                let img = egui::Rect::from_center_size(rect.center(), size * scale);
                ui.painter().image(tex.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                widgets::paint_loading(ui, rect, "Görüntü bekleniyor…");
            }
        }
        ui.allocate_rect(rect, Sense::hover());
    }

    // -------------------------------------------------------------- dialogs

    /// Draws the open modal, if any. The modal is *taken out* of `self.modal`
    /// (`take()` leaves `None`), drawn, and put back as `next` if it should stay
    /// open. Taking it out avoids borrowing `self.modal` while other `self` methods
    /// are called inside the dialog.
    fn modals(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.take() else { return };
        let mut keep = true;
        let mut next: Option<Modal> = None;
        // HeroUI modal: dark surface, very thin light border, large radius (28), generous padding.
        let mut closed = false;
        let frame = Frame::new()
            .fill(SURFACE)
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(14)))
            .corner_radius(28)
            .inner_margin(Margin::same(28))
            .shadow(egui::Shadow { offset: [0, 20], blur: 60, spread: 0, color: Color32::from_black_alpha(190) });
        let resp = egui::Modal::new(Id::new("modal"))
            .backdrop_color(Color32::from_black_alpha(128))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(440.0);
                match modal {
                    Modal::CreateChannel { kind, mut name } => {
                        let (title, desc) = if kind == "voice" {
                            ("Ses kanalı oluştur", "Arkadaşlarınla konuşmak ve ekran paylaşmak için yeni bir ses kanalı.")
                        } else {
                            ("Metin kanalı oluştur", "Mesajlaşmak için yeni bir metin kanalı.")
                        };
                        closed |= widgets::modal_header(ui, title, desc);
                        let r = ui.add(
                            TextEdit::singleline(&mut name)
                                .hint_text(if kind == "voice" { "Kanal adı" } else { "kanal-adi" })
                                .char_limit(50)
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(16, 9)),
                        );
                        if self.modal_focus {
                            r.request_focus();
                            self.modal_focus = false;
                        }
                        let mut create = widgets::enter_pressed(ui, &r);
                        ui.add_space(24.0);
                        let (cancel, ok) = modal_buttons(ui, "Oluştur", !name.trim().is_empty());
                        create |= ok;
                        if cancel {
                            keep = false;
                        } else if create && !name.trim().is_empty() {
                            self.ws(json!({ "t": "create_channel", "serverId": self.server_id, "name": name.trim(), "kind": kind }));
                            keep = false;
                        } else {
                            next = Some(Modal::CreateChannel { kind, name });
                        }
                    }
                    Modal::DeleteChannel { id, name } => {
                        // Channel name in bold inside the description.
                        let mut job = egui::text::LayoutJob::default();
                        let normal = egui::TextFormat { font_id: font(14.5), color: MUTED, ..Default::default() };
                        let strong = egui::TextFormat { font_id: bold_font(14.5), color: TEXT, ..Default::default() };
                        job.append("Bu işlem ", 0.0, normal.clone());
                        job.append(&name, 0.0, strong);
                        job.append(" kanalını ve tüm mesajlarını kalıcı olarak silecek. Bu işlem geri alınamaz.", 0.0, normal);
                        closed |= widgets::modal_header_rich(ui, "Kanal kalıcı olarak silinsin mi?", Some(job.into()));
                        let (cancel, ok) = modal_buttons_danger(ui, "Kanalı sil");
                        if cancel {
                            keep = false;
                        } else if ok {
                            self.ws(json!({ "t": "delete_channel", "channelId": id }));
                            keep = false;
                        } else {
                            next = Some(Modal::DeleteChannel { id, name });
                        }
                    }
                    Modal::AudioShare { apps, mut selected } => {
                        closed |= widgets::modal_header(ui, "Uygulama sesi paylaş",
                            "Seçtiğin uygulamanın sesi ses kanalındaki herkese gider. Yalnızca o uygulamanın sesi paylaşılır.");
                        let mut share_now = false;
                        ScrollArea::vertical().max_height(260.0).min_scrolled_height(260.0).auto_shrink([false, false]).show(ui, |ui| {
                            match &apps {
                                None => widgets::loading_center(ui, "Ses çalan uygulamalar aranıyor…"),
                                Some(list) if list.is_empty() => {
                                    ui.add_space(20.0);
                                    ui.vertical_centered(|ui| {
                                        ui.label(RichText::new("Ses çalan uygulama bulunamadı.").color(MUTED));
                                        ui.label(RichText::new("Spotify'da bir şarkı başlatıp tekrar dene.").size(12.5).color(MUTED));
                                    });
                                }
                                Some(list) => {
                                    for a in list {
                                        let is_sel = selected == Some(a.pid);
                                        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
                                        let bg = if is_sel { ACCENT.gamma_multiply(0.12) } else if r.hovered() { ACTIVE } else { HOVER };
                                        let stroke = if is_sel { Stroke::new(2.0, ACCENT) } else { Stroke::NONE };
                                        ui.painter().rect(rect.shrink(1.0), CornerRadius::same(19), bg, stroke, StrokeKind::Inside);
                                        ui.painter().text(egui::pos2(rect.left() + 14.0, rect.center().y), Align2::LEFT_CENTER, ph::MUSIC_NOTES, icon_font(17.0), MUTED);
                                        ui.painter().text(egui::pos2(rect.left() + 40.0, rect.center().y), Align2::LEFT_CENTER, truncate(&a.name, 52), font(13.5), TEXT_STRONG);
                                        if r.clicked() {
                                            selected = Some(a.pid);
                                        }
                                        if r.double_clicked() {
                                            selected = Some(a.pid);
                                            share_now = true;
                                        }
                                    }
                                }
                            }
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Sesi dengele").color(TEXT));
                                ui.label(
                                    RichText::new("Uygulamadaki ses kısık olsa da herkese aynı yükseklikte gider.")
                                        .size(12.0)
                                        .color(MUTED),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::toggle(ui, &mut self.settings.music_normalize).changed() {
                                    self.settings.save();
                                }
                            });
                        });
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("İpucu: hoparlör kullanıyorsan müzik mikrofonuna da girebilir; kulaklık en iyisi.")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.add_space(24.0);
                        let (cancel, ok) = modal_buttons(ui, "Paylaş", selected.is_some());
                        if cancel {
                            keep = false;
                        } else if (ok || share_now) && selected.is_some() {
                            if let Some(pid) = selected {
                                self.music_busy = true;
                                self.backend.voice(VoiceCmd::MusicStart { pid });
                            }
                            keep = false;
                        } else {
                            next = Some(Modal::AudioShare { apps, selected });
                        }
                    }
                    Modal::Settings => {
                        closed |= widgets::modal_header(ui, "Ses ayarları", "Mikrofon hassasiyeti ve kişilere özel ses seviyeleri.");
                        widgets::field_label(ui, "Giriş hassasiyeti");
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Otomatik belirle").color(TEXT));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::toggle(ui, &mut self.settings.input_auto).changed() {
                                    self.settings.save();
                                }
                            });
                        });
                        ui.add_space(8.0);
                        let in_voice = self.my_voice.is_some();
                        let testing = self.mic_test.is_some();
                        let level = if testing {
                            level_db(self.mic_test_level)
                        } else if in_voice {
                            level_db(self.levels.me)
                        } else {
                            -100.0
                        };
                        let auto = self.settings.input_auto;
                        let mut th = if auto { self.levels.auto_threshold_db.min(-1.0) } else { self.settings.input_threshold_db };
                        if auto && !in_voice {
                            th = -45.0;
                        }
                        let r = widgets::sensitivity_bar(ui, &mut th, level, !auto);
                        if !auto && r.changed() {
                            self.settings.input_threshold_db = th;
                        }
                        if r.drag_stopped() {
                            self.settings.save();
                        }
                        ui.add_space(6.0);
                        let hint = if !in_voice && !testing {
                            "Mikrofon seviyesini görmek için testi başlat ya da bir ses kanalına katıl.".to_string()
                        } else if auto && !in_voice {
                            "Otomatik eşik, ses kanalına katılınca odanın gürültüsüne göre belirlenir.".to_string()
                        } else if auto {
                            "Arka plan gürültüsüne göre eşik kendiliğinden ayarlanıyor.".to_string()
                        } else {
                            format!("Eşik: {th:.0} dB. Çizginin sağına geçen sesler iletilir.")
                        };
                        ui.label(RichText::new(hint).size(12.5).color(MUTED));
                        ui.add_space(18.0);
                        widgets::field_label(ui, "Mikrofon testi");
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Kendi sesini duy").color(TEXT));
                                ui.label(
                                    RichText::new("Sesin kulaklığından geri çalınır. Yankı olmasın diye kulaklık tak.")
                                        .size(12.0)
                                        .color(MUTED),
                                );
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let mut on = testing;
                                if widgets::toggle(ui, &mut on).changed() {
                                    if on {
                                        match crate::mictest::MicTest::start() {
                                            Ok(t) => self.mic_test = Some(t),
                                            Err(e) => {
                                                log::warn!("mikrofon testi başlatılamadı: {e:#}");
                                                self.toast(format!("Mikrofon testi başlatılamadı: {e}"), true);
                                            }
                                        }
                                    } else {
                                        self.mic_test = None;
                                    }
                                }
                            });
                        });
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new("Kişilerin ses seviyesini değiştirmek için ses kanalında adlarına sağ tıkla.")
                                .size(12.5)
                                .color(MUTED),
                        );
                        ui.add_space(24.0);
                        // Bottom row: version on the left, buttons on the right (primary action rightmost).
                        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), widgets::BUTTON_H), Layout::left_to_right(Align::Center), |ui| {
                            ui.label(RichText::new(format!("goodbyedc · {}", crate::update::version_label())).size(12.0).color(MUTED));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                let w = widgets::button_width(ui, "Tamam");
                                if widgets::accent_button(ui, "Tamam", w, true, false).clicked() {
                                    keep = false;
                                }
                                let w = widgets::button_width(ui, "Çıkış yap");
                                if widgets::secondary_button(ui, "Çıkış yap", w).clicked() {
                                    keep = false;
                                    self.logout();
                                }
                            });
                        });
                        if keep {
                            next = Some(Modal::Settings);
                        }
                    }
                    Modal::ScreenShare { sources, mut tab, mut selected } => {
                        closed |= widgets::modal_header(ui, "Ekran paylaş", "Ses kanalındakilerin izleyeceği ekranı veya pencereyi seç.");
                        // Tabs
                        // HeroUI Tabs (solid): gray track, the selected tab is a lighter "cursor".
                        Frame::new()
                            .fill(BG_INPUT)
                            .corner_radius(20)
                            .inner_margin(Margin::same(4))
                            .show(ui, |ui| {
                                ui.columns(2, |cols| {
                                    for (i, (t, icon, label)) in [
                                        (SourceTab::Screens, ph::MONITOR, "Tüm ekran"),
                                        (SourceTab::Windows, ph::APP_WINDOW, "Uygulamalar"),
                                    ]
                                    .into_iter()
                                    .enumerate()
                                    {
                                        let ui = &mut cols[i];
                                        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::click());
                                        let active = tab == t;
                                        let bg = if active { ACTIVE } else { Color32::TRANSPARENT };
                                        ui.painter().rect_filled(rect, CornerRadius::same(16), bg);
                                        let fg = if active { TEXT_STRONG } else { MUTED };
                                        let text = ui.painter().layout_no_wrap(label.to_string(), font(13.5), fg);
                                        let total = 16.0 + 8.0 + text.size().x;
                                        let x = rect.center().x - total / 2.0;
                                        ui.painter().text(egui::pos2(x + 8.0, rect.center().y), Align2::CENTER_CENTER, icon, icon_font(16.0), fg);
                                        ui.painter().galley(egui::pos2(x + 24.0, rect.center().y - text.size().y / 2.0), text, fg);
                                        if r.clicked() && !active {
                                            tab = t;
                                            let want = if t == SourceTab::Screens { "screen" } else { "window" };
                                            selected = sources
                                                .as_ref()
                                                .and_then(|s| s.iter().find(|s| s.kind == want))
                                                .map(|s| s.id.to_string());
                                        }
                                    }
                                });
                            });
                        ui.add_space(8.0);
                        let mut share_now = false;
                        ScrollArea::vertical().max_height(300.0).min_scrolled_height(300.0).auto_shrink([false, false]).show(ui, |ui| {
                            match &sources {
                                None => widgets::loading_center(ui, "Ekranlar ve pencereler aranıyor…"),
                                Some(list) => {
                                    let want = if tab == SourceTab::Screens { "screen" } else { "window" };
                                    let items: Vec<&SourceInfo> = list.iter().filter(|s| s.kind == want).collect();
                                    if items.is_empty() {
                                        ui.add_space(20.0);
                                        ui.vertical_centered(|ui| {
                                            ui.label(RichText::new(if want == "screen" { "Ekran bulunamadı." } else { "Açık uygulama penceresi bulunamadı." }).color(MUTED))
                                        });
                                    }
                                    for s in items {
                                        let id = s.id.to_string();
                                        let is_sel = selected.as_deref() == Some(id.as_str());
                                        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
                                        let bg = if is_sel { ACCENT.gamma_multiply(0.12) } else if r.hovered() { ACTIVE } else { HOVER };
                                        let stroke = if is_sel { Stroke::new(2.0, ACCENT) } else { Stroke::NONE };
                                        ui.painter().rect(rect.shrink(1.0), CornerRadius::same(19), bg, stroke, StrokeKind::Inside);
                                        let icon = if want == "screen" { ph::MONITOR } else { ph::APP_WINDOW };
                                        ui.painter().text(egui::pos2(rect.left() + 14.0, rect.center().y), Align2::LEFT_CENTER, icon, icon_font(17.0), MUTED);
                                        let title = truncate(&s.title, 52);
                                        ui.painter().text(egui::pos2(rect.left() + 40.0, rect.center().y), Align2::LEFT_CENTER, title, font(13.5), TEXT_STRONG);
                                        if r.clicked() {
                                            selected = Some(id.clone());
                                        }
                                        if r.double_clicked() {
                                            selected = Some(id);
                                            share_now = true;
                                        }
                                    }
                                }
                            }
                        });
                        ui.add_space(24.0);
                        let (cancel, ok) = modal_buttons(ui, "Paylaş", selected.is_some());
                        if cancel {
                            keep = false;
                        } else if (ok || share_now) && selected.is_some() {
                            let src = sources
                                .as_ref()
                                .and_then(|l| l.iter().find(|s| Some(s.id.to_string()) == selected).cloned());
                            if let Some(s) = src {
                                self.screen_busy = true;
                                self.backend.voice(VoiceCmd::ScreenStart { id: s.id, kind: s.kind.clone() });
                            }
                            keep = false;
                        } else {
                            next = Some(Modal::ScreenShare { sources, tab, selected });
                        }
                    }
                }
            });
        if closed || resp.should_close() || ctx.input(|i| i.key_pressed(Key::Escape)) {
            keep = false;
        }
        if keep {
            self.modal = next;
        }
    }

    fn toast_overlay(&mut self, ctx: &egui::Context) {
        let Some((text, error, at)) = &self.toast else { return };
        if at.elapsed() > Duration::from_secs(4) {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let (text, error) = (text.clone(), *error);
        egui::Area::new(Id::new("toast"))
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                // HeroUI toast-style notification
                Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, Color32::from_white_alpha(14)))
                    .corner_radius(20)
                    .inner_margin(Margin::symmetric(18, 12))
                    .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(140) })
                    .show(ui, |ui| {
                        ui.set_max_width(520.0);
                        ui.horizontal(|ui| {
                            let (icon, c) = if error { (ph::WARNING_CIRCLE, RED) } else { (ph::INFO, MUTED) };
                            ui.label(RichText::new(icon).font(icon_font(16.0)).color(c));
                            ui.label(RichText::new(text).font(bold_font(13.5)).color(TEXT_STRONG));
                        });
                    });
            });
    }

    /// Development: with GOODBYEDC_SCREENSHOT=file.ppm, takes a screenshot after a few seconds and exits.
    #[cfg(debug_assertions)]
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, frames)) = &mut self.screenshot else { return };
        *frames += 1;
        ctx.request_repaint();
        let delay: u32 = std::env::var("GOODBYEDC_SCREENSHOT_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(180);
        if *frames == delay {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let path = path.clone();
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let mut out = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
            for p in &img.pixels {
                out.extend_from_slice(&[p.r(), p.g(), p.b()]);
            }
            let _ = std::fs::write(&path, out);
            std::process::exit(0);
        }
    }
}

// ---------------------------------------------------------------- pieces

/// One channel row in the sidebar.
/// Returns a tuple: (row clicked?, delete button clicked?).
fn channel_row(ui: &mut Ui, icon: &str, name: &str, active: bool, loading: bool) -> (bool, bool) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::click());
    let hovered = resp.hovered() || ui.rect_contains_pointer(rect);
    // HeroUI-style hover/selected background.
    let bg = if active { ACTIVE } else if hovered { HOVER } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, CornerRadius::same(16), bg);
    let fg = if active || hovered { TEXT_STRONG } else { MUTED };
    ui.painter().text(egui::pos2(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, icon, icon_font(17.0), MUTED);
    ui.painter().text(egui::pos2(rect.left() + 34.0, rect.center().y), Align2::LEFT_CENTER, truncate(name, 24), font(14.0), fg);
    let mut delete = false;
    if loading {
        let r = egui::Rect::from_center_size(egui::pos2(rect.right() - 16.0, rect.center().y), Vec2::splat(14.0));
        widgets::spin(ui, r, MUTED);
    } else if hovered {
        let del = egui::Rect::from_center_size(egui::pos2(rect.right() - 16.0, rect.center().y), Vec2::splat(22.0));
        let dr = ui.interact(del, ui.id().with(("del", name, rect.top() as i32)), Sense::click());
        let c = if dr.hovered() { TEXT_STRONG } else { MUTED };
        ui.painter().text(del.center(), Align2::CENTER_CENTER, ph::X, icon_font(13.0), c);
        if dr.on_hover_text("Kanalı sil").clicked() {
            delete = true;
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    (resp.clicked() && !delete, delete)
}

/// One chat message: avatar + name + time (unless `grouped` with the previous
/// message from the same person), the text, and a link preview card.
fn message_row(
    ui: &mut Ui,
    m: &Message,
    grouped: bool,
    _mine: bool,
    previews: &HashMap<String, PreviewState>,
    wanted: &mut Vec<String>,
) {
    let left = 16.0;
    let avatar = 38.0;
    let text_x = left + avatar + 14.0;
    if !grouped {
        ui.add_space(12.0);
    }
    let width = ui.available_width();
    let start = ui.cursor().min;
    // Reserve a slot for the (hover) background first, draw the content, then fill the slot.
    let bg_idx = ui.painter().add(egui::Shape::Noop);
    let inner = ui.scope_builder(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
            egui::pos2(start.x + text_x, start.y),
            egui::vec2(width - text_x - 16.0, f32::INFINITY),
        )),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if !grouped {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&m.username).font(bold_font(14.5)).color(TEXT_STRONG));
                    ui.label(RichText::new(fmt_time(m.created_at)).size(11.5).color(MUTED));
                });
            }
            let urls = crate::preview::find_urls(&m.content);
            if urls.is_empty() {
                ui.add(egui::Label::new(RichText::new(&m.content).size(14.5).color(TEXT)).wrap().selectable(true));
            } else {
                message_text_with_links(ui, &m.content, &urls);
                // Preview of the first link
                let url = crate::preview::slice_chars(&m.content, urls[0]);
                match previews.get(&url) {
                    None => wanted.push(url),
                    Some(PreviewState::Ready(v)) => {
                        ui.add_space(4.0);
                        preview_card(ui, &url, v);
                    }
                    _ => {}
                }
            }
        },
    );
    let h = inner.response.rect.height().max(if grouped { 0.0 } else { avatar });
    let row = egui::Rect::from_min_size(egui::pos2(start.x, start.y - 2.0), egui::vec2(width, h + 4.0));
    if ui.rect_contains_pointer(row) {
        ui.painter().set(bg_idx, egui::Shape::rect_filled(row, 0.0, Color32::from_white_alpha(4)));
    }
    if !grouped {
        let av = egui::Rect::from_min_size(egui::pos2(start.x + left, start.y + 1.0), Vec2::splat(avatar));
        widgets::paint_avatar(ui, av, &m.username, false);
    }
    ui.allocate_rect(egui::Rect::from_min_size(start, egui::vec2(width, h)), Sense::hover());
    ui.add_space(3.0);
}

/// Message text with underlined, clickable links.
/// egui has no rich text with per-word click areas, so we lay the text out
/// ourselves (`LayoutJob` = text plus formatting per section), then find which
/// character is under the mouse to tell whether a link is hovered/clicked.
fn message_text_with_links(ui: &mut Ui, text: &str, urls: &[(usize, usize)]) {
    use egui::text::{LayoutJob, TextFormat};
    let chars: Vec<char> = text.chars().collect();
    let normal = TextFormat { font_id: font(14.5), color: TEXT, ..Default::default() };
    let link = TextFormat { font_id: font(14.5), color: LINK, underline: Stroke::new(1.0, LINK), ..Default::default() };
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let mut last = 0;
    for &(a, b) in urls {
        if a > last {
            job.append(&chars[last..a].iter().collect::<String>(), 0.0, normal.clone());
        }
        job.append(&chars[a..b].iter().collect::<String>(), 0.0, link.clone());
        last = b;
    }
    if last < chars.len() {
        job.append(&chars[last..].iter().collect::<String>(), 0.0, normal);
    }
    let (pos, galley, resp) = egui::Label::new(job).sense(Sense::click()).layout_in_ui(ui);
    // Which link is the mouse over?
    let mut hovered: Option<String> = None;
    if let Some(p) = resp.hover_pos() {
        let rel = p - pos;
        let on_text = galley.rows.iter().any(|r| r.rect().expand(1.0).contains(egui::pos2(rel.x, rel.y)));
        if on_text {
            let idx = galley.cursor_from_pos(rel).index.0;
            if let Some(&(a, b)) = urls.iter().find(|&&(a, b)| idx >= a && idx < b) {
                hovered = Some(chars[a..b].iter().collect());
            }
        }
    }
    ui.painter().galley(pos, galley, TEXT);
    if let Some(url) = hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
        }
        resp.on_hover_text(url);
    }
}

/// Link preview card: site name, title (clickable), description and image.
fn preview_card(ui: &mut Ui, url: &str, v: &PreviewView) {
    let max_w = ui.available_width().min(440.0);
    let image_only = v.title.is_none() && v.desc.is_none();
    let open = |ui: &Ui| ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    let image = |ui: &mut Ui, tex: &TextureHandle, max_w: f32| {
        let size = tex.size_vec2();
        let scale = (max_w / size.x).min(260.0 / size.y).min(1.0);
        let r = ui.add(
            egui::Image::new(tex)
                .fit_to_exact_size(size * scale)
                .corner_radius(8)
                .sense(Sense::click()),
        );
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        r.clicked()
    };
    if image_only {
        if let Some(tex) = &v.image {
            if image(ui, tex, max_w) {
                open(ui);
            }
        }
        return;
    }
    let resp = Frame::new()
        .fill(Color32::from_rgb(0x22, 0x23, 0x24))
        .corner_radius(10)
        .inner_margin(Margin { left: 14, right: 12, top: 10, bottom: 12 })
        .show(ui, |ui| {
            ui.set_max_width(max_w - 26.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            if let Some(site) = &v.site {
                ui.label(RichText::new(site).size(12.0).color(MUTED));
            }
            if let Some(title) = &v.title {
                let r = ui.add(egui::Label::new(RichText::new(title).font(bold_font(14.0)).color(LINK)).wrap().sense(Sense::click()));
                if r.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if r.clicked() {
                    open(ui);
                }
            }
            if let Some(desc) = &v.desc {
                ui.add(egui::Label::new(RichText::new(desc).size(13.0).color(TEXT)).wrap());
            }
            if let Some(tex) = &v.image {
                ui.add_space(6.0);
                let w = ui.available_width();
                if image(ui, tex, w) {
                    open(ui);
                }
            }
        });
    // Thin accent bar on the left
    let r = resp.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_max(r.min, egui::pos2(r.left() + 3.0, r.bottom())),
        CornerRadius { nw: 10, sw: 10, ne: 0, se: 0 },
        ACCENT,
    );
}

/// Two buttons side by side: Cancel / confirm. Returns (cancel clicked, confirm clicked).
fn modal_buttons(ui: &mut Ui, ok_text: &str, ok_enabled: bool) -> (bool, bool) {
    modal_buttons_inner(ui, ok_text, ok_enabled, false)
}

/// For destructive actions: the confirm button is red.
fn modal_buttons_danger(ui: &mut Ui, ok_text: &str) -> (bool, bool) {
    modal_buttons_inner(ui, ok_text, true, true)
}

fn modal_buttons_inner(ui: &mut Ui, ok_text: &str, ok_enabled: bool, danger: bool) -> (bool, bool) {
    let mut cancel = false;
    let mut ok = false;
    // HeroUI dialog footer: right-aligned buttons sized to their text; the primary
    // action is rightmost.
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), widgets::BUTTON_H), Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let w = widgets::button_width(ui, ok_text);
        ok = if danger {
            widgets::danger_button(ui, ok_text, w).clicked()
        } else {
            widgets::accent_button(ui, ok_text, w, ok_enabled, false).clicked()
        };
        let w = widgets::button_width(ui, "Vazgeç");
        cancel = widgets::secondary_button(ui, "Vazgeç", w).clicked();
    });
    (cancel, ok)
}

/// Tooltip text for the traffic meter: the window slides, so there is no single
/// reset date; every night the oldest day's traffic drops out.
fn usage_tooltip(u: &Usage) -> String {
    let days = u.window_days.unwrap_or(31);
    let mut text = format!(
        "Son {days} gündeki tahmini ses/görüntü trafiği.\n\
         Tek bir sıfırlanma tarihi yok: her gece en eski günün trafiği hesaptan düşer."
    );
    if let (Some(gb), Some(at)) = (u.next_drop_gb, u.next_drop_at) {
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(t) = chrono::DateTime::from_timestamp_millis(at).filter(|_| at > now) {
            let local = t.with_timezone(&chrono::Local);
            let day = if local.date_naive() == chrono::Local::now().date_naive() { "Bugün" } else { "Yarın" };
            text += &format!("\n\n{day} {}: {gb:.1} GB düşecek", local.format("%H:%M"));
        }
    }
    text += &format!("\nSınır: {:.0} GB (Cloudflare'in aylık 1000 GB ücretsiz kotasından pay bırakılarak)", u.limit_gb);
    text
}

/// Shortens text to `max` characters, adding "…" (counts characters, not bytes).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

/// "Bugün 14:05" (today) or "07.11.2026 14:05" (local time).
fn fmt_time(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_millis_opt(ms).single() else { return String::new() };
    let now = Local::now();
    if t.date_naive() == now.date_naive() {
        format!("Bugün {}", t.format("%H:%M"))
    } else if Some(t.date_naive()) == now.date_naive().pred_opt() {
        format!("Dün {}", t.format("%H:%M"))
    } else {
        t.format("%d.%m.%Y %H:%M").to_string()
    }
}
