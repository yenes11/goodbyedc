//! Ses kanalı bağlantısı: tek bir PeerConnection üzerinden Cloudflare
//! Realtime SFU'ya kendi mikrofonumuzu/ekranımızı gönderir (push) ve
//! diğerlerininkini çeker (pull).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::peer_connection::TrackEvent;
use libwebrtc::prelude::*;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_source::AudioSourceOptions;
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;

use crate::screen::{self, Capture};
use crate::sfu::{Sdp, Sfu};

/// Ekran izlerken arayüze aktarılan en yüksek kare hızı.
const VIEW_FPS: u64 = 30;

/// Arka plan görevlerinin çalıştığı tokio çalışma ortamı.
/// WebRTC geri çağrıları (on_track gibi) kendi iş parçacıklarında çalışır; oradan
/// `tokio::spawn` çağrılırsa program çöker. Bu yüzden görevler bu tutamaçla başlatılır.
static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

pub fn set_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    RUNTIME.get().expect("tokio çalışma ortamı ayarlanmadı").spawn(fut)
}

/// İzlenen ekranın son karesi.
pub struct ScreenFrame {
    /// Ekranı paylaşanın oturum id'si.
    pub owner: String,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Ses seviyeleri (0..1): kendi mikrofonumuz ve oturum id'sine göre diğerleri.
#[derive(Default, Clone)]
pub struct Levels {
    pub me: f32,
    /// Mikrofon şu an ses gönderiyor mu (hassasiyet eşiğini geçti mi)?
    pub transmitting: bool,
    /// Otomatik modda o anki eşik (dB).
    pub auto_threshold_db: f32,
    pub peers: HashMap<String, f32>,
}

/// Giriş hassasiyeti: mikrofon yalnızca ses bu eşiği geçince açılır.
#[derive(Clone, Copy)]
pub struct Gate {
    /// Otomatik: arka plan gürültüsüne göre eşik kendiliğinden ayarlanır.
    pub auto: bool,
    /// Elle ayarlanan eşik (dB, -100..0).
    pub threshold_db: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Self { auto: true, threshold_db: -45.0 }
    }
}

/// Bir oturumun müzik yayınının sahip anahtarı (ses ayarı ve seviyeler için).
pub fn music_owner(session: &str) -> String {
    format!("{session}#music")
}

/// 0..1 seviyeyi dB'ye çevirir (-100..0).
pub fn level_db(level: f32) -> f32 {
    if level <= 0.00001 {
        -100.0
    } else {
        (20.0 * level.log10()).clamp(-100.0, 0.0)
    }
}

/// Ses eşiği geçtikten sonra mikrofonun açık kalma süresi (kelime sonları kesilmesin).
const GATE_HOLD: Duration = Duration::from_millis(600);

/// Call ile paylaşılan, WebRTC callback'lerinden de erişilen durum.
#[derive(Default)]
pub struct Shared {
    pub deafened: AtomicBool,
    pub muted: AtomicBool,
    pub gate: Mutex<Gate>,
    /// Oturum id'si -> çalma yüksekliği (0..2, 1 = normal).
    pub volumes: Mutex<HashMap<String, f32>>,
    /// mid -> uzak ses track'i (yüksekliği ayarlamak için)
    pub audio_tracks: Mutex<HashMap<String, RtcAudioTrack>>,
    /// Bir süredir hiç veri gelmeyen (takılmış) ses track'lerinin mid'leri.
    pub stalled: Mutex<HashSet<String>>,
    /// WebRTC bağlantısının durumu ve bu duruma geçiş anı.
    pub pc_state: Mutex<Option<(PeerConnectionState, Instant)>>,
    /// mid -> uzak oturum id'si (görüntünün kime ait olduğunu bilmek için)
    pub mid_owner: Mutex<HashMap<String, String>>,
    /// mid -> ekran izleme görevi
    pub viewers: Mutex<HashMap<String, JoinHandle<()>>>,
    /// İzlenen ekranın son karesi (arayüz alıp dokuya çevirir).
    pub frame: Mutex<Option<ScreenFrame>>,
    /// Son ölçülen ses seviyeleri.
    pub levels: Mutex<Levels>,
    /// Yeni veri gelince arayüzü yeniden çizdirmek için.
    pub repaint: Mutex<Option<eframe::egui::Context>>,
}

