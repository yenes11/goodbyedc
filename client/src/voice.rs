//! The voice channel connection. Over a single WebRTC `PeerConnection` to
//! Cloudflare's Realtime SFU it sends ("pushes") our own microphone, screen and
//! shared app audio, and receives ("pulls") everyone else's.
//!
//! The big picture:
//!   - `Call` is one voice session: created by `Call::join`, destroyed by `leave`.
//!   - Our server tells every client who is in the channel and which tracks each
//!     person publishes. `Call::sync` compares that list with what we currently
//!     pull and pulls/unpulls the difference.
//!   - Received audio is played automatically by WebRTC's audio device module
//!     (the "ADM", which owns the speakers and microphone).
//!   - `stats_loop` runs every 50 ms: it measures levels (who is speaking), opens
//!     and closes our mic according to the sensitivity gate, applies per-user
//!     volumes and detects stalled audio.
//!   - `Shared` holds state that both the UI thread and WebRTC's own threads touch;
//!     it's wrapped in `Arc` and protected by `Mutex`es / atomics.
//!
//! See sfu.rs for what "SDP", "track", "mid" and "renegotiation" mean.

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

/// Highest frame rate passed to the UI while watching a screen.
const VIEW_FPS: u64 = 30;

/// The tokio runtime our background tasks run on.
/// WebRTC callbacks (like `on_track`) run on WebRTC's own threads; calling
/// `tokio::spawn` there crashes the program because those threads don't belong to
/// tokio. So tasks are started through this stored handle instead. It's a global
/// (`static`) set once at startup by `set_runtime`.
static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

pub fn set_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

/// Spawns a future on our runtime from any thread.
/// Generic signature: `F` is any future that can move between threads (`Send`)
/// and doesn't borrow temporary data (`'static`); its output must be `Send` too.
fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    RUNTIME.get().expect("tokio çalışma ortamı ayarlanmadı").spawn(fut)
}

/// The latest frame of the screen we're watching.
pub struct ScreenFrame {
    /// Session id of the person sharing the screen.
    pub owner: String,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Audio levels (0..1): our own microphone, and everyone else by session id.
#[derive(Default, Clone)]
pub struct Levels {
    pub me: f32,
    /// Is the microphone transmitting right now (above the sensitivity threshold)?
    pub transmitting: bool,
    /// The current threshold in automatic mode (dB).
    pub auto_threshold_db: f32,
    /// Round-trip time to Cloudflare's server (ms), if it could be measured.
    pub rtt_ms: Option<f32>,
    pub peers: HashMap<String, f32>,
}

/// Input sensitivity (voice gate): the mic only opens when sound is above the threshold.
#[derive(Clone, Copy)]
pub struct Gate {
    /// Automatic: the threshold follows the background noise.
    pub auto: bool,
    /// Manually chosen threshold (dB, -100..0).
    pub threshold_db: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Self { auto: true, threshold_db: -45.0 }
    }
}

/// Owner key of a session's music stream (separate from its mic for volume and levels).
pub fn music_owner(session: &str) -> String {
    format!("{session}#music")
}

/// Converts a 0..1 level to decibels (clamped to -100..0).
pub fn level_db(level: f32) -> f32 {
    if level <= 0.00001 {
        -100.0
    } else {
        (20.0 * level.log10()).clamp(-100.0, 0.0)
    }
}

/// How long the mic stays open after the level drops below the threshold
/// (so word endings aren't cut off).
const GATE_HOLD: Duration = Duration::from_millis(600);

