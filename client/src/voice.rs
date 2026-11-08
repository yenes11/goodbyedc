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
    pub peers: HashMap<String, f32>,
}

/// Call ile paylaşılan, WebRTC callback'lerinden de erişilen durum.
#[derive(Default)]
pub struct Shared {
    pub deafened: AtomicBool,
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

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub session_id: String,
    pub tracks: Vec<String>,
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
    /// "oturumId/trackAdı" -> mid
    pulled: HashMap<String, String>,
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

        pc.on_connection_state_change(Some(Box::new(|s| log::info!("bağlantı durumu: {s:?}"))));
        {
            let shared = shared.clone();
            pc.on_track(Some(Box::new(move |ev: TrackEvent| on_track(&shared, ev))));
        }

        // Gerçek mikrofon/hoparlör (platform ADM, yankı engelleme dahil).
        if !factory.acquire_platform_adm() {
            return Err(anyhow!("Ses aygıtı başlatılamadı"));
        }
        factory.set_adm_recording_enabled(true);
        factory.set_adm_playout_enabled(true);
        log_devices(&factory);

        let session_id = sfu.new_session().await?;
        let mic = factory.create_device_audio_track("mic");
        mic.set_enabled(!muted);

        let stats_task = spawn(stats_loop(pc.clone(), shared.clone()));
        let call = Self {
            sfu,
            factory,
            pc,
            session_id,
            mic: mic.clone(),
            screen: None,
            pulled: HashMap::new(),
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
        // on_track tetiklenmeden önce sahibini kaydet.
        self.shared.mid_owner.lock().insert(mid.clone(), remote_session.to_string());
        if res.requires_immediate_renegotiation {
            if let Some(offer) = &res.session_description {
                self.answer_offer(offer).await?;
            }
        }
        self.pulled.insert(format!("{remote_session}/{track}"), mid);
        Ok(())
    }

    async fn unpull(&mut self, key: &str) -> Result<()> {
        let Some(mid) = self.pulled.remove(key) else { return Ok(()) };
        if let Some(h) = self.shared.viewers.lock().remove(&mid) {
            h.abort();
        }
        self.shared.mid_owner.lock().remove(&mid);
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
        let mut desired: HashSet<(String, String)> = HashSet::new();
        for p in participants {
            if p.session_id == self.session_id {
                continue;
            }
            if p.tracks.iter().any(|t| t == "mic") {
                desired.insert((p.session_id.clone(), "mic".into()));
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

    pub fn set_muted(&self, muted: bool) {
        self.mic.set_enabled(!muted);
    }

    pub fn set_deafened(&self, deafened: bool) {
        self.shared.deafened.store(deafened, Ordering::Relaxed);
        for r in self.pc.receivers() {
            if let Some(MediaStreamTrack::Audio(t)) = r.track() {
                t.set_enabled(!deafened);
            }
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
        if self.screen.is_some() {
            vec!["mic", "screen"]
        } else {
            vec!["mic"]
        }
    }

    pub fn leave(mut self) {
        self.stats_task.abort();
        self.screen = None; // yakalamayı durdurur
        for (_, h) in self.shared.viewers.lock().drain() {
            h.abort();
        }
        self.shared.mid_owner.lock().clear();
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

/// Her 250 ms'de ses seviyelerini ölçüp arayüze gönderir ("kim konuşuyor").
/// Ayrıca birkaç saniyede bir gönderilen/alınan ses miktarını log'a yazar.
async fn stats_loop(pc: PeerConnection, shared: Arc<Shared>) {
    use libwebrtc::stats::RtcStats;
    let mut tick: u32 = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        tick += 1;
        let Ok(stats) = pc.get_stats().await else { continue };

        let mut me = 0.0f64;
        let mut sent = 0u64;
        let mut peers = serde_json::Map::new();
        let mut recv = serde_json::Map::new();
        let owners = shared.mid_owner.lock().clone();
        for st in &stats {
            match st {
                RtcStats::MediaSource(m) if m.source.kind == "audio" => me = me.max(m.audio.audio_level),
                RtcStats::OutboundRtp(o) if o.stream.kind == "audio" => sent += o.sent.bytes_sent,
                RtcStats::InboundRtp(i) if i.stream.kind == "audio" => {
                    if let Some(owner) = owners.get(&i.inbound.mid) {
                        peers.insert(owner.clone(), json!(i.inbound.audio_level));
                        recv.insert(owner.clone(), json!(i.inbound.bytes_received));
                    }
                }
                _ => {}
            }
        }
        if tick % 20 == 0 {
            log::info!("ses: mikrofon seviyesi {me:.3}, gönderilen {sent} B, alınan {recv:?}, seviyeler {peers:?}");
        }
        {
            let mut lv = shared.levels.lock();
            lv.me = me as f32;
            lv.peers = peers.iter().filter_map(|(k, v)| Some((k.clone(), v.as_f64()? as f32))).collect();
        }
        shared.request_repaint();
    }
}