impl Shared {
    pub fn request_repaint(&self) {
        if let Some(ctx) = self.repaint.lock().as_ref() {
            ctx.request_repaint();
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub session_id: String,
    pub tracks: Vec<String>,
}

/// Paylaşılan uygulama sesi (ör. Spotify).
struct LocalMusic {
    _capture: crate::appaudio::Capture,
    pacer: JoinHandle<()>,
    transceiver: RtpTransceiver,
    mid: String,
}

struct LocalScreen {
    _capture: Capture,
    transceiver: RtpTransceiver,
    mid: String,
}

pub struct Call {
    sfu: Sfu,
    factory: PeerConnectionFactory,
    pc: PeerConnection,
    pub session_id: String,
    mic: RtcAudioTrack,
    screen: Option<LocalScreen>,
    music: Option<LocalMusic>,
    /// "oturumId/trackAdı" -> mid
    pulled: HashMap<String, String>,
    /// Son bilinen katılımcılar ve izlenen ekran (başarısız çekmeleri yeniden denemek için).
    last_sync: (Vec<Participant>, Option<String>),
    /// Takıldığı için yeniden çekilen track'lerin son deneme anı.
    repulled: HashMap<String, Instant>,
    /// SFU'da kapatılamamış (susturulmuş) track'lerin mid'leri.
    zombies: HashSet<String>,
    shared: Arc<Shared>,
    stats_task: JoinHandle<()>,
}

fn offer_options() -> OfferOptions {
    // Varsayılan Some(false) değerleri, çektiğimiz (recvonly) track'leri
    // devre dışı bırakırdı; None ile yönlerine dokunulmaz.
    OfferOptions { ice_restart: false, offer_to_receive_audio: None, offer_to_receive_video: None }
}

fn parse(sdp: &Sdp) -> Result<SessionDescription> {
    let kind = match sdp.kind.as_str() {
        "offer" => SdpType::Offer,
        "answer" => SdpType::Answer,
        other => return Err(anyhow!("bilinmeyen SDP tipi: {other}")),
    };
    SessionDescription::parse(&sdp.sdp, kind).map_err(|e| anyhow!("{e}"))
}

impl Call {
    pub async fn join(factory: PeerConnectionFactory, sfu: Sfu, shared: Arc<Shared>, muted: bool) -> Result<Self> {
        let mut config = RtcConfiguration::default();
        config.ice_servers = sfu.ice_servers().await.unwrap_or_else(|e| {
            log::warn!("ICE sunucuları alınamadı, sadece STUN: {e}");
            vec![IceServer {
                urls: vec!["stun:stun.cloudflare.com:3478".into()],
                username: String::new(),
                password: String::new(),
            }]
        });
        let pc = factory.create_peer_connection(config).map_err(|e| anyhow!("{e}"))?;

        *shared.pc_state.lock() = None;
        shared.stalled.lock().clear();
        {
            let shared = shared.clone();
            pc.on_connection_state_change(Some(Box::new(move |s| {
                log::info!("bağlantı durumu: {s:?}");
                *shared.pc_state.lock() = Some((s, Instant::now()));
            })));
        }
        {
            let shared = shared.clone();
            pc.on_track(Some(Box::new(move |ev: TrackEvent| on_track(&shared, ev))));
        }

        // Windows diğer uygulamaların sesini kısmasın (aygıt açılmadan önce ve sonra).
        let _ = tokio::task::spawn_blocking(crate::ducking::disable).await;

        // Gerçek mikrofon/hoparlör (platform ADM, yankı engelleme dahil).
        if !factory.acquire_platform_adm() {
            return Err(anyhow!("Ses aygıtı başlatılamadı"));
        }
        factory.set_adm_recording_enabled(true);
        factory.set_adm_playout_enabled(true);
        let _ = tokio::task::spawn_blocking(crate::ducking::disable).await;
        log_devices(&factory);

        let session_id = sfu.new_session().await?;
        let mic = factory.create_device_audio_track("mic");
        shared.muted.store(muted, Ordering::Relaxed);
        // Hassasiyet döngüsü eşik geçilince açar.
        mic.set_enabled(false);

        // Mikrofon seviyesini ayrıca ölç (WebRTC, track kapalıyken ölçmüyor).
        let meter = tokio::task::spawn_blocking(crate::meter::Meter::start).await.ok().flatten();
        if meter.is_none() {
            log::warn!("mikrofon ölçer yok: giriş hassasiyeti devre dışı, mikrofon hep açık");
        }
        let stats_task = spawn(stats_loop(pc.clone(), mic.clone(), meter, shared.clone()));
        let call = Self {
            sfu,
            factory,
            pc,
            session_id,
            mic: mic.clone(),
            screen: None,
            music: None,
            pulled: HashMap::new(),
            last_sync: (Vec::new(), None),
            repulled: HashMap::new(),
            zombies: HashSet::new(),
            shared,
            stats_task,
        };
        let tr = call.add_send_transceiver(MediaStreamTrack::Audio(mic), vec![])?;
        call.push_track(&tr, "mic").await.context("mikrofon gönderilemedi")?;
        Ok(call)
    }

    fn add_send_transceiver(
        &self,
        track: MediaStreamTrack,
        send_encodings: Vec<RtpEncodingParameters>,
    ) -> Result<RtpTransceiver> {
        self.pc
            .add_transceiver(
                track,
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::SendOnly,
                    stream_ids: vec![self.session_id.clone()],
                    send_encodings,
                },
            )
            .map_err(|e| anyhow!("{e}"))
    }

    /// Yerel bir transceiver'ı SFU'ya "trackName" adıyla yayınlar.
    async fn push_track(&self, tr: &RtpTransceiver, name: &str) -> Result<String> {
        let offer = self.pc.create_offer(offer_options()).await.map_err(|e| anyhow!("{e}"))?;
        self.pc.set_local_description(offer.clone()).await.map_err(|e| anyhow!("{e}"))?;
        let mid = tr.mid().ok_or_else(|| anyhow!("transceiver mid yok"))?;
        let res = self
            .sfu
            .tracks_new(
                &self.session_id,
                json!({
                    "sessionDescription": { "type": "offer", "sdp": offer.to_string() },
                    "tracks": [{ "location": "local", "mid": mid, "trackName": name }]
                }),
            )
            .await?;
        let answer = res.session_description.ok_or_else(|| anyhow!("SFU answer göndermedi"))?;
        self.pc.set_remote_description(parse(&answer)?).await.map_err(|e| anyhow!("{e}"))?;
        Ok(mid)
    }

    /// SFU'nun gönderdiği offer'a cevap verir.
    async fn answer_offer(&self, offer: &Sdp) -> Result<()> {
        self.pc.set_remote_description(parse(offer)?).await.map_err(|e| anyhow!("{e}"))?;
        let answer = self.pc.create_answer(AnswerOptions::default()).await.map_err(|e| anyhow!("{e}"))?;
        self.pc.set_local_description(answer.clone()).await.map_err(|e| anyhow!("{e}"))?;
        self.sfu
            .renegotiate(&self.session_id, Sdp { kind: "answer".into(), sdp: answer.to_string() })
            .await
    }

    async fn pull(&mut self, remote_session: &str, track: &str) -> Result<()> {
        let res = self
            .sfu
            .tracks_new(
                &self.session_id,
                json!({ "tracks": [{ "location": "remote", "sessionId": remote_session, "trackName": track }] }),
            )
            .await?;
        let mid = res
            .tracks
            .first()
            .and_then(|t| t.mid.clone())
            .ok_or_else(|| anyhow!("SFU track mid döndürmedi"))?;
        // on_track tetiklenmeden önce sahibini kaydet. Müzik ayrı bir sahip anahtarı alır
        // ("oturum#music"): konuşma halkasını yakmasın, ses ayarı ayrı olsun.
        let owner = if track == "music" { music_owner(remote_session) } else { remote_session.to_string() };
        self.shared.mid_owner.lock().insert(mid.clone(), owner);
        if res.requires_immediate_renegotiation {
            if let Some(offer) = &res.session_description {
                if let Err(e) = self.answer_offer(offer).await {
                    // Yarım kalan çekme: SFU'da açık kalırsa sonraki denemeyle sesi iki kez duyarız.
                    // Önce susturup kapatmayı dene; olmazsa sonra tekrar denenir.
                    self.drop_mid(&mid).await;
                    return Err(e);
                }
            }
        }
        self.pulled.insert(format!("{remote_session}/{track}"), mid);
        Ok(())
    }

    async fn unpull(&mut self, key: &str) -> Result<()> {
        let Some(mid) = self.pulled.remove(key) else { return Ok(()) };
        self.drop_mid(&mid).await;
        Ok(())
    }

    /// Bir uzak track'i susturur, yerel kayıtlardan siler ve SFU'da kapatır.
    /// Kapatılamazsa `zombies` listesine eklenir ve sonra yeniden denenir.
    async fn drop_mid(&mut self, mid: &str) {
        if let Some(t) = self.shared.audio_tracks.lock().remove(mid) {
            t.set_enabled(false);
        }
        if let Some(h) = self.shared.viewers.lock().remove(mid) {
            h.abort();
        }
        self.shared.mid_owner.lock().remove(mid);
        if let Err(e) = self.close_mid(mid).await {
            log::warn!("track kapatılamadı ({mid}), sonra yeniden denenecek: {e:#}");
            self.zombies.insert(mid.to_string());
        } else {
            self.zombies.remove(mid);
        }
    }

    async fn close_mid(&self, mid: &str) -> Result<()> {
        let res = self
            .sfu
            .tracks_close(&self.session_id, json!({ "tracks": [{ "mid": mid }], "force": true }))
            .await?;
        if let Some(sd) = &res.session_description {
            if sd.kind == "offer" {
                self.answer_offer(sd).await?;
            }
        }
        Ok(())
    }

    /// Ses kanalındaki katılımcılara göre çekilen track'leri günceller.
    /// `watch`: ekranını izlediğimiz kişinin oturum id'si.
    pub async fn sync(&mut self, participants: &[Participant], watch: Option<&str>) {
        self.last_sync = (participants.to_vec(), watch.map(String::from));
        let mut desired: HashSet<(String, String)> = HashSet::new();
        for p in participants {
            if p.session_id == self.session_id {
                continue;
            }
            if p.tracks.iter().any(|t| t == "mic") {
                desired.insert((p.session_id.clone(), "mic".into()));
            }
            if p.tracks.iter().any(|t| t == "music") {
                desired.insert((p.session_id.clone(), "music".into()));
            }
            if watch == Some(p.session_id.as_str()) && p.tracks.iter().any(|t| t == "screen") {
                desired.insert((p.session_id.clone(), "screen".into()));
            }
        }
        let desired_keys: HashSet<String> = desired.iter().map(|(s, t)| format!("{s}/{t}")).collect();

        let stale: Vec<String> = self.pulled.keys().filter(|k| !desired_keys.contains(*k)).cloned().collect();
        for key in stale {
            if let Err(e) = self.unpull(&key).await {
                log::warn!("track kapatılamadı {key}: {e:#}");
            }
        }
        for (sid, track) in desired {
            if self.pulled.contains_key(&format!("{sid}/{track}")) {
                continue;
            }
            if let Err(e) = self.pull(&sid, &track).await {
                log::warn!("track çekilemedi {sid}/{track}: {e:#}");
            }
        }
    }

    /// Bağlantı koptu mu (yeniden katılmak gerekir mi)?
    pub fn is_broken(&self) -> bool {
        match *self.shared.pc_state.lock() {
            Some((PeerConnectionState::Failed, _)) => true,
            Some((PeerConnectionState::Disconnected, t)) => t.elapsed() > Duration::from_secs(8),
            _ => false,
        }
    }

    /// Düzenli bakım: takılmış ses track'lerini kapatıp yeniden çeker ve
    /// daha önce çekilemeyen track'leri yeniden dener.
    pub async fn heal(&mut self) {
        // Kapatılamamış track'leri yeniden kapatmayı dene.
        let zombies: Vec<String> = self.zombies.iter().cloned().collect();
        for mid in zombies {
            self.drop_mid(&mid).await;
        }
        // Hiçbir çekmeye ait olmayan ses track'leri (sahipsiz kopyalar): sustur ve kapat.
        let live: HashSet<String> = self.pulled.values().cloned().collect();
        let orphans: Vec<String> =
            self.shared.audio_tracks.lock().keys().filter(|m| !live.contains(*m)).cloned().collect();
        for mid in orphans {
            log::warn!("sahipsiz ses track'i kapatılıyor: {mid}");
            self.drop_mid(&mid).await;
        }
        let stalled: Vec<String> = self.shared.stalled.lock().drain().collect();
        for mid in stalled {
            let Some(key) = self.pulled.iter().find(|(_, m)| **m == mid).map(|(k, _)| k.clone()) else { continue };
            if self.repulled.get(&key).is_some_and(|t| t.elapsed() < Duration::from_secs(15)) {
                continue;
            }
            log::warn!("{key} sesinden bir süredir veri gelmiyor, yeniden çekiliyor");
            self.repulled.insert(key.clone(), Instant::now());
            let _ = self.unpull(&key).await;
        }
        let (participants, watch) = self.last_sync.clone();
        self.sync(&participants, watch.as_deref()).await;
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
        if muted {
            self.mic.set_enabled(false);
        }
        // Açılış, hassasiyet döngüsünde ses eşiği geçince olur.
    }

    pub fn set_deafened(&self, deafened: bool) {
        self.shared.deafened.store(deafened, Ordering::Relaxed);
        // Yalnızca takip edilen ses track'leri: susturulmuş kopyalar yeniden açılmasın.
        for t in self.shared.audio_tracks.lock().values() {
            t.set_enabled(!deafened);
        }
    }

    pub async fn start_screen(&mut self, source_id: u64, kind: String) -> Result<()> {
        if self.screen.is_some() {
            self.stop_screen().await?;
        }
        let video = NativeVideoSource::new(VideoResolution { width: 1920, height: 1080 }, true);
        let capture = Capture::start(source_id, kind, video.clone())?;
        let track = self.factory.create_video_track("screen", video);
        let enc = RtpEncodingParameters {
            max_bitrate: Some(3_000_000),
            max_framerate: Some(screen::FPS as f64),
            ..Default::default()
        };
        let tr = self.add_send_transceiver(MediaStreamTrack::Video(track), vec![enc])?;
        let mid = self.push_track(&tr, "screen").await.context("ekran gönderilemedi")?;
        self.screen = Some(LocalScreen { _capture: capture, transceiver: tr, mid });
        Ok(())
    }

    pub async fn stop_screen(&mut self) -> Result<()> {
        let Some(s) = self.screen.take() else { return Ok(()) };
        let LocalScreen { _capture, transceiver, mid } = s;
        drop(_capture);
        self.stop_local(transceiver, mid).await
    }

    /// Bir uygulamanın sesini (pid) "music" track'i olarak yayınlar.
    pub async fn start_music(&mut self, pid: u32) -> Result<()> {
        use crate::appaudio::{CHANNELS, SAMPLE_RATE};
        if self.music.is_some() {
            self.stop_music().await?;
        }
        // Yakalanan ses tampona girer; 10 ms'lik dilimler düzenli olarak gönderilir.
        // Uygulama sustuğunda (duraklatınca) sessizlik gönderilir, böylece yayın kesilmez.
        let buf = Arc::new(Mutex::new(std::collections::VecDeque::<i16>::new()));
        let capture = {
            let buf = buf.clone();
            crate::appaudio::capture(pid, move |d| {
                let mut b = buf.lock();
                b.extend(d.iter().copied());
                // En fazla 200 ms gecikme: fazlası atılır.
                let max = (SAMPLE_RATE / 5 * CHANNELS) as usize;
                if b.len() > max {
                    let extra = b.len() - max;
                    b.drain(..extra);
                }
            })?
        };
        let source = NativeAudioSource::new(
            AudioSourceOptions { echo_cancellation: false, noise_suppression: false, auto_gain_control: false },
            SAMPLE_RATE,
            CHANNELS,
            0,
        );
        let pacer = {
            let source = source.clone();
            spawn(async move {
                let chunk = (SAMPLE_RATE / 100 * CHANNELS) as usize;
                let mut tick = tokio::time::interval(Duration::from_millis(10));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
                // Başta biraz biriktir (titremeye karşı).
                let mut primed = false;
                loop {
                    tick.tick().await;
                    let data: Vec<i16> = {
                        let mut b = buf.lock();
                        if !primed && b.len() >= chunk * 4 {
                            primed = true;
                        }
                        if primed && b.len() >= chunk {
                            b.drain(..chunk).collect()
                        } else {
                            if b.is_empty() {
                                primed = false;
                            }
                            vec![0; chunk]
                        }
                    };
                    let frame = AudioFrame {
                        data: data.into(),
                        sample_rate: SAMPLE_RATE,
                        num_channels: CHANNELS,
                        samples_per_channel: SAMPLE_RATE / 100,
                    };
                    let _ = source.capture_frame(&frame).await;
                }
            })
        };
        let track = self.factory.create_audio_track("music", source);
        let enc = RtpEncodingParameters { max_bitrate: Some(128_000), ..Default::default() };
        let tr = match self.add_send_transceiver(MediaStreamTrack::Audio(track), vec![enc]) {
            Ok(t) => t,
            Err(e) => {
                pacer.abort();
                return Err(e);
            }
        };
        let mid = match self.push_track(&tr, "music").await {
            Ok(m) => m,
            Err(e) => {
                pacer.abort();
                let _ = tr.stop();
                return Err(e).context("ses yayını gönderilemedi");
            }
        };
        self.music = Some(LocalMusic { _capture: capture, pacer, transceiver: tr, mid });
        Ok(())
    }

    pub async fn stop_music(&mut self) -> Result<()> {
        let Some(m) = self.music.take() else { return Ok(()) };
        let LocalMusic { _capture, pacer, transceiver, mid } = m;
        drop(_capture);
        pacer.abort();
        self.stop_local(transceiver, mid).await
    }

    /// Yerel bir yayını durdurur ve SFU'da kapatır.
    async fn stop_local(&mut self, transceiver: RtpTransceiver, mid: String) -> Result<()> {
        transceiver.stop().map_err(|e| anyhow!("{e}"))?;
        let offer = self.pc.create_offer(offer_options()).await.map_err(|e| anyhow!("{e}"))?;
        self.pc.set_local_description(offer.clone()).await.map_err(|e| anyhow!("{e}"))?;
        let res = self
            .sfu
            .tracks_close(
                &self.session_id,
                json!({
                    "tracks": [{ "mid": mid }],
                    "sessionDescription": { "type": "offer", "sdp": offer.to_string() },
                    "force": false
                }),
            )
            .await?;
        if let Some(sd) = &res.session_description {
            if sd.kind == "answer" {
                self.pc.set_remote_description(parse(sd)?).await.map_err(|e| anyhow!("{e}"))?;
            }
        }
        Ok(())
    }

    pub fn tracks(&self) -> Vec<&'static str> {
        let mut t = vec!["mic"];
        if self.screen.is_some() {
            t.push("screen");
        }
        if self.music.is_some() {
            t.push("music");
        }
        t
    }