/// State shared between the UI, the `Call` and WebRTC's callback threads.
///
/// `#[derive(Default)]` builds it with every field at its default (false, empty
/// maps, `None`). Each field has its own lock (`Mutex`) or is atomic, so different
/// threads can touch different fields without blocking each other. `parking_lot::Mutex`
/// is used (no poisoning, `.lock()` returns the guard directly).
#[derive(Default)]
pub struct Shared {
    pub deafened: AtomicBool,
    pub muted: AtomicBool,
    /// Should shared app audio be normalized before sending?
    pub music_normalize: AtomicBool,
    pub gate: Mutex<Gate>,
    /// Session id -> playback volume (0..2, 1 = normal). Written by the UI every frame.
    pub volumes: Mutex<HashMap<String, f32>>,
    /// mid -> remote audio track (to set its volume, mute it, ...).
    pub audio_tracks: Mutex<HashMap<String, RtcAudioTrack>>,
    /// mids of audio tracks that haven't received data for a while (stalled).
    pub stalled: Mutex<HashSet<String>>,
    /// The WebRTC connection's state and when it entered that state.
    pub pc_state: Mutex<Option<(PeerConnectionState, Instant)>>,
    /// mid -> remote session id (to know whose audio/video a track is).
    pub mid_owner: Mutex<HashMap<String, String>>,
    /// mid -> screen-viewing task.
    pub viewers: Mutex<HashMap<String, JoinHandle<()>>>,
    /// The latest frame of the watched screen (the UI takes it and turns it into a texture).
    pub frame: Mutex<Option<ScreenFrame>>,
    /// The most recently measured audio levels.
    pub levels: Mutex<Levels>,
    /// Used to make the UI repaint when new data arrives.
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
/// One person in our voice channel, as the server describes them.
/// `Deserialize` because it comes from JSON; `Clone` so `sync` can keep a copy.
pub struct Participant {
    pub session_id: String,
    pub tracks: Vec<String>,
}

/// Shared app audio being sent (e.g. Spotify).
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

/// One active voice session.
/// The UI never holds a `Call` directly; it lives inside `voice_loop` in backend.rs.
pub struct Call {
    sfu: Sfu,
    factory: PeerConnectionFactory,
    pc: PeerConnection,
    pub session_id: String,
    mic: RtcAudioTrack,
    /// Our own mic pipeline (capture → RNNoise → gate). `None` = fallback to
    /// WebRTC's own capture. Kept here so it lives (and the mic stays open) for
    /// the whole call; dropping the `Call` drops it and closes the microphone.
    _mic_proc: Option<crate::micproc::MicPipeline>,
    screen: Option<LocalScreen>,
    music: Option<LocalMusic>,
    /// Was the current "music" track started automatically by screen sharing?
    /// (Then stopping the screen share stops it too.)
    music_from_screen: bool,
    /// "sessionId/trackName" -> mid of every track we currently pull.
    pulled: HashMap<String, String>,
    /// Last known participants and watched screen (to retry failed pulls later).
    last_sync: (Vec<Participant>, Option<String>),
    /// When we last re-pulled a stalled track (avoids re-pulling too often).
    repulled: HashMap<String, Instant>,
    /// mids of tracks we couldn't close on the SFU yet (muted; closing is retried).
    zombies: HashSet<String>,
    shared: Arc<Shared>,
    stats_task: JoinHandle<()>,
}

fn offer_options() -> OfferOptions {
    // The defaults (`Some(false)`) would disable the receive-only tracks we pull;
    // with `None` WebRTC leaves their directions alone.
    OfferOptions { ice_restart: false, offer_to_receive_audio: None, offer_to_receive_video: None }
}

/// Converts the SFU's JSON SDP into libwebrtc's `SessionDescription`.
fn parse(sdp: &Sdp) -> Result<SessionDescription> {
    let kind = match sdp.kind.as_str() {
        "offer" => SdpType::Offer,
        "answer" => SdpType::Answer,
        other => return Err(anyhow!("bilinmeyen SDP tipi: {other}")),
    };
    SessionDescription::parse(&sdp.sdp, kind).map_err(|e| anyhow!("{e}"))
}

impl Call {
    /// Joins voice: sets up the PeerConnection and the audio devices, creates an SFU
    /// session and publishes our microphone. `muted` is the initial mute state.
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
            // `Some(Box::new(closure))`: the callback is a boxed closure. `move` gives
            // it its own clone of `shared` (an Arc), so it stays valid after this function.
            pc.on_track(Some(Box::new(move |ev: TrackEvent| on_track(&shared, ev))));
        }

