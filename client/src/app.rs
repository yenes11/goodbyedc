//! Uygulama durumu ve arayüz (egui).

use std::collections::{HashMap, HashSet};
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
use crate::screen::SourceInfo;
use crate::theme::*;
use crate::voice::{level_db, Gate, Levels, Participant};
use crate::widgets::{self, Tone};

/// Üst çubukların yüksekliği (macOS pencere düğmeleri bunun ortasına yerleşir).
const BAR: f32 = 46.0;
const SIDEBAR_W: f32 = 248.0;
const SPEAK_LEVEL: f32 = 0.02;
const UPDATE_EVERY: Duration = Duration::from_secs(30 * 60);
const IS_MAC: bool = cfg!(target_os = "macos");

#[derive(Default)]
struct ChannelMsgs {
    list: Vec<Message>,
    more: bool,
    loading: bool,
}

struct MyVoice {
    channel_id: i64,
    session_id: String,
    tracks: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceTab {
    Screens,
    Windows,
}

enum Modal {
    CreateChannel { kind: &'static str, name: String },
    DeleteChannel { id: i64, name: String },
    ScreenShare { sources: Option<Vec<SourceInfo>>, tab: SourceTab, selected: Option<String> },
    Settings,
}

struct AuthForm {
    username: String,
    password: String,
    invite: String,
    registering: bool,
    error: String,
    busy: bool,
}

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
    /// Eski mesajlar başa eklenince kaydırma konumunu korumak için önceki içerik yüksekliği.
    scroll_fix: Option<f32>,
    last_content_h: f32,
    composer: String,

    my_voice: Option<MyVoice>,
    /// Bağlanılmakta olan ses kanalı.
    voice_joining: Option<i64>,
    /// Ekran paylaşımı başlatılıyor/durduruluyor.
    screen_busy: bool,
    watching: Option<String>,
    screen_tex: Option<(String, TextureHandle)>,
    speaking: HashSet<String>,
    /// Son ölçülen ses seviyeleri (ayarlar penceresindeki gösterge için).
    levels: Levels,

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
            last_content_h: 0.0,
            composer: String::new(),
            my_voice: None,
            voice_joining: None,
            screen_busy: false,
            watching: None,
            screen_tex: None,
            speaking: HashSet::new(),
            levels: Levels::default(),
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

    // ------------------------------------------------------------ yardımcılar

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some((text.into(), error, Instant::now()));
    }

    fn ws(&self, v: serde_json::Value) {
        self.backend.ws(v);
    }

    fn channel(&self, id: i64) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    // --------------------------------------------------------------- olaylar