    pub fn leave(mut self) {
        self.stats_task.abort();
        self.screen = None; // yakalamayı durdurur
        if let Some(m) = self.music.take() {
            m.pacer.abort();
        }
        for (_, h) in self.shared.viewers.lock().drain() {
            h.abort();
        }
        self.shared.mid_owner.lock().clear();
        self.shared.audio_tracks.lock().clear();
        *self.shared.frame.lock() = None;
        *self.shared.levels.lock() = Levels::default();
        self.shared.request_repaint();
        self.pc.close();
        self.factory.release_platform_adm();
    }
}

fn on_track(shared: &Arc<Shared>, ev: TrackEvent) {
    let mid = ev.transceiver.mid().unwrap_or_default();
    match ev.track {
        MediaStreamTrack::Audio(t) => {
            // Ses, platform ADM üzerinden otomatik çalınır.
            if shared.deafened.load(Ordering::Relaxed) {
                t.set_enabled(false);
            }
            // Aynı kişiden önceden gelen ses varsa (kopya) sustur; bakım döngüsü kapatır.
            let owners = shared.mid_owner.lock().clone();
            if let Some(owner) = owners.get(&mid) {
                for (m, old) in shared.audio_tracks.lock().iter() {
                    if m != &mid && owners.get(m) == Some(owner) {
                        log::warn!("{owner} için ikinci ses track'i geldi, eskisi ({m}) susturuldu");
                        old.set_enabled(false);
                    }
                }
            }
            // Yükseklik ayarı hassasiyet döngüsünde uygulanır (WebRTC iş parçacığını bekletmemek için).
            shared.audio_tracks.lock().insert(mid, t);
        }
        MediaStreamTrack::Video(t) => {
            let owner = shared.mid_owner.lock().get(&mid).cloned().unwrap_or_default();
            let shared2 = shared.clone();
            let handle = spawn(view_screen(shared2, owner, t));
            if let Some(old) = shared.viewers.lock().insert(mid, handle) {
                old.abort();
            }
        }
    }
}