        // Keep Windows from lowering other apps' volume (before and after opening the devices).
        let _ = tokio::task::spawn_blocking(crate::ducking::disable).await;

        // Real microphone/speakers (the platform ADM, including echo cancellation).
        if !factory.acquire_platform_adm() {
            return Err(anyhow!("Ses aygıtı başlatılamadı"));
        }
        shared.muted.store(muted, Ordering::Relaxed);

        // Preferred path: capture the mic ourselves and clean it with RNNoise
        // (see micproc.rs). The processed audio goes into this source: 48 kHz,
        // mono, queue 0 = frames are handed to WebRTC as soon as we produce them.
        // WebRTC's own processing is off for it (RNNoise does the cleaning).
        let mic_source = NativeAudioSource::new(
            AudioSourceOptions { echo_cancellation: false, noise_suppression: false, auto_gain_control: false },
            crate::micproc::RATE,
            1,
            0,
        );
        // `start` waits for the device to open, so run it on the blocking pool.
        let mic_proc = {
            let (src, sh) = (mic_source.clone(), shared.clone());
            tokio::task::spawn_blocking(move || crate::micproc::MicPipeline::start(src, sh))
                .await
                .ok()
                .flatten()
        };
        // WebRTC records the microphone only in the fallback case; otherwise the
        // mic would be opened twice. Playout (hearing others) always goes through it.
        factory.set_adm_recording_enabled(mic_proc.is_none());
        factory.set_adm_playout_enabled(true);
        // Windows: play on the default (console) device instead of the default
        // communications device, so Windows doesn't lower other apps' volume
        // (see `ducking::default_render_id`).
        if let Ok(Some(id)) = tokio::task::spawn_blocking(crate::ducking::default_render_id).await {
            use_playout_device(&factory, &id);
        }
        let _ = tokio::task::spawn_blocking(crate::ducking::disable).await;
        log_devices(&factory);

        let session_id = sfu.new_session().await?;
        let mic = if mic_proc.is_some() {
            log::info!("mikrofon: RNNoise hattı kullanılıyor");
            factory.create_audio_track("mic", mic_source)
        } else {
            log::warn!("mikrofon: RNNoise hattı açılamadı, WebRTC yakalamasına dönülüyor");
            factory.create_device_audio_track("mic")
        };
        // `stats_loop` enables the track (immediately with our pipeline, which
        // gates by itself; in the fallback once the level crosses the threshold).
        mic.set_enabled(false);

        // Fallback only: measure the mic level separately (WebRTC doesn't measure
        // while the track is disabled). `spawn_blocking` runs the blocking
        // `Meter::start` on a thread meant for blocking work.
        let meter = if mic_proc.is_none() {
            let m = tokio::task::spawn_blocking(crate::meter::Meter::start).await.ok().flatten();
            if m.is_none() {
                log::warn!("mikrofon ölçer yok: giriş hassasiyeti devre dışı, mikrofon hep açık");
            }
            m
        } else {
            None
        };
        let proc_state = mic_proc.as_ref().map(|p| p.state.clone());
        let stats_task = spawn(stats_loop(pc.clone(), mic.clone(), meter, proc_state, shared.clone()));
        let call = Self {
            sfu,
            factory,
            pc,
            session_id,
            mic: mic.clone(),
            _mic_proc: mic_proc,
            screen: None,
            music: None,
            music_from_screen: false,
            pulled: HashMap::new(),
            last_sync: (Vec::new(), None),
            repulled: HashMap::new(),
            zombies: HashSet::new(),
            shared,
            stats_task,
        };
        // Publish our microphone to the SFU as track "mic".
        // Priority High: when the upload is tight (e.g. during screen sharing),
        // WebRTC gives the voice its bandwidth first and squeezes the video instead.
        let mic_enc = RtpEncodingParameters { priority: Priority::High, ..Default::default() };
        let tr = call.add_send_transceiver(MediaStreamTrack::Audio(mic), vec![mic_enc])?;
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

