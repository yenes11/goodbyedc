//! Our own microphone pipeline: capture → noise removal → voice gate → WebRTC.
//!
//! Before this module, WebRTC captured the microphone itself and we could only
//! switch the whole track on/off. That had two problems:
//!   1. The gate opened only *after* the level crossed the threshold, so the first
//!      syllable of every sentence was already lost ("start of speech cut off").
//!   2. WebRTC's built-in noise suppression only handles steady noise (fans, hum);
//!      keyboard clicks, a TV or other voices went straight through.
//!
//! Now we capture the microphone ourselves (with `cpal`) and process it in steps:
//!
//!   cpal (any sample rate, any channel count)
//!     → mono, resampled to 48 kHz
//!     → cut into 10 ms frames (480 samples)
//!     → RNNoise (`nnnoiseless`): a small neural network that removes background
//!       noise and also tells us how likely the frame is human speech (VAD =
//!       "voice activity detection", 0..1)
//!     → 30 ms delay line ("pre-roll") + gate
//!     → `NativeAudioSource` → WebRTC (Opus) → SFU
//!
//! Why the 30 ms delay helps: frames wait in a short queue before they're sent.
//! When speech is detected in the newest frame, the gate opens and the 3 frames
//! still waiting in the queue (the 30 ms *before* detection) are sent too, so the
//! beginning of the word survives. The cost is 30 ms of extra latency (plus
//! RNNoise's own ~10 ms), which isn't audible in a conversation.
//!
//! Trade-off: this path has no echo cancellation (WebRTC only applies it to audio
//! it captures itself). With headphones that doesn't matter.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use libwebrtc::audio_frame::AudioFrame;
use libwebrtc::audio_source::native::NativeAudioSource;
use nnnoiseless::DenoiseState;
use tokio::sync::mpsc;

use crate::voice::{level_db, Shared};

/// Sample rate of everything after the resampler (RNNoise requires 48 kHz).
pub const RATE: u32 = 48_000;
/// One 10 ms frame at 48 kHz (also RNNoise's fixed frame size).
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// Pre-roll: how many 10 ms frames wait in the delay line (3 = 30 ms).
const PREROLL_FRAMES: usize = 3;
/// After the last speech frame, keep the gate open this many frames (50 = 500 ms)
/// so word endings and short pauses aren't cut.
const HOLD_FRAMES: u32 = 50;
/// Automatic mode: RNNoise's speech probability needed to open the gate.
const VAD_OPEN: f32 = 0.6;
/// Automatic mode: the cleaned frame must also be at least this loud, so very
/// quiet murmurs far from the mic don't open the gate.
pub const AUTO_FLOOR_DB: f32 = -55.0;
/// Gain change per sample when the gate opens/closes. 1/240 = a 5 ms fade, which
/// avoids audible clicks from cutting the waveform abruptly.
const RAMP_STEP: f32 = 1.0 / 240.0;

/// Values the pipeline publishes for the UI / stats loop.
#[derive(Default)]
pub struct MicState {
    /// Highest cleaned level (0..1) since the last `take_level`, as f32 bits
    /// (there's no `AtomicF32`; see meter.rs for the same trick).
    peak: AtomicU32,
    /// Is the gate open (are we sending speech) right now?
    pub open: AtomicBool,
}

impl MicState {
    /// The highest level since the previous call; reading resets it to 0.
    pub fn take_level(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    fn record(&self, p: f32) {
        let _ = self.peak.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            (p > f32::from_bits(old)).then_some(p.to_bits())
        });
    }
}