/// Uzak ekran görüntüsünü RGBA'ya çevirip arayüze bırakır.
async fn view_screen(shared: Arc<Shared>, owner: String, track: RtcVideoTrack) {
    let mut stream = NativeVideoStream::new(track);
    let min_gap = Duration::from_millis(1000 / VIEW_FPS);
    let mut last = Instant::now() - min_gap;
    while let Some(frame) = stream.next().await {
        if last.elapsed() < min_gap {
            continue;
        }
        last = Instant::now();
        let converted = tokio::task::spawn_blocking(move || screen::frame_to_rgba(&frame)).await;
        let Ok(Some((width, height, rgba))) = converted else { continue };
        *shared.frame.lock() = Some(ScreenFrame { owner: owner.clone(), width, height, rgba });
        shared.request_repaint();
    }
}

fn log_devices(factory: &PeerConnectionFactory) {
    let rec: Vec<String> = (0..factory.recording_devices().max(0) as u16).map(|i| factory.recording_device_name(i)).collect();
    let play: Vec<String> = (0..factory.playout_devices().max(0) as u16).map(|i| factory.playout_device_name(i)).collect();
    log::info!("mikrofonlar: {rec:?}");
    log::info!("hoparlörler: {play:?}");
    log::info!(
        "kayıt hazır: {}, çalma hazır: {}",
        factory.recording_is_initialized(),
        factory.playout_is_initialized()
    );
}