    /// Geliştirme: GOODBYEDC_DEMO ile belirli bir pencereyi açık başlat (ekran görüntüsü testleri için).
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
            "settings" => {
                self.settings.input_auto = false;
                self.modal = Some(Modal::Settings);
            }
            _ => {}
        }
    }

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
                Event::VoiceLeft => {}
                Event::Tracks(tracks) => {
                    self.screen_busy = false;
                    if let Some(v) = &mut self.my_voice {
                        v.tracks = tracks.clone();
                        self.ws(json!({ "t": "voice_update", "tracks": tracks }));
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

        // Ses ayarlarını ses döngüsüne ilet (hassasiyet ve kişilere özel yükseklik).
        *self.backend.shared.gate.lock() =
            Gate { auto: self.settings.input_auto, threshold_db: self.settings.input_threshold_db };
        {
            let vols: HashMap<String, f32> = self
                .voice
                .iter()
                .filter_map(|v| self.settings.user_volumes.get(&v.user_id.to_string()).map(|x| (v.session_id.clone(), *x)))
                .collect();
            *self.backend.shared.volumes.lock() = vols;
        }

        // Konuşanlar
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

        // İzlenen ekranın yeni karesi
        if let Some(frame) = self.backend.shared.frame.lock().take() {
            if Some(&frame.owner) == self.watching.as_ref() {
                let img = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
                match &mut self.screen_tex {
                    Some((owner, tex)) if *owner == frame.owner => tex.set(img, TextureOptions::LINEAR),
                    _ => self.screen_tex = Some((frame.owner, ctx.load_texture("screen", img, TextureOptions::LINEAR))),
                }
            }
        }

        // Düzenli güncelleme kontrolü
        if self.me.is_some() && self.last_update_check.is_none_or(|t| t.elapsed() > UPDATE_EVERY) {
            self.check_update(false);
        }
    }

    fn handle_server(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Ready { me, servers, channels, voice, usage } => {
                self.me = Some(me);
                self.servers = servers;
                self.channels = channels;
                self.voice = voice;
                self.usage = usage;
                // Yeniden bağlandıysak ses durumumuzu tekrar bildir.
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

    // -------------------------------------------------------------- işlemler

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

    fn select_channel(&mut self, id: i64) {
        self.channel_id = Some(id);
        if let Some(sid) = self.server_id {
            self.settings.last_channel.insert(sid.to_string(), id);
            self.settings.save();
        }
        if self.watching.is_some() {
            self.watch(None);
        }
    }

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
        if matches!(self.modal, Some(Modal::ScreenShare { .. })) {
            self.modal = None;
        }
    }

    fn sync_voice(&mut self) {
        let Some(my) = &self.my_voice else { return };
        // İzlediğimiz kişi paylaşımı bıraktıysa izlemeyi kapat.
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
        // Sağırken susturmayı kaldırmak sağırlığı da kaldırır.
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
// Çizim
// =====================================================================

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
    // ---------------------------------------------------------- giriş ekranı

    fn auth_screen(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().frame(Frame::new().fill(BG_MAIN)).show(ui, |ui| {
            let full = ui.max_rect();
            widgets::drag_region(ui, egui::Rect::from_min_size(full.min, egui::vec2(full.width(), BAR)), "auth-drag");
            if !IS_MAC {
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
                        egui::pos2(full.right() - 138.0, full.top()),
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
                    .fill(BG_SIDEBAR)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(16)
                    .inner_margin(Margin::same(28))
                    .shadow(egui::Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(100) })
                    .show(ui, |ui| {
                        ui.set_width(card_w - 56.0);
                        ui.vertical_centered(|ui| {
                            ui.add(egui::Image::new(&self.logo).fit_to_exact_size(Vec2::splat(56.0)));
                            ui.add_space(6.0);
                            ui.label(RichText::new("goodbyedc").font(bold_font(22.0)).color(TEXT_STRONG));
                            let sub = if self.auth.registering { "Hesap oluştur" } else { "Tekrar hoş geldin!" };
                            ui.label(RichText::new(sub).color(MUTED));
                        });
                        ui.add_space(14.0);

                        let mut submit = false;
                        widgets::section_label(ui, "Kullanıcı adı");
                        let r = ui.add(TextEdit::singleline(&mut self.auth.username).desired_width(f32::INFINITY).margin(Margin::symmetric(10, 8)));
                        submit |= widgets::enter_pressed(ui, &r);
                        ui.add_space(4.0);
                        widgets::section_label(ui, "Şifre");
                        let r = ui.add(TextEdit::singleline(&mut self.auth.password).password(true).desired_width(f32::INFINITY).margin(Margin::symmetric(10, 8)));
                        submit |= widgets::enter_pressed(ui, &r);
                        if self.auth.registering {
                            ui.add_space(4.0);
                            widgets::section_label(ui, "Davet kodu");
                            let r = ui.add(TextEdit::singleline(&mut self.auth.invite).desired_width(f32::INFINITY).margin(Margin::symmetric(10, 8)));
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

    // ------------------------------------------------------------ ana ekran

    fn main_screen(&mut self, ui: &mut Ui) {
        egui::Panel::left("sidebar")
            .exact_size(SIDEBAR_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_SIDEBAR))
            .show(ui, |ui| self.sidebar(ui));

        // Kenar çubuğu ile ana alan arasındaki çizgi
        let r = ui.max_rect();
        ui.painter().vline(r.left() + SIDEBAR_W, r.y_range(), Stroke::new(1.0, BORDER));

        egui::CentralPanel::default().frame(Frame::new().fill(BG_MAIN)).show(ui, |ui| {
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
        // Üst kısım: pencere düğmeleri (macOS) ve sürükleme alanı
        let top = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(SIDEBAR_W, BAR));
        widgets::drag_region(ui, top, "sidebar-drag");
        ui.painter().hline(top.x_range(), top.bottom() - 0.5, Stroke::new(1.0, BORDER));
        ui.add_space(BAR);

        // Alt kısımlar önce yerleşir, kanal listesi kalan alanı kullanır.
        egui::Panel::bottom("user-panel")
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_SIDEBAR).inner_margin(Margin::symmetric(8, 8)))
            .show(ui, |ui| self.user_panel(ui));
        if self.my_voice.is_some() {
            egui::Panel::bottom("voice-panel")
                .show_separator_line(false)
                .frame(Frame::new().fill(BG_SIDEBAR).inner_margin(Margin::symmetric(10, 8)))
                .show(ui, |ui| self.voice_panel(ui));
        }
        if let Some(u) = self.usage {
            egui::Panel::bottom("usage")
                .show_separator_line(false)
                .frame(Frame::new().fill(BG_SIDEBAR).inner_margin(Margin { left: 12, right: 12, top: 4, bottom: 6 }))
                .show(ui, |ui| {
                    let warn = u.used_gb >= u.limit_gb * 0.8;
                    ui.label(
                        RichText::new(format!("Ses/görüntü trafiği (31 gün): {:.1} / {:.0} GB", u.used_gb, u.limit_gb))
                            .size(11.0)
                            .color(if warn { RED } else { MUTED }),
                    );
                });
        }

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
            let (clicked, delete) = channel_row(ui, ph::HASH, &c.name, active, false);
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
            let (clicked, delete) = channel_row(ui, ph::SPEAKER_HIGH, &c.name, mine || joining, joining);
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
                widgets::section_label(ui, "Kullanıcı ses seviyesi");
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
                ui.add_space(4.0);
                if ui.button("Varsayılana döndür (%100)").clicked() {
                    self.settings.user_volumes.remove(&key);
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

        // Sağ taraf: CANLI etiketi ve durum ikonları
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
        // Sesini kıstığımız/kapattığımız kişi
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
        let ch_name = self.channel(v.channel_id).map(|c| c.name.clone()).unwrap_or_default();
        ui.painter().hline(ui.max_rect().x_range(), ui.max_rect().top() - 8.0, Stroke::new(1.0, BORDER));
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(RichText::new("Ses bağlandı").font(bold_font(13.5)).color(GREEN));
                ui.label(RichText::new(ch_name).size(12.0).color(MUTED));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if widgets::icon_button(ui, ph::PHONE_DISCONNECT, "Bağlantıyı kes", Tone::Danger).clicked() {
                    self.leave_voice();
                }
                let tip = if sharing { "Paylaşımı durdur" } else { "Ekran paylaş" };
                if self.screen_busy {
                    widgets::spinner_slot(ui, 32.0, if sharing { "Durduruluyor…" } else { "Paylaşım başlatılıyor…" });
                } else if widgets::icon_button(ui, ph::SCREENCAST, tip, if sharing { Tone::On } else { Tone::Normal }).clicked() {
                    self.toggle_screen_share();
                }
            });
        });
    }

    fn user_panel(&mut self, ui: &mut Ui) {
        ui.painter().hline(ui.max_rect().x_range(), ui.max_rect().top() - 8.0, Stroke::new(1.0, BORDER));
        let name = self.me.as_ref().map(|m| m.username.clone()).unwrap_or_default();
        let my_sid = self.my_voice.as_ref().map(|v| v.session_id.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            widgets::avatar(ui, &name, 30.0, self.speaking.contains(&my_sid));
            ui.label(RichText::new(&name).font(bold_font(14.0)).color(TEXT_STRONG));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
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

    // ------------------------------------------------------------ üst çubuk

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
                    ui.label(RichText::new(ph::HASH).font(icon_font(18.0)).color(MUTED));
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

    // --------------------------------------------------------------- sohbet

    fn chat(&mut self, ui: &mut Ui) {
        let Some(ch_id) = self.channel_id else { return };
        let ch_name = self.channel(ch_id).map(|c| c.name.clone()).unwrap_or_default();
        if !self.messages.contains_key(&ch_id) {
            self.messages.insert(ch_id, ChannelMsgs { list: vec![], more: true, loading: true });
            self.ws(json!({ "t": "history", "channelId": ch_id }));
        }

        // Mesaj kutusu altta
        egui::Panel::bottom("composer")
            .show_separator_line(false)
            .frame(Frame::new().fill(BG_MAIN).inner_margin(Margin { left: 16, right: 16, top: 6, bottom: 18 }))
            .show(ui, |ui| {
                Frame::new()
                    .fill(BG_ELEV)
                    .stroke(Stroke::new(1.0, Color32::from_rgb(0x33, 0x34, 0x35)))
                    .corner_radius(12)
                    .inner_margin(Margin::symmetric(14, 11))
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
        let Some(c) = self.messages.get(&ch_id) else { return };
        let (more, loading) = (c.more, c.loading);
        let out = ScrollArea::vertical()
            .id_salt(("messages", ch_id))
            .stick_to_bottom(true)
            .auto_shrink(false)
            .show(ui, |ui| {
                if c.list.is_empty() && loading {
                    widgets::loading_center(ui, "Mesajlar yükleniyor…");
                    return;
                }
                ui.add_space(12.0);
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
                    message_row(ui, m, grouped, Some(m.user_id) == me_id);
                    prev = Some(m);
                }
                ui.add_space(8.0);
            });

        // Eski mesajlar başa eklendiyse görünen yeri koru.
        if let Some(old_h) = scroll_fix {
            let mut state = out.state;
            state.offset.y += out.content_size.y - old_h;
            state.store(ui.ctx(), out.id);
            ui.ctx().request_repaint();
        }
        self.last_content_h = out.content_size.y;

        // En üste yaklaşınca eski mesajları iste.
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

    // ------------------------------------------------------------ ekran izleme

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

    // -------------------------------------------------------------- pencereler

    fn modals(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.modal.take() else { return };
        let mut keep = true;
        let mut next: Option<Modal> = None;
        let frame = Frame::new()
            .fill(BG_SIDEBAR)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(16)
            .inner_margin(Margin::same(22))
            .shadow(egui::Shadow { offset: [0, 16], blur: 48, spread: 0, color: Color32::from_black_alpha(120) });
        let resp = egui::Modal::new(Id::new("modal"))
            .backdrop_color(Color32::from_black_alpha(128))
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(420.0);
                match modal {
                    Modal::CreateChannel { kind, mut name } => {
                        let title = if kind == "voice" { "Ses kanalı oluştur" } else { "Metin kanalı oluştur" };
                        ui.label(RichText::new(title).font(bold_font(18.0)).color(TEXT_STRONG));
                        ui.add_space(10.0);
                        let r = ui.add(
                            TextEdit::singleline(&mut name)
                                .hint_text(if kind == "voice" { "Kanal adı" } else { "kanal-adi" })
                                .char_limit(50)
                                .desired_width(f32::INFINITY)
                                .margin(Margin::symmetric(10, 8)),
                        );
                        if self.modal_focus {
                            r.request_focus();
                            self.modal_focus = false;
                        }
                        let mut create = widgets::enter_pressed(ui, &r);
                        ui.add_space(14.0);
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
                        ui.label(RichText::new("Kanalı sil").font(bold_font(18.0)).color(TEXT_STRONG));
                        ui.add_space(10.0);
                        ui.label(RichText::new(format!("“{name}” kanalı ve tüm mesajları silinecek. Emin misin?")).color(TEXT));
                        ui.add_space(14.0);
                        let (cancel, ok) = modal_buttons(ui, "Sil", true);
                        if cancel {
                            keep = false;
                        } else if ok {
                            self.ws(json!({ "t": "delete_channel", "channelId": id }));
                            keep = false;
                        } else {
                            next = Some(Modal::DeleteChannel { id, name });
                        }
                    }
                    Modal::Settings => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Ses ayarları").font(bold_font(18.0)).color(TEXT_STRONG));
                        });
                        ui.add_space(14.0);
                        widgets::section_label(ui, "Giriş hassasiyeti");
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
                        let level = if in_voice { level_db(self.levels.me) } else { -100.0 };
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
                        let hint = if !in_voice {
                            "Mikrofon seviyesini görmek için bir ses kanalına katıl.".to_string()
                        } else if auto {
                            "Arka plan gürültüsüne göre eşik kendiliğinden ayarlanıyor.".to_string()
                        } else {
                            format!("Eşik: {th:.0} dB. Çizginin sağına geçen sesler iletilir.")
                        };
                        ui.label(RichText::new(hint).size(12.5).color(MUTED));
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Kişilerin ses seviyesini değiştirmek için ses kanalında adlarına sağ tıkla.")
                                .size(12.5)
                                .color(MUTED),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            if ui.link(RichText::new("Çıkış yap").color(RED).size(13.5)).clicked() {
                                keep = false;
                                self.logout();
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::accent_button(ui, "Tamam", 100.0, true, false).clicked() {
                                    keep = false;
                                }
                            });
                        });
                        if keep {
                            next = Some(Modal::Settings);
                        }
                    }
                    Modal::ScreenShare { sources, mut tab, mut selected } => {
                        ui.label(RichText::new("Ekran paylaş").font(bold_font(18.0)).color(TEXT_STRONG));
                        ui.add_space(10.0);
                        // Sekmeler
                        Frame::new()
                            .fill(BG_INPUT)
                            .stroke(Stroke::new(1.0, BORDER))
                            .corner_radius(10)
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
                                        let bg = if active { ACTIVE } else if r.hovered() { HOVER } else { Color32::TRANSPARENT };
                                        ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
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
                                        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), Sense::click());
                                        let bg = if is_sel { BG_ELEV } else if r.hovered() { HOVER } else { BG_MAIN };
                                        let stroke = if is_sel { Stroke::new(1.5, ACCENT) } else { Stroke::new(1.0, BG_MAIN) };
                                        ui.painter().rect(rect.shrink(1.0), CornerRadius::same(8), bg, stroke, StrokeKind::Inside);
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
                        ui.add_space(14.0);
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
        if resp.should_close() || ctx.input(|i| i.key_pressed(Key::Escape)) {
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
                Frame::new()
                    .fill(BG_ELEV)
                    .stroke(Stroke::new(1.0, if error { RED } else { BORDER }))
                    .corner_radius(10)
                    .inner_margin(Margin::symmetric(16, 10))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(110) })
                    .show(ui, |ui| {
                        ui.set_max_width(520.0);
                        ui.label(RichText::new(text).color(TEXT_STRONG));
                    });
            });
    }

    /// Geliştirme: GOODBYEDC_SCREENSHOT=dosya.ppm ise birkaç saniye sonra ekran görüntüsü alıp çıkar.
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