/// A running pipeline. Dropping it closes the microphone and stops processing.
pub struct MicPipeline {
    pub state: Arc<MicState>,
    /// Tells the cpal thread to close the stream.
    stop: Arc<AtomicBool>,
    /// The async task doing the processing.
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MicPipeline {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

/// Messages from the microphone thread to the processing task.
enum Msg {
    /// The device was (re)opened with this sample rate.
    Rate(u32),
    /// A chunk of mono samples at the current device rate.
    Data(Vec<f32>),
}

/// If the microphone delivers nothing for this long, the stream is considered
/// dead (unplugged device, Bluetooth headset switching mode, Windows audio
/// service restart...) and is reopened.
const DEAD_AFTER: Duration = Duration::from_secs(2);

impl MicPipeline {
    /// Opens the default microphone and starts feeding processed audio into
    /// `source` (a 48 kHz mono `NativeAudioSource`). Returns `None` if the
    /// microphone can't be opened; the caller then falls back to WebRTC's own capture.
    ///
    /// Must be called from inside the tokio runtime (it spawns a task).
    pub fn start(source: NativeAudioSource, shared: Arc<Shared>) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        // The cpal callback sends raw mono chunks through this channel. An
        // *unbounded* channel never blocks the sender, which matters because the
        // callback runs on a real-time audio thread.
        let (tx, rx) = mpsc::unbounded_channel::<Msg>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
        {
            let stop = stop.clone();
            // cpal streams may not be movable between threads on every platform,
            // so the stream is opened, kept and closed on one dedicated thread.
            std::thread::Builder::new()
                .name("mikrofon".into())
                .spawn(move || mic_thread(tx, ready_tx, stop))
                .ok()?;
        }
        // Wait (max 5 s) for the first successful open.
        if ready_rx.recv_timeout(Duration::from_secs(5)) != Ok(true) {
            stop.store(true, Ordering::Relaxed);
            return None;
        }
        let state = Arc::new(MicState::default());
        let task = tokio::spawn(process(rx, source, shared, state.clone()));
        Some(Self { state, stop, task })
    }
}

/// Body of the microphone thread: opens the stream and keeps it alive, and
/// reopens it whenever it dies (error reported by cpal, or no data for
/// `DEAD_AFTER`). Before this watchdog, a stream that silently stopped meant the
/// others stopped hearing us until we left and rejoined the channel.
fn mic_thread(tx: mpsc::UnboundedSender<Msg>, ready_tx: std::sync::mpsc::Sender<bool>, stop: Arc<AtomicBool>) {
    let mut first = true;
    while !stop.load(Ordering::Relaxed) {
        // `alive` counts callbacks (the watchdog checks that it keeps moving);
        // `failed` is set by cpal's error callback.
        let alive = Arc::new(AtomicU32::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        match open(tx.clone(), alive.clone(), failed.clone()) {
            Ok((stream, rate, name)) => {
                let _ = tx.send(Msg::Rate(rate));
                if first {
                    let _ = ready_tx.send(true);
                    first = false;
                }
                let mut last_count = 0;
                let mut last_change = std::time::Instant::now();
                let mut device_checked = std::time::Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                    // Follow the system's default microphone: when a headset is
                    // plugged in (or Bluetooth connects) the default changes, but
                    // the open stream keeps recording from the old device. Before,
                    // the new microphone was only used after rejoining.
                    if device_checked.elapsed() > Duration::from_secs(2) {
                        device_checked = std::time::Instant::now();
                        let now = default_input_name();
                        if now.is_some() && now != name {
                            log::info!("varsayılan mikrofon değişti ({now:?}), oraya geçiliyor");
                            break;
                        }
                    }
                    let count = alive.load(Ordering::Relaxed);
                    if count != last_count {
                        last_count = count;
                        last_change = std::time::Instant::now();
                    }
                    if failed.load(Ordering::Relaxed) {
                        log::warn!("mikrofon akışı hata verdi, yeniden açılıyor");
                        break;
                    }
                    if last_change.elapsed() > DEAD_AFTER {
                        log::warn!("mikrofondan {DEAD_AFTER:?} veri gelmedi, yeniden açılıyor");
                        break;
                    }
                }
                drop(stream);
            }
            Err(e) => {
                log::warn!("mikrofon açılamadı: {e:#}");
                if first {
                    // The very first open failed: report it so the call falls
                    // back to WebRTC's own capture.
                    let _ = ready_tx.send(false);
                    return;
                }
            }
        }
        // Short pause before retrying, so a missing device doesn't spin the CPU.
        if !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    log::info!("mikrofon kapandı");
}

/// The processing loop: runs for as long as the call lasts.
async fn process(
    mut rx: mpsc::UnboundedReceiver<Msg>,
    source: NativeAudioSource,
    shared: Arc<Shared>,
    state: Arc<MicState>,
) {
    // Replaced as soon as the first `Msg::Rate` arrives.
    let mut resampler = Resampler::new(RATE);
    // 48 kHz samples waiting to fill a whole 10 ms frame.
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME * 4);
    // RNNoise keeps internal state between frames (it "listens" over time), so one
    // instance is used for the whole call. `Box` = it lives on the heap (it's big).
    let mut denoise = DenoiseState::new();
    // The pre-roll delay line: cleaned frames waiting to be sent.
    let mut delay: VecDeque<Vec<f32>> = VecDeque::with_capacity(PREROLL_FRAMES + 1);
    // Frames left until the gate closes (counts down after the last speech frame).
    let mut hold_left: u32 = 0;
    // Current gate gain 0..1, moved gradually towards 0 or 1 (the fade).
    let mut gain: f32 = 0.0;
    let mut cleaned = vec![0.0f32; FRAME];
    // When the last microphone data arrived (to tell "mic is dead" from "late").
    let mut last_data = std::time::Instant::now();
    // Guard against a device that delivers audio faster than real time (seen
    // with a misbehaving driver: 17 times too much). Everything we're given is
    // sent, so the others would receive hundreds of packets per second from us,
    // which breaks their playback of our voice. `budget` is how many 48 kHz
    // samples we may still accept: it refills at 1.1x real time, holds at most
    // a quarter second, and chunks that don't fit are dropped.
    let mut budget: f64 = RATE as f64 / 4.0;
    let mut budget_at = std::time::Instant::now();
    let mut flood_logged = false;

    loop {
        // Wait for the next message, but at most 50 ms. `timeout` returns
        // `Err(_)` when the time is up. If the mic has been silent for a while
        // (it's being reopened), we send silence so our outgoing audio never
        // stops completely (a stream with no packets looks "stalled" to the
        // others). A merely *late* chunk (busy computer, e.g. while screen
        // sharing) must not get silence inserted: that was audible as dropouts.
        match tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            // Sender gone: the microphone thread ended (call is closing).
            Ok(None) => break,
            Ok(Some(Msg::Rate(r))) => {
                log::info!("mikrofon hızı: {r} Hz");
                resampler = Resampler::new(r);
                continue;
            }
            Ok(Some(Msg::Data(chunk))) => {
                last_data = std::time::Instant::now();
                let now = std::time::Instant::now();
                budget = (budget + now.duration_since(budget_at).as_secs_f64() * RATE as f64 * 1.1)
                    .min(RATE as f64 / 4.0);
                budget_at = now;
                let before = pending.len();
                resampler.push(&chunk, &mut pending);
                let added = (pending.len() - before) as f64;
                if added > budget {
                    // Doesn't fit: drop this chunk and keep saving up for the next.
                    if !flood_logged {
                        log::warn!("mikrofon gerçek zamandan hızlı veri veriyor, fazlası atılıyor");
                        flood_logged = true;
                    }
                    pending.truncate(before);
                    continue;
                }
                budget -= added;
                // If we ever fall behind, don't let the delay grow: keep at most
                // 60 ms of unprocessed audio and drop the oldest beyond that.
                let max = FRAME * 6;
                if pending.len() > max {
                    let extra = pending.len() - max;
                    pending.drain(..extra);
                }
            }
            Err(_) if last_data.elapsed() > Duration::from_millis(300) => {
                pending.extend(std::iter::repeat(0.0).take(FRAME * 5));
            }
            Err(_) => continue,
        }
        while pending.len() >= FRAME {
            // RNNoise expects samples in 16-bit range (-32768..32767), not -1..1.
            let input: Vec<f32> = pending.drain(..FRAME).map(|s| s * 32767.0).collect();
            let vad = denoise.process_frame(&mut cleaned, &input);

            // Level of the *cleaned* frame (0..1) for the UI meter and manual mode.
            let peak = cleaned.iter().fold(0.0f32, |m, s| m.max(s.abs())) / 32767.0;
            state.record(peak);
            let db = level_db(peak);

            // Is this frame speech?
            let gate = *shared.gate.lock();
            let speech = if gate.auto {
                vad >= VAD_OPEN && db >= AUTO_FLOOR_DB
            } else {
                db >= gate.threshold_db
            };
            hold_left = if speech { HOLD_FRAMES } else { hold_left.saturating_sub(1) };
            let open = hold_left > 0 && !shared.muted.load(Ordering::Relaxed);
            state.open.store(open, Ordering::Relaxed);

            // Push the new frame into the delay line and take out the oldest one.
            // Until the line is full (first 30 ms of the call) nothing is sent.
            delay.push_back(cleaned.clone());
            if delay.len() <= PREROLL_FRAMES {
                continue;
            }
            // `unwrap` is safe: we just checked the queue isn't empty.
            let mut out = delay.pop_front().unwrap();

            // Apply the gate with a short fade instead of a hard cut.
            let target = if open { 1.0 } else { 0.0 };
            for s in out.iter_mut() {
                gain += (target - gain).clamp(-RAMP_STEP, RAMP_STEP);
                *s *= gain;
            }

            // Convert to 16-bit integers for WebRTC. `as i16` after `clamp` can't overflow.
            let pcm: Vec<i16> = out.iter().map(|s| s.clamp(-32768.0, 32767.0) as i16).collect();
            let frame = AudioFrame {
                data: pcm.into(),
                sample_rate: RATE,
                num_channels: 1,
                samples_per_channel: FRAME as u32,
            };
            let _ = source.capture_frame(&frame).await;
        }
    }
}