/// Her 50 ms'de ses seviyelerini ölçer:
/// - "kim konuşuyor" bilgisini arayüze verir,
/// - giriş hassasiyetine göre mikrofonu açıp kapatır,
/// - kişilere özel ses yüksekliklerini uygular.
/// Ayrıca birkaç saniyede bir gönderilen/alınan ses miktarını log'a yazar.
async fn stats_loop(pc: PeerConnection, mic: RtcAudioTrack, meter: Option<crate::meter::Meter>, shared: Arc<Shared>) {
    use libwebrtc::stats::RtcStats;
    let mut tick: u32 = 0;
    let mut open_until: Option<Instant> = None;
    let mut mic_on = false;
    // Otomatik mod: arka plan gürültüsü tahmini (dB).
    let mut noise_db: f32 = -60.0;
    let mut applied: HashMap<String, f32> = HashMap::new();
    // mid -> (alınan bayt, son değişim anı)
    let mut inbound: HashMap<String, (u64, Instant)> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        tick += 1;
        let Ok(stats) = pc.get_stats().await else { continue };

        let mut me = 0.0f64;
        let mut sent = 0u64;
        let mut peers = serde_json::Map::new();
        let mut recv = serde_json::Map::new();
        let owners = shared.mid_owner.lock().clone();
        // Çekilen her ses track'i için alınan bayt (hiç istatistiği yoksa 0).
        let mut bytes_now: HashMap<String, u64> =
            shared.audio_tracks.lock().keys().map(|m| (m.clone(), 0)).collect();
        for st in &stats {
            match st {
                RtcStats::MediaSource(m) if m.source.kind == "audio" => me = me.max(m.audio.audio_level),
                RtcStats::OutboundRtp(o) if o.stream.kind == "audio" => sent += o.sent.bytes_sent,
                RtcStats::InboundRtp(i) if i.stream.kind == "audio" => {
                    bytes_now.insert(i.inbound.mid.clone(), i.inbound.bytes_received);
                    if let Some(owner) = owners.get(&i.inbound.mid) {
                        peers.insert(owner.clone(), json!(i.inbound.audio_level));
                        recv.insert(owner.clone(), json!(i.inbound.bytes_received));
                    }
                }
                _ => {}
            }
        }
        // Takılan ses: 6 saniyedir hiç yeni veri gelmiyorsa yeniden çekilsin.
        {
            let now = Instant::now();
            inbound.retain(|mid, _| bytes_now.contains_key(mid));
            for (mid, bytes) in &bytes_now {
                let e = inbound.entry(mid.clone()).or_insert((*bytes, now));
                if e.0 != *bytes {
                    *e = (*bytes, now);
                } else if now.duration_since(e.1) > Duration::from_secs(6) && owners.contains_key(mid) {
                    shared.stalled.lock().insert(mid.clone());
                    e.1 = now;
                }
            }
        }
        if tick % 100 == 0 {
            log::info!("ses: mikrofon seviyesi {me:.3}, açık {mic_on}, gönderilen {sent} B, alınan {recv:?}, seviyeler {peers:?}");
        }

        // Giriş hassasiyeti
        // Seviye ayrı ölçerden gelir; WebRTC'nin değeri track kapalıyken 0 olur.
        let level = meter.as_ref().map(|m| m.take_level()).unwrap_or(me as f32);
        let db = level_db(level);
        if tick % 100 == 0 {
            log::info!("ses: ölçer seviyesi {level:.3} ({db:.0} dB)");
        }
        let gate = *shared.gate.lock();
        // Gürültü tabanı: aşağı hızlı, yukarı yavaş iner/çıkar (konuşma tabanı yükseltmesin).
        // Gürültü tabanı: aşağı hızlı iner; yukarı yalnızca konuşulmuyorken (eşik kapalıyken)
        // ve çok yavaş çıkar. Böylece uzun konuşmalar "gürültü" sanılıp eşiği yükseltmez.
        let speaking_now = open_until.is_some_and(|t| Instant::now() < t);
        if tick <= 20 {
            // İlk saniye: odanın gürültüsünü doğrudan ölç (en düşük seviye).
            noise_db = if tick == 1 { db } else { noise_db.min(db) };
        } else if db < noise_db {
            noise_db += (db - noise_db) * 0.3;
        } else {
            // Sessizken ~25 sn'de, konuşurken ~2 dk'da uyum sağlar.
            noise_db += (db - noise_db) * if speaking_now { 0.0004 } else { 0.002 };
        }
        // Konuşma neredeyse her zaman -35 dB'nin üstündedir; eşik bunu geçmesin.
        let auto_threshold = (noise_db + 12.0).clamp(-60.0, -38.0);
        let threshold = if gate.auto { auto_threshold } else { gate.threshold_db };
        let now = Instant::now();
        if db >= threshold {
            open_until = Some(now + GATE_HOLD);
        }
        // Ölçer yoksa eşik uygulanmaz: mikrofon hep açık.
        let open = meter.is_none() || open_until.is_some_and(|t| now < t);
        let want = open && !shared.muted.load(Ordering::Relaxed);
        if want != mic_on {
            mic.set_enabled(want);
            mic_on = want;
        }

        // Kişilere özel ses yüksekliği
        {
            let volumes = shared.volumes.lock().clone();
            let tracks: Vec<(String, RtcAudioTrack)> =
                shared.audio_tracks.lock().iter().map(|(m, t)| (m.clone(), t.clone())).collect();
            applied.retain(|mid, _| tracks.iter().any(|(m, _)| m == mid));
            for (mid, track) in tracks {
                let Some(owner) = owners.get(&mid) else { continue };
                let v = volumes.get(owner).copied().unwrap_or(1.0);
                if applied.get(&mid) != Some(&v) {
                    track.set_volume(v as f64);
                    applied.insert(mid, v);
                }
            }
        }

        {
            let mut lv = shared.levels.lock();
            lv.me = level;
            lv.transmitting = mic_on;
            lv.auto_threshold_db = auto_threshold;
            lv.peers = peers.iter().filter_map(|(k, v)| Some((k.clone(), v.as_f64()? as f32))).collect();
        }
        shared.request_repaint();
    }
}
