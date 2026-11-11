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

/// The health of *one* call, written by WebRTC's callbacks and the stats loop
/// and read by the voice loop.
///
/// Every `Call` gets its own `Health`. It used to live in `Shared`, which all
/// calls share: a callback or stats loop left over from an earlier call (for
/// example one whose join failed half-way) then kept reporting *its* dead
/// connection there, and the new, healthy call was torn down and rebuilt over
/// and over until the app was restarted.
#[derive(Default)]
struct Health {
    /// The WebRTC connection's state and when it entered that state.
    pc_state: Mutex<Option<(PeerConnectionState, Instant)>>,
    /// Set by the stats loop when audio stopped flowing although the connection
    /// still claims to be fine (the reason, for the log). The voice loop then
    /// rebuilds the whole call, exactly what leaving and rejoining by hand did.
    dead: Mutex<Option<&'static str>>,
    /// mids of audio tracks that haven't received data for a while (stalled).
    stalled: Mutex<HashSet<String>>,
    /// Set by the stats loop when audio arrives but the speaker stopped
    /// playing it (see the playout watchdog there); `heal` restarts the speaker.
    playout_stuck: AtomicBool,
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
    /// mids of tracks we couldn't close on the SFU yet (muted), with the number
    /// of attempts so far; closing is retried a few times during maintenance.
    zombies: HashMap<String, u32>,
    /// Pulled tracks whose owner vanished from the participant list, and when.
    /// They're only closed after `GONE_GRACE` (see `sync`).
    gone: HashMap<String, Instant>,
    /// Tracks we keep failing to pull: key -> (failures in a row, first failure).
    pull_fails: HashMap<String, (u32, Instant)>,
    shared: Arc<Shared>,
    health: Arc<Health>,
    stats_task: JoinHandle<()>,
    /// When this call was created (a connection that never comes up is "broken" too).
    joined_at: Instant,
    /// Endpoint id of the speaker we play on (Windows), to notice when the
    /// default device changes (headphones plugged in, Bluetooth connected...).
    playout_id: Option<String>,
    /// Tests only: the task feeding silence in place of a microphone.
    #[cfg(test)]
    silent_mic: JoinHandle<()>,
}