/// Converts audio from the device's sample rate to 48 kHz with linear
/// interpolation (drawing a straight line between neighbouring samples).
/// Good enough for speech; most devices already run at 48 kHz anyway.
struct Resampler {
    /// How far to advance in the input for each output sample (in_rate / 48000).
    step: f64,
    /// Position of the next output sample, measured in input samples from `buf[0]`.
    pos: f64,
    /// Input samples not fully used yet (interpolation needs the next sample too).
    buf: Vec<f32>,
}

impl Resampler {
    fn new(in_rate: u32) -> Self {
        Self { step: in_rate as f64 / RATE as f64, pos: 0.0, buf: Vec::new() }
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        // Same rate: nothing to do.
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        // Produce output samples while both neighbours (i and i+1) are available.
        while self.pos + 1.0 < self.buf.len() as f64 {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.buf[i] * (1.0 - frac) + self.buf[i + 1] * frac);
            self.pos += self.step;
        }
        // Forget input samples we've moved past, and shift `pos` accordingly.
        let used = (self.pos as usize).min(self.buf.len());
        self.buf.drain(..used);
        self.pos -= used as f64;
    }
}

/// Name of the system's current default microphone (`None` if there is none
/// or it can't be read).
fn default_input_name() -> Option<String> {
    cpal::default_host().default_input_device()?.name().ok()
}