// ---------------------------------------------------------------- parçalar

/// Kanal satırı. (tıklandı mı, sil düğmesine basıldı mı)
fn channel_row(ui: &mut Ui, icon: &str, name: &str, active: bool, loading: bool) -> (bool, bool) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::click());
    let hovered = resp.hovered() || ui.rect_contains_pointer(rect);
    let bg = if active { ACTIVE } else if hovered { HOVER } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
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

fn message_row(ui: &mut Ui, m: &Message, grouped: bool, _mine: bool) {
    let left = 16.0;
    let avatar = 38.0;
    let text_x = left + avatar + 14.0;
    if !grouped {
        ui.add_space(12.0);
    }
    let width = ui.available_width();
    let start = ui.cursor().min;
    // Arka plan (üzerine gelince) için önce yer ayır, sonra içeriği çiz.
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
            ui.add(egui::Label::new(RichText::new(&m.content).size(14.5).color(TEXT)).wrap().selectable(true));
        },
    );
    let h = inner.response.rect.height().max(if grouped { 20.0 } else { avatar });
    let row = egui::Rect::from_min_size(egui::pos2(start.x, start.y - 2.0), egui::vec2(width, h + 4.0));
    if ui.rect_contains_pointer(row) {
        ui.painter().set(bg_idx, egui::Shape::rect_filled(row, 0.0, Color32::from_white_alpha(4)));
    }
    if !grouped {
        let av = egui::Rect::from_min_size(egui::pos2(start.x + left, start.y + 1.0), Vec2::splat(avatar));
        widgets::paint_avatar(ui, av, &m.username, false);
    }
    ui.allocate_rect(egui::Rect::from_min_size(start, egui::vec2(width, h)), Sense::hover());
    ui.add_space(2.0);
}

/// İki düğme: Vazgeç / onay. (vazgeç, onay)
fn modal_buttons(ui: &mut Ui, ok_text: &str, ok_enabled: bool) -> (bool, bool) {
    let mut cancel = false;
    let mut ok = false;
    // Sabit yükseklikli satır: yoksa düzen tüm boş yüksekliği kaplar ve pencere uzar.
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 36.0), Layout::right_to_left(Align::Center), |ui| {
        ok = widgets::accent_button(ui, ok_text, 100.0, ok_enabled, false).clicked();
        cancel = widgets::secondary_button(ui, "Vazgeç", 90.0).clicked();
    });
    (cancel, ok)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

/// "Bugün 14:05" veya "07.11.2026 14:05" (yerel saat).
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