/// How long a pulled track is kept after its owner disappeared from the
/// participant list. The list comes over the WebSocket, the audio over WebRTC:
/// when someone's WebSocket reconnects (a short network hiccup, a server
/// restart) they drop out of the list for a moment although their audio never
/// stopped. Closing their track right away cut them off for everyone until the
/// list caught up again; now a short absence isn't audible at all.
const GONE_GRACE: Duration = Duration::from_secs(6);
/// How many times closing a track on the SFU is retried before giving up.
const ZOMBIE_TRIES: u32 = 5;

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
        // The SFU session comes first: it's the step that fails while the network
        // is down, and failing here costs nothing (no devices were opened yet).
        let session_id = sfu.new_session().await?;

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

        let health = Arc::new(Health::default());
        {
            let health = health.clone();
            pc.on_connection_state_change(Some(Box::new(move |s| {
                log::info!("bağlantı durumu: {s:?}");
                *health.pc_state.lock() = Some((s, Instant::now()));
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
            pc.close();
            return Err(anyhow!("Ses aygıtı başlatılamadı"));
        }
        // From here to the creation of the `Call` below nothing may return early:
        // only `Call`'s `Drop` gives the audio devices back.
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
        #[cfg(not(test))]
        let mic_proc = {
            let (src, sh) = (mic_source.clone(), shared.clone());
            tokio::task::spawn_blocking(move || crate::micproc::MicPipeline::start(src, sh))
                .await
                .ok()
                .flatten()
        };
        // Tests have no microphone permission: they send silence instead.
        #[cfg(test)]
        let (mic_proc, silent_mic) = (None::<crate::micproc::MicPipeline>, spawn(silence(mic_source.clone())));
        let use_source = cfg!(test) || mic_proc.is_some();
        // WebRTC records the microphone only in the fallback case; otherwise the
        // mic would be opened twice. Playout (hearing others) always goes through it.
        factory.set_adm_recording_enabled(!use_source);
        factory.set_adm_playout_enabled(true);
        // Windows: play on the default (console) device instead of the default
        // communications device, so Windows doesn't lower other apps' volume
        // (see `ducking::default_render_id`).
        let playout_id = tokio::task::spawn_blocking(crate::ducking::default_render_id).await.ok().flatten();
        if let Some(id) = &playout_id {
            use_playout_device(&factory, id);
        }
        let _ = tokio::task::spawn_blocking(crate::ducking::disable).await;
        log_devices(&factory);

        let mic = if use_source {
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
        let meter = if !use_source {
            let m = tokio::task::spawn_blocking(crate::meter::Meter::start).await.ok().flatten();
            if m.is_none() {
                log::warn!("mikrofon ölçer yok: giriş hassasiyeti devre dışı, mikrofon hep açık");
            }
            m
        } else {
            None
        };
        let proc_state = mic_proc.as_ref().map(|p| p.state.clone());
        let stats_task =
            spawn(stats_loop(pc.clone(), mic.clone(), meter, proc_state, shared.clone(), health.clone()));
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
            zombies: HashMap::new(),
            gone: HashMap::new(),
            pull_fails: HashMap::new(),
            shared,
            health,
            #[cfg(test)]
            silent_mic,
            stats_task,
            joined_at: Instant::now(),
            playout_id,
        };
        // Publish our microphone to the SFU as track "mic".
        // Priority High: when the upload is tight (e.g. during screen sharing),
        // WebRTC gives the voice its bandwidth first and squeezes the video instead.
        let mic_enc = RtpEncodingParameters { priority: Priority::High, ..Default::default() };
        let tr = call.add_send_transceiver(MediaStreamTrack::Audio(mic), vec![mic_enc])?;
        call.push_track(&tr, "mic").await.context("mikrofon gönderilemedi")?;
        // Wait until the connection is really up before reporting success. The
        // caller announces us to the channel right after this, and everyone then
        // pulls our microphone at once; if our audio isn't flowing yet, the SFU
        // keeps each of them waiting ~8 s and then refuses ("no track data").
        let started = Instant::now();
        loop {
            match *call.health.pc_state.lock() {
                Some((PeerConnectionState::Connected, _)) => break,
                Some((PeerConnectionState::Failed | PeerConnectionState::Closed, _)) => {
                    return Err(anyhow!("bağlantı kurulamadı"));
                }
                _ if started.elapsed() > Duration::from_secs(15) => {
                    return Err(anyhow!("bağlantı 15 sn içinde kurulamadı"));
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
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
    /// If it fails, the half-made offer is undone and the transceiver stopped,
    /// so the connection is left exactly as it was.
    async fn push_track(&self, tr: &RtpTransceiver, name: &str) -> Result<String> {
        let res = self.push_track_inner(tr, name).await;
        if res.is_err() {
            self.rollback().await;
            let _ = tr.stop();
        }
        res
    }

    async fn push_track_inner(&self, tr: &RtpTransceiver, name: &str) -> Result<String> {
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
        if let Some(e) = res.track_error() {
            return Err(anyhow!(e));
        }
        let answer = res.session_description.ok_or_else(|| anyhow!("SFU answer göndermedi"))?;
        self.pc.set_remote_description(parse(&answer)?).await.map_err(|e| anyhow!("{e}"))?;
        Ok(mid)
    }

    /// Takes back an offer of ours that never got its answer (the request to the
    /// SFU failed or timed out). Without this the connection stays in the
    /// "waiting for an answer" state, in which it refuses every offer from the
    /// SFU: from then on nobody new could be pulled (heard) until rejoining.
    async fn rollback(&self) {
        if self.pc.signaling_state() != SignalingState::HaveLocalOffer {
            return;
        }
        let undone = match SessionDescription::parse("", SdpType::Rollback) {
            Ok(rb) => self.pc.set_local_description(rb).await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = undone {
            // Can't be repaired in place: let the voice loop rebuild the call.
            log::warn!("yarım kalan teklif geri alınamadı: {e}");
            *self.health.dead.lock() = Some("sinyalleşme takıldı");
        }
    }

    /// Answers an offer the SFU sent (renegotiation initiated by the SFU).
    async fn answer_offer(&self, offer: &Sdp) -> Result<()> {
        self.rollback().await;
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
        // Answer the SFU's offer first, even if the track itself failed: an
        // unanswered offer blocks the session for every later request.
        // (A failed track comes back with an empty mid.)
        let mid = res.tracks.first().and_then(|t| t.mid.clone()).filter(|m| !m.is_empty());
        // Record the owner before `on_track` fires. Music gets its own owner key
        // ("session#music"), so it doesn't light the speaking ring and its volume is separate.
        if let Some(mid) = &mid {
            let owner = if track == "music" { music_owner(remote_session) } else { remote_session.to_string() };
            self.shared.mid_owner.lock().insert(mid.clone(), owner);
        }
        let mut result = Ok(());
        if res.requires_immediate_renegotiation {
            if let Some(offer) = &res.session_description {
                result = self.answer_offer(offer).await;
            }
        }
        if let (Ok(()), Some(e)) = (&result, res.track_error()) {
            result = Err(anyhow!(e));
        }
        let mid = match (result, mid) {
            (Ok(()), Some(mid)) => mid,
            (Ok(()), None) => return Err(anyhow!("SFU track mid döndürmedi")),
            (Err(e), mid) => {
                // Half-finished pull: if it stays open on the SFU, the next attempt would make
                // us hear this person twice. Mute and close it now; if closing fails it's retried later.
                if let Some(mid) = mid {
                    self.drop_mid(&mid).await;
                }
                return Err(e);
            }
        };
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
            let tries = self.zombies.entry(mid.to_string()).or_insert(0);
            *tries += 1;
            if *tries >= ZOMBIE_TRIES {
                log::warn!("track kapatılamadı ({mid}), vazgeçildi: {e:#}");
                self.zombies.remove(mid);
            } else {
                log::warn!("track kapatılamadı ({mid}), sonra yeniden denenecek: {e:#}");
            }
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
        // A per-track error means the SFU doesn't have (or already closed) this
        // track: nothing is left to close, so retrying would never succeed.
        if let Some(e) = res.track_error() {
            log::debug!("track zaten kapalı ({mid}): {e}");
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

        // Tracks we pull but no longer want. If the owner is still in the channel
        // (they stopped sharing, or we stopped watching) the track is closed right
        // away. If the owner vanished from the list, it's kept for `GONE_GRACE`
        // first: they may just be reconnecting their WebSocket (see `GONE_GRACE`).
        // `heal` calls `sync` every few seconds, which is what ends the grace.
        let present: HashSet<&str> = participants.iter().map(|p| p.session_id.as_str()).collect();
        let stale: Vec<String> = self.pulled.keys().filter(|k| !desired_keys.contains(*k)).cloned().collect();
        self.gone.retain(|k, _| stale.contains(k));
        for key in stale {
            let owner = key.split('/').next().unwrap_or_default();
            if !present.contains(owner) {
                let since = *self.gone.entry(key.clone()).or_insert_with(Instant::now);
                if since.elapsed() < GONE_GRACE {
                    continue;
                }
            }
            self.gone.remove(&key);
            if let Err(e) = self.unpull(&key).await {
                log::warn!("track kapatılamadı {key}: {e:#}");
            }
        }
        self.pull_fails.retain(|k, _| desired_keys.contains(k));
        for (sid, track) in desired {
            let key = format!("{sid}/{track}");
            if self.pulled.contains_key(&key) {
                continue;
            }
            match self.pull(&sid, &track).await {
                Ok(()) => {
                    self.pull_fails.remove(&key);
                }
                Err(e) => {
                    log::warn!("track çekilemedi {key}: {e:#}");
                    self.pull_fails.entry(key).or_insert((0, Instant::now())).0 += 1;
                }
            }
        }
    }

    /// Tracks of people in the channel that we've been failing to pull for a
    /// while (at least 5 attempts over 20 s). Usually that means *our* session
    /// is stuck, and only a rebuilt call hears them again; the voice loop
    /// decides (it rebuilds at most once per track, in case the fault is theirs).
    pub fn failing_pulls(&self) -> Vec<String> {
        self.pull_fails
            .iter()
            .filter(|(_, (n, since))| *n >= 5 && since.elapsed() > Duration::from_secs(20))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Is the call broken, so it has to be rebuilt? Returns the reason (for the
    /// log), or `None` if everything is fine. Broken means any of:
    /// - WebRTC says the connection failed, or it has been "disconnected" for > 8 s;
    /// - the connection never came up (still new/connecting 20 s after joining);
    /// - the stats loop saw our audio stop flowing (see `Shared::dead`).
    pub fn broken_reason(&self) -> Option<&'static str> {
        if let Some(reason) = *self.health.dead.lock() {
            return Some(reason);
        }
        let slow = Duration::from_secs(20);
        match *self.health.pc_state.lock() {
            Some((PeerConnectionState::Failed, _)) => Some("bağlantı başarısız"),
            Some((PeerConnectionState::Closed, _)) => Some("bağlantı kapandı"),
            Some((PeerConnectionState::Disconnected, t)) if t.elapsed() > Duration::from_secs(8) => {
                Some("bağlantı 8 sn'dir kopuk")
            }
            Some((PeerConnectionState::New | PeerConnectionState::Connecting, t)) if t.elapsed() > slow => {
                Some("bağlantı kurulamadı")
            }
            None if self.joined_at.elapsed() > slow => Some("bağlantı kurulamadı"),
            _ => None,
        }
    }

    /// Periodic maintenance: closes and re-pulls stalled audio tracks and retries
    /// tracks that couldn't be pulled before.
    pub async fn heal(&mut self) {
        // Windows: if the default speaker changed (headphones plugged in or out,
        // Bluetooth connected...), move our playback there. Without this the
        // others' voices kept going to the old (maybe gone) device until rejoining.
        if let Ok(Some(id)) = tokio::task::spawn_blocking(crate::ducking::default_render_id).await {
            if self.playout_id.as_deref() != Some(id.as_str()) {
                log::info!("varsayılan hoparlör değişti, oraya geçiliyor");
                use_playout_device(&self.factory, &id);
                self.playout_id = Some(id);
            }
        }
        // Audio arrives but the speaker stopped playing it (the output device
        // hiccuped, the audio service restarted, the computer woke from sleep...):
        // restart playback. Before, this silence lasted until rejoining.
        if self.health.playout_stuck.swap(false, Ordering::Relaxed) {
            log::warn!("ses geliyor ama hoparlör çalmıyor, çalma yeniden başlatılıyor");
            self.factory.stop_playout();
            if let Some(id) = &self.playout_id {
                self.factory.set_playout_device_by_guid(id);
            }
            self.factory.init_playout();
            self.factory.start_playout();
        }
        // Retry closing tracks the SFU didn't close earlier.
        let zombies: Vec<String> = self.zombies.keys().cloned().collect();
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
        let stalled: Vec<String> = self.health.stalled.lock().drain().collect();
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
        let res = self.close_local(&mid).await;
        if res.is_err() {
            // Don't leave our offer hanging (see `rollback`), and let maintenance
            // retry closing the track on the SFU.
            self.rollback().await;
            self.zombies.entry(mid).or_insert(0);
        }
        res
    }

    async fn close_local(&self, mid: &str) -> Result<()> {
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
        match &res.session_description {
            Some(sd) if sd.kind == "answer" => {
                self.pc.set_remote_description(parse(sd)?).await.map_err(|e| anyhow!("{e}"))?;
            }
            _ => return Err(anyhow!("SFU answer göndermedi")),
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
    /// enforces that. The actual cleanup is in `Drop` below.
    pub fn leave(self) {}
}

/// Runs whenever a `Call` goes away, however that happens: `leave()`, a join
/// that failed half-way, the app closing. Doing the cleanup here (it used to be
/// in `leave`) guarantees it's never skipped. A failed join used to leave its
/// stats loop running and its audio devices held forever.
impl Drop for Call {
    fn drop(&mut self) {
        self.stats_task.abort();
        #[cfg(test)]
        self.silent_mic.abort();
        self.screen = None; // dropping the Capture stops screen capturing
        if let Some(m) = self.music.take() {
            m.pacer.abort();
        }
        // No more callbacks from this connection into the shared state.
        self.pc.on_track(None);
        self.pc.on_connection_state_change(None);
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

/// Tests only: stands in for the microphone by sending silence.
#[cfg(test)]
async fn silence(source: NativeAudioSource) {
    let mut tick = tokio::time::interval(Duration::from_millis(10));
    loop {
        tick.tick().await;
        let frame = AudioFrame {
            data: vec![0i16; 480].into(),
            sample_rate: crate::micproc::RATE,
            num_channels: 1,
            samples_per_channel: 480,
        };
        let _ = source.capture_frame(&frame).await;
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
    health: Arc<Health>,
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
    // Outgoing audio watchdog: bytes sent so far and when that number last grew.
    let mut out_bytes: (u64, Instant) = (0, Instant::now());
    // Incoming audio watchdog: bytes per track at the previous tick, and when
    // any track last received new data.
    let mut prev_in: HashMap<String, u64> = HashMap::new();
    let mut any_inbound = Instant::now();
    // Speaker watchdog: when any track last received data, and the number of
    // samples handed to the speaker so far with the time it last grew.
    let mut last_receiving: Option<Instant> = None;
    let mut played: (u64, Instant) = (0, Instant::now());
    loop {
        tokio::time::sleep(Duration::from_millis(50)).await;
        tick += 1;
        // WebRTC statistics: a list of entries of different kinds (one enum variant
        // per kind). We pick out the ones we need with `match`.
        let Ok(stats) = pc.get_stats().await else { continue };

        let mut me = 0.0f64;
        let mut sent = 0u64;
        let mut emitted = 0u64;
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
                    emitted += i.inbound.jitter_buffer_emitted_count;
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
                    health.stalled.lock().insert(mid.clone());
                    e.1 = now;
                }
            }
        }
        // Whole-call watchdogs. Both only run while the connection reports
        // "connected": otherwise the connection state itself triggers the rebuild.
        // They catch the case where WebRTC thinks everything is fine but audio no
        // longer flows, which before needed leaving and rejoining by hand.
        {
            let now = Instant::now();
            let connected = matches!(*health.pc_state.lock(), Some((PeerConnectionState::Connected, _)));
            // 1) We send nothing. While our mic track is on, WebRTC sends a packet
            //    every 20 ms even in silence, so a byte count that stops growing
            //    means our outgoing audio is stuck and nobody hears us.
            if sent != out_bytes.0 || !mic_on || !connected {
                out_bytes = (sent, now);
            } else if now.duration_since(out_bytes.1) > Duration::from_secs(8) {
                *health.dead.lock() = Some("giden ses 8 sn'dir akmıyor");
            }
            // 2) We receive nothing from anyone. With 2+ people sending to us, all
            //    of them stalling at once points at our own connection, so the whole
            //    call is rebuilt (one stalled person is handled by re-pulling just
            //    their track, see `heal`).
            // "Receiving" = some track's byte count grew since the last tick
            // (tracks that just appeared or disappeared don't count).
            let receiving = bytes_now.iter().any(|(mid, b)| prev_in.get(mid).is_some_and(|p| b > p));
            prev_in = bytes_now.clone();
            if receiving || bytes_now.len() < 2 || !connected {
                any_inbound = now;
            } else if now.duration_since(any_inbound) > Duration::from_secs(12) {
                *health.dead.lock() = Some("12 sn'dir kimseden ses gelmiyor");
            }
            // 3) The speaker died. Received audio waits in a buffer until the
            //    speaker's thread asks for it; `emitted` counts the samples it
            //    took. Audio keeps arriving but that count stands still = nothing
            //    is being played, although every other number looks healthy.
            //    `heal` then restarts playback.
            if receiving {
                last_receiving = Some(now);
            }
            let arriving = last_receiving.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1));
            if emitted != played.0 || !arriving {
                played = (emitted, now);
            } else if now.duration_since(played.1) > Duration::from_secs(5) {
                health.playout_stuck.store(true, Ordering::Relaxed);
                played.1 = now + Duration::from_secs(5); // give the restart time
            }
        }
        if tick % 100 == 0 {
            log::info!("ses: mikrofon seviyesi {me:.3}, açık {mic_on}, gönderilen {sent} B, alınan {recv:?}, çalınan {emitted} örnek, seviyeler {peers:?}");
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

/// Live tests against the real server (need a saved login and a microphone).
/// Run with: `cargo test live_ -- --ignored --nocapture`
#[cfg(test)]
mod live_tests {
    use super::*;

    /// One runtime for all tests: `set_runtime` keeps the first handle it gets,
    /// so background tasks would otherwise land on an earlier test's dead runtime.
    fn setup() -> Option<(&'static tokio::runtime::Runtime, Sfu)> {
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let _ = env_logger::Builder::new().parse_filters("goodbyedc=info").is_test(false).try_init();
        let token = crate::config::Settings::load().token?;
        let rt = RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap());
        set_runtime(rt.handle().clone());
        Some((rt, Sfu::new(&crate::config::server_url(), &token)))
    }

    /// Joining and leaving twice in a row must be quick and never hang.
    #[test]
    #[ignore]
    fn live_join_leave_timing() {
        let Some((rt, sfu)) = setup() else { return };
        rt.block_on(async {
            let factory = PeerConnectionFactory::default();
            for round in 0..2 {
                let t = Instant::now();
                let call = Call::join(factory.clone(), sfu.clone(), Arc::new(Shared::default()), false).await.unwrap();
                println!("tur {round}: katılma {:?}", t.elapsed());
                tokio::time::sleep(Duration::from_secs(4)).await;
                assert_eq!(call.broken_reason(), None);
                assert!(call.shared.levels.lock().rtt_ms.is_some(), "istatistik döngüsü çalışmıyor");
                let t = Instant::now();
                call.leave();
                println!("tur {round}: ayrılma {:?}", t.elapsed());
                assert!(t.elapsed() < Duration::from_secs(3));
            }
            assert_eq!(factory.platform_adm_ref_count(), 0);
        });
    }

    /// How much we send while unmuted and silent vs. muted.
    #[test]
    #[ignore]
    fn live_muted_bitrate() {
        let Some((rt, sfu)) = setup() else { return };
        rt.block_on(async {
            let factory = PeerConnectionFactory::default();
            let call = Call::join(factory.clone(), sfu, Arc::new(Shared::default()), false).await.unwrap();
            async fn sent(pc: &PeerConnection) -> (u64, u64) {
                let mut out = (0, 0);
                for st in pc.get_stats().await.unwrap() {
                    if let libwebrtc::stats::RtcStats::OutboundRtp(o) = st {
                        out = (o.sent.bytes_sent, o.sent.packets_sent);
                    }
                }
                out
            }
            for muted in [false, true, false, true] {
                call.set_muted(muted);
                tokio::time::sleep(Duration::from_secs(1)).await;
                let a = sent(&call.pc).await;
                tokio::time::sleep(Duration::from_secs(4)).await;
                let b = sent(&call.pc).await;
                println!("sessizde={muted}: {} kbps, {} paket/sn", (b.0 - a.0) * 8 / 4000, (b.1 - a.1) / 4);
                assert!((b.1 - a.1) / 4 >= 40, "ses paketleri akmıyor");
            }
        });
    }

    /// A join that fails half-way must give everything back, and must not be
    /// able to mark a later call as dead.
    #[test]
    #[ignore]
    fn live_failed_join_cleans_up() {
        let Some((rt, sfu)) = setup() else { return };
        rt.block_on(async {
            let factory = PeerConnectionFactory::default();
            let bad = Sfu::new("http://127.0.0.1:9", "x");
            assert!(Call::join(factory.clone(), bad, Arc::new(Shared::default()), false).await.is_err());
            assert_eq!(factory.platform_adm_ref_count(), 0);
            let call = Call::join(factory.clone(), sfu, Arc::new(Shared::default()), false).await.unwrap();
            tokio::time::sleep(Duration::from_secs(12)).await;
            assert_eq!(call.broken_reason(), None);
        });
    }

    /// Two calls: B hears A. Checks pulling, the speaker counter, the grace
    /// period for vanished participants and recovery from a failed negotiation.
    #[test]
    #[ignore]
    fn live_pull_grace_and_rollback() {
        let Some((rt, sfu)) = setup() else { return };
        rt.block_on(async {
            let factory = PeerConnectionFactory::default();
            let a = Call::join(factory.clone(), sfu.clone(), Arc::new(Shared::default()), false).await.unwrap();
            let mut b = Call::join(factory.clone(), sfu.clone(), Arc::new(Shared::default()), false).await.unwrap();
            let pa = Participant { session_id: a.session_id.clone(), tracks: vec!["mic".into()] };
            let key = format!("{}/mic", a.session_id);

            // A request to the SFU fails in the middle of a negotiation...
            let good = std::mem::replace(&mut b.sfu, Sfu::new("http://127.0.0.1:9", "x"));
            let src = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 0);
            let tr = b
                .add_send_transceiver(MediaStreamTrack::Audio(factory.create_audio_track("x", src)), vec![])
                .unwrap();
            assert!(b.push_track(&tr, "x").await.is_err());
            assert_eq!(b.pc.signaling_state(), SignalingState::Stable);
            b.sfu = good;

            // ...and pulling still works afterwards.
            b.sync(&[pa.clone()], None).await;
            assert!(b.pulled.contains_key(&key), "A çekilemedi");
            tokio::time::sleep(Duration::from_secs(8)).await;
            let stats = b.pc.get_stats().await.unwrap();
            let (mut bytes, mut emitted) = (0, 0);
            for st in &stats {
                if let libwebrtc::stats::RtcStats::InboundRtp(i) = st {
                    bytes += i.inbound.bytes_received;
                    emitted += i.inbound.jitter_buffer_emitted_count;
                }
            }
            println!("alınan {bytes} B, çalınan {emitted} örnek");
            assert!(bytes > 0 && emitted > 48_000, "ses akmıyor ya da çalınmıyor");
            assert!(!b.health.playout_stuck.load(Ordering::Relaxed));
            assert!(b.health.stalled.lock().is_empty());

            // A muted person still sends (silent) packets, so they can be pulled.
            let c = Call::join(factory.clone(), sfu.clone(), Arc::new(Shared::default()), true).await.unwrap();
            let pc_ = Participant { session_id: c.session_id.clone(), tracks: vec!["mic".into()] };
            let t = Instant::now();
            b.sync(&[pa.clone(), pc_], None).await;
            println!("sessizdeki kişiyi çekme {:?}", t.elapsed());
            assert!(b.pulled.contains_key(&format!("{}/mic", c.session_id)), "sessizdeki kişi çekilemedi");
            tokio::time::sleep(Duration::from_secs(7)).await;
            assert!(b.health.stalled.lock().is_empty(), "sessizdeki kişi 'takıldı' sayıldı");
            drop(c);

            // A drops out of the list for a moment: still pulled.
            b.sync(&[], None).await;
            assert!(b.pulled.contains_key(&key));
            b.sync(&[pa.clone()], None).await;
            assert!(!b.gone.contains_key(&key));
            // A is gone for good: closed after the grace period.
            b.sync(&[], None).await;
            tokio::time::sleep(GONE_GRACE + Duration::from_millis(200)).await;
            b.heal().await;
            assert!(!b.pulled.contains_key(&key));
            assert!(b.zombies.is_empty());
            assert_eq!(a.broken_reason(), None);
            assert_eq!(b.broken_reason(), None);
        });
    }
}