/// Opens the default input device. Returns the stream, its sample rate and the
/// device's name.
/// Every callback downmixes to mono -1..1 floats and sends the chunk to `tx`.
fn open(
    tx: mpsc::UnboundedSender<Msg>,
    alive: Arc<AtomicU32>,
    failed: Arc<AtomicBool>,
) -> anyhow::Result<(cpal::Stream, u32, Option<String>)> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("mikrofon yok"))?;
    let config = device.default_input_config()?;
    let name = device.name().ok();
    log::info!("mikrofon: {} ({:?})", name.clone().unwrap_or_default(), config);
    let rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let fmt = config.sample_format();
    let cfg: cpal::StreamConfig = config.into();
    let stream = match fmt {
        cpal::SampleFormat::F32 => build::<f32>(&device, &cfg, channels, tx, alive, failed)?,
        cpal::SampleFormat::I16 => build::<i16>(&device, &cfg, channels, tx, alive, failed)?,
        cpal::SampleFormat::I32 => build::<i32>(&device, &cfg, channels, tx, alive, failed)?,
        cpal::SampleFormat::U16 => build::<u16>(&device, &cfg, channels, tx, alive, failed)?,
        other => anyhow::bail!("desteklenmeyen örnek biçimi: {other:?}"),
    };
    // Windows: opening a microphone counts as "communications activity", and
    // Windows then lowers every other app's volume ("ducking"). The opt-out has
    // to be in place when the stream *starts*, so it's applied right here: the
    // stream (and its audio session) exists now, but isn't running yet.
    // On other systems `disable` does nothing.
    crate::ducking::disable();
    stream.play()?;
    Ok((stream, rate, name))
}

/// Builds the input stream for one sample type `T`.
///
/// This is a *generic* function: `<T>` is a placeholder type, and the `where`
/// clause lists what `T` must support. `SizedSample` = it's a cpal sample type;
/// `f32: FromSample<T>` = a `T` can be converted to `f32` (to -1..1). The compiler
/// creates one copy of the function per type it's used with (f32, i16, ...).
fn build<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    channels: usize,
    tx: mpsc::UnboundedSender<Msg>,
    alive: Arc<AtomicU32>,
    failed: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = channels.max(1);
    let stream = device.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            // Samples are interleaved (L R L R ...): average each group of
            // `channels` samples into one mono sample.
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|c| c.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / c.len() as f32)
                .collect();
            let _ = tx.send(Msg::Data(mono));
            // `wrapping_add` = add, starting again from 0 instead of overflowing.
            alive.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.wrapping_add(1))).ok();
        },
        move |e| {
            log::warn!("mikrofon hatası: {e}");
            failed.store(true, Ordering::Relaxed);
        },
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_keeps_duration() {
        // 1 second at 44.1 kHz, fed in 441-sample chunks, must give ~1 s at 48 kHz.
        let mut r = Resampler::new(44_100);
        let mut out = Vec::new();
        for _ in 0..100 {
            r.push(&[0.5; 441], &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 2, "got {}", out.len());
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn resampler_passthrough_at_48k() {
        let mut r = Resampler::new(48_000);
        let mut out = Vec::new();
        r.push(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }
}