    /// Publishes a local transceiver to the SFU under the name `name` ("mic", "screen", "music").
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

    /// Answers an offer the SFU sent (renegotiation initiated by the SFU).
    async fn answer_offer(&self, offer: &Sdp) -> Result<()> {
        self.pc.set_remote_description(parse(offer)?).await.map_err(|e| anyhow!("{e}"))?;
        let answer = self.pc.create_answer(AnswerOptions::default()).await.map_err(|e| anyhow!("{e}"))?;
        self.pc.set_local_description(answer.clone()).await.map_err(|e| anyhow!("{e}"))?;
        self.sfu
            .renegotiate(&self.session_id, Sdp { kind: "answer".into(), sdp: answer.to_string() })
            .await
    }

    /// Pulls one remote track (`track` of session `remote_session`).
    /// `&mut self` because we record the new mid in `self.pulled`.
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
        // Record the owner before `on_track` fires. Music gets its own owner key
        // ("session#music"), so it doesn't light the speaking ring and its volume is separate.
        let owner = if track == "music" { music_owner(remote_session) } else { remote_session.to_string() };
        self.shared.mid_owner.lock().insert(mid.clone(), owner);
        if res.requires_immediate_renegotiation {
            if let Some(offer) = &res.session_description {
                if let Err(e) = self.answer_offer(offer).await {
                    // Half-finished pull: if it stays open on the SFU, the next attempt would make
                    // us hear this person twice. Mute and close it now; if closing fails it's retried later.
                    self.drop_mid(&mid).await;
                    return Err(e);
                }
            }
        }
        self.pulled.insert(format!("{remote_session}/{track}"), mid);
        Ok(())
    }

    /// Stops pulling the track with key "sessionId/trackName".
    async fn unpull(&mut self, key: &str) -> Result<()> {
        let Some(mid) = self.pulled.remove(key) else { return Ok(()) };
        self.drop_mid(&mid).await;
        Ok(())
    }

    /// Mutes a remote track, removes it from our local bookkeeping and closes it on the SFU.
    /// If closing fails it's added to `zombies` and retried during maintenance.
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

    /// Asks the SFU to close the track with this mid (and answers its renegotiation offer).
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

    /// Updates which tracks we pull so they match the voice channel's participants.
    /// `watch`: session id of the person whose screen we're watching (screens are only
    /// pulled while watched, to save bandwidth; mics and music are always pulled).
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

    /// Is the connection broken (do we need to rejoin)? "Disconnected" for > 8 s also counts.
    pub fn is_broken(&self) -> bool {
        match *self.shared.pc_state.lock() {
            Some((PeerConnectionState::Failed, _)) => true,
            Some((PeerConnectionState::Disconnected, t)) => t.elapsed() > Duration::from_secs(8),
            _ => false,
        }
    }

    /// Periodic maintenance: closes and re-pulls stalled audio tracks and retries
    /// tracks that couldn't be pulled before.
    pub async fn heal(&mut self) {
        // Retry closing tracks the SFU didn't close earlier.
        let zombies: Vec<String> = self.zombies.iter().cloned().collect();
        for mid in zombies {
            self.drop_mid(&mid).await;
        }
        // Audio tracks that belong to no current pull (orphaned duplicates): mute and close them.
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

    /// Mute/unmute our microphone.
    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
        if muted {
            self.mic.set_enabled(false);
        }
        // Unmuting happens in the gate loop, once the level is above the threshold.
    }

    /// Deafen = stop playing everyone else's audio.
    pub fn set_deafened(&self, deafened: bool) {
        self.shared.deafened.store(deafened, Ordering::Relaxed);
        // Only the tracked audio tracks: muted duplicates must not be re-enabled.
        for t in self.shared.audio_tracks.lock().values() {
            t.set_enabled(!deafened);
        }
    }

    /// Starts sharing a screen or window. `source_id`/`kind` come from
    /// `screen::list_sources`.
    pub async fn start_screen(&mut self, source_id: u64, kind: String) -> Result<()> {
        if self.screen.is_some() {
            self.stop_screen().await?;
        }
        let video = NativeVideoSource::new(VideoResolution { width: 1920, height: 1080 }, true);
        let capture = Capture::start(source_id, kind.clone(), video.clone())?;
        let track = self.factory.create_video_track("screen", video);
        let enc = RtpEncodingParameters {
            // 2.5 Mbps at most (was 3): many home connections upload only a few
            // Mbps, and a saturated upload made our voice break up while sharing.
            max_bitrate: Some(2_500_000),
            max_framerate: Some(screen::FPS as f64),
            // Lower priority than the voice (see the mic's `Priority::High`).
            priority: Priority::Low,
            ..Default::default()
        };
        let tr = self.add_send_transceiver(MediaStreamTrack::Video(track), vec![enc])?;
        let mid = self.push_track(&tr, "screen").await.context("ekran gönderilemedi")?;
        self.screen = Some(LocalScreen { _capture: capture, transceiver: tr, mid });

        // Share the sound too (Windows only), unless app audio is already being
        // shared by hand: a window -> that app's audio; a whole screen ->
        // everything except goodbyedc (so nobody hears their own voice back).
        // Failing here isn't fatal: the picture is already being shared.
        if crate::appaudio::supported() && self.music.is_none() {
            let target = if kind == "window" {
                screen::window_pid(source_id).map(crate::appaudio::Target::App)
            } else {
                Some(crate::appaudio::Target::AllButUs)
            };
            if let Some(target) = target {
                match self.start_music(target).await {
                    Ok(()) => self.music_from_screen = true,
                    Err(e) => log::warn!("ekran sesi paylaşılamadı: {e:#}"),
                }
            }
        }
        Ok(())
    }

    pub async fn stop_screen(&mut self) -> Result<()> {
        let Some(s) = self.screen.take() else { return Ok(()) };
        let LocalScreen { _capture, transceiver, mid } = s;
        drop(_capture);
        // Stop the sound that came with the screen share (not hand-started audio).
        if self.music_from_screen {
            let _ = self.stop_music().await;
        }
        self.stop_local(transceiver, mid).await
    }

    /// Publishes the audio of `target` (one app, or everything but us) as track "music".
    pub async fn start_music(&mut self, target: crate::appaudio::Target) -> Result<()> {
        use crate::appaudio::{CHANNELS, SAMPLE_RATE};
        if self.music.is_some() {
            self.stop_music().await?;
        }
        // Captured audio goes into a buffer; a timer sends it on in 10 ms slices.
        // When the app goes quiet (paused), silence is sent so the stream doesn't stall.
        let buf = Arc::new(Mutex::new(std::collections::VecDeque::<f32>::new()));
        let capture = {
            let buf = buf.clone();
            crate::appaudio::capture(target, move |d| {
                let mut b = buf.lock();
                b.extend(d.iter().copied());
                // At most 200 ms of delay: anything beyond that is dropped.
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
            let shared = self.shared.clone();
            spawn(async move {
                let mut norm = crate::normalize::Normalizer::default();
                let chunk = (SAMPLE_RATE / 100 * CHANNELS) as usize;
                let mut tick = tokio::time::interval(Duration::from_millis(10));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
                // Collect a little first (protects against jitter).
                let mut primed = false;
                loop {
                    tick.tick().await;
                    let mut data: Vec<f32> = {
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
                            vec![0.0; chunk]
                        }
                    };
                    if shared.music_normalize.load(Ordering::Relaxed) {
                        norm.process(&mut data);
                    }
                    let pcm: Vec<i16> = data.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
                    let frame = AudioFrame {
                        data: pcm.into(),
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
        self.music_from_screen = false;
        let Some(m) = self.music.take() else { return Ok(()) };
        let LocalMusic { _capture, pacer, transceiver, mid } = m;
        drop(_capture);
        pacer.abort();
        self.stop_local(transceiver, mid).await
    }

    /// Stops a local stream and closes it on the SFU.
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

    /// The track names we currently publish (sent to the server so others know
    /// what to pull). `&'static str` = string literals that live forever.
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

    /// Leaves the voice channel. Takes `self` by value (not `&self`), so the `Call`
    /// is consumed: after `leave()` the caller can't use it anymore — the compiler
    /// enforces that.
    pub fn leave(mut self) {
        self.stats_task.abort();
        self.screen = None; // dropping the Capture stops screen capturing
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

/// Called by WebRTC (on its own thread) whenever a remote track starts arriving.
/// Audio tracks are registered for volume control; video tracks get a viewer task.
fn on_track(shared: &Arc<Shared>, ev: TrackEvent) {
    let mid = ev.transceiver.mid().unwrap_or_default();
    match ev.track {
        MediaStreamTrack::Audio(t) => {
            // Audio is played automatically through the platform ADM.
            if shared.deafened.load(Ordering::Relaxed) {
                t.set_enabled(false);
            }
            // If audio from the same person already exists (a duplicate), mute it; maintenance closes it.
            let owners = shared.mid_owner.lock().clone();
            if let Some(owner) = owners.get(&mid) {
                for (m, old) in shared.audio_tracks.lock().iter() {
                    if m != &mid && owners.get(m) == Some(owner) {
                        log::warn!("{owner} için ikinci ses track'i geldi, eskisi ({m}) susturuldu");
                        old.set_enabled(false);
                    }
                }
            }
            // Volume is applied by the stats loop (so we don't block WebRTC's thread
            // here with a call that waits on another WebRTC thread).
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

/// Converts the remote screen's frames to RGBA and hands them to the UI.
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

/// Switches WebRTC's speaker output to the device with endpoint id `id`.
/// WebRTC refuses to change the device while playout is running, so if it
/// already runs it's stopped, switched and restarted.
fn use_playout_device(factory: &PeerConnectionFactory, id: &str) {
    let running = factory.playout_is_initialized();
    if running {
        factory.stop_playout();
    }
    let ok = factory.set_playout_device_by_guid(id);
    if running {
        factory.init_playout();
        factory.start_playout();
    }
    log::info!("hoparlör: varsayılan aygıt seçildi ({ok})");
}

/// Logs the audio devices WebRTC sees (helps debugging on friends' machines).
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

/// Runs every 50 ms:
/// - measures audio levels ("who is speaking") for the UI,
/// - opens/closes our microphone according to the input sensitivity,
/// - applies per-user volumes,
/// - detects stalled incoming audio.
/// Every few seconds it also logs how much audio was sent/received.
///
/// `proc_state` is set when our own mic pipeline (micproc.rs) runs: it does the
/// gating itself, so here we only follow mute and read its level/state. Otherwise
/// (fallback) the gate below switches WebRTC's track on/off using `meter`.
async fn stats_loop(
    pc: PeerConnection,
    mic: RtcAudioTrack,
    meter: Option<crate::meter::Meter>,
    proc_state: Option<Arc<crate::micproc::MicState>>,
    shared: Arc<Shared>,
) {
    use libwebrtc::stats::RtcStats;
    let mut tick: u32 = 0;
    let mut open_until: Option<Instant> = None;
    let mut mic_on = false;
    // Automatic mode: estimate of the background noise level (dB).
    let mut noise_db: f32 = -60.0;
    let mut applied: HashMap<String, f32> = HashMap::new();
    // mid -> (bytes received, when that number last changed)
    let mut inbound: HashMap<String, (u64, Instant)> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        tick += 1;
        // WebRTC statistics: a list of entries of different kinds (one enum variant
        // per kind). We pick out the ones we need with `match`.
        let Ok(stats) = pc.get_stats().await else { continue };

        let mut me = 0.0f64;
        let mut sent = 0u64;
        let mut rtt: Option<f64> = None;
        let mut peers = serde_json::Map::new();
        let mut recv = serde_json::Map::new();
        let owners = shared.mid_owner.lock().clone();
        // Bytes received for every pulled audio track (0 if it has no statistics yet).
        let mut bytes_now: HashMap<String, u64> =
            shared.audio_tracks.lock().keys().map(|m| (m.clone(), 0)).collect();
        for st in &stats {
            match st {
                RtcStats::MediaSource(m) if m.source.kind == "audio" => me = me.max(m.audio.audio_level),
                RtcStats::OutboundRtp(o) if o.stream.kind == "audio" => sent += o.sent.bytes_sent,
                // Round-trip time of the selected connection pair (measured from ICE
                // connectivity checks; no extra traffic).
                RtcStats::CandidatePair(c) if c.candidate_pair.nominated && c.candidate_pair.current_round_trip_time > 0.0 => {
                    rtt = Some(c.candidate_pair.current_round_trip_time * 1000.0);
                }
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
        // Stalled audio: if no new data arrived for 6 seconds, re-pull the track.
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

        // Input sensitivity (voice gate)
        // `(a, b, c) = if ... { ... } else { ... }` computes three values at once
        // (a tuple), depending on which mic path is active.
        let (level, transmitting, auto_threshold) = if let Some(ps) = &proc_state {
            // Our pipeline gates by itself (with pre-roll); the track only follows mute.
            let want = !shared.muted.load(Ordering::Relaxed);
            if want != mic_on {
                mic.set_enabled(want);
                mic_on = want;
            }
            (ps.take_level(), ps.open.load(Ordering::Relaxed), crate::micproc::AUTO_FLOOR_DB)
        } else {
            // The level comes from the separate meter; WebRTC's value is 0 while the track is off.
            let level = meter.as_ref().map(|m| m.take_level()).unwrap_or(me as f32);
            let db = level_db(level);
            if tick % 100 == 0 {
                log::info!("ses: ölçer seviyesi {level:.3} ({db:.0} dB)");
            }
            let gate = *shared.gate.lock();
            // Noise floor: follows quiet levels down quickly; rises only very slowly, and
            // even more slowly while we're talking. So long speech isn't mistaken for
            // "noise" that would push the threshold up.
            let speaking_now = open_until.is_some_and(|t| Instant::now() < t);
            if tick <= 20 {
                // First second: measure the room's noise directly (lowest level seen).
                noise_db = if tick == 1 { db } else { noise_db.min(db) };
            } else if db < noise_db {
                noise_db += (db - noise_db) * 0.3;
            } else {
                // Adapts in ~25 s while quiet, ~2 min while talking.
                noise_db += (db - noise_db) * if speaking_now { 0.0004 } else { 0.002 };
            }
            // Speech is almost always above -35 dB; never let the threshold go higher.
            let auto_threshold = (noise_db + 12.0).clamp(-60.0, -38.0);
            let threshold = if gate.auto { auto_threshold } else { gate.threshold_db };
            let now = Instant::now();
            if db >= threshold {
                open_until = Some(now + GATE_HOLD);
            }
            // Without a meter there's no gate: the mic is always on.
            let open = meter.is_none() || open_until.is_some_and(|t| now < t);
            let want = open && !shared.muted.load(Ordering::Relaxed);
            if want != mic_on {
                mic.set_enabled(want);
                mic_on = want;
            }
            (level, mic_on, auto_threshold)
        };

        // Per-user volume
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
            lv.transmitting = transmitting;
            lv.auto_threshold_db = auto_threshold;
            lv.rtt_ms = rtt.map(|r| r as f32).or(lv.rtt_ms);
            lv.peers = peers.iter().filter_map(|(k, v)| Some((k.clone(), v.as_f64()? as f32))).collect();
        }
        shared.request_repaint();
    }
}
