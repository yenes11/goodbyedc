//! Microphone level meter.
//!
//! WebRTC stops measuring the microphone level while our mic track is disabled
//! (i.e. while the voice gate is closed because we're below the sensitivity
//! threshold). That meant the gate could close once and never open again. This
//! meter listens to the default microphone separately (with the `cpal` audio
//! library), so it always reports a level, whether the track is on or off.
//!
//! Concepts used here:
//!   - `Arc<T>` ("atomically reference counted"): a pointer that lets several
//!     owners (here: two threads) share one value. The value is freed when the
//!     last `Arc` is dropped.
//!   - Atomics (`AtomicU32`, `AtomicBool`): numbers that can be read and written
//!     from several threads at once safely, without a lock.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

// cpal's traits must be in scope to call their methods (e.g. `play()`).
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct Meter {
    /// Highest level seen since the last read (0..1). There is no `AtomicF32`, so
    /// the float is stored as its raw bits in an `AtomicU32`
    /// (`f32::to_bits` / `f32::from_bits` convert without changing the bits).
    peak: Arc<AtomicU32>,
    /// Set to true to make the audio thread shut the microphone stream down.
    stop: Arc<AtomicBool>,
}

impl Meter {
    /// Starts listening to the default microphone. Returns `None` if the
    /// microphone can't be opened (no device, no permission...).
    pub fn start() -> Option<Self> {
        let peak = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        // A channel: a one-way queue between threads. The audio thread uses
        // `ready_tx` (sender) to tell us whether opening worked; we wait on
        // `ready_rx` (receiver).
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        // An inner block so the cloned `peak`/`stop` below shadow the outer ones
        // only inside it; the outer ones are kept for `Self { peak, stop }`.
        {
            // `Arc::clone` doesn't copy the data, it creates another handle to it.
            let peak = peak.clone();
            let stop = stop.clone();
            // On some platforms a cpal stream can't be moved between threads, so
            // it is opened, kept and closed on its own dedicated thread.
            std::thread::Builder::new()
                .name("mikrofon-olcer".into())
                // `move` transfers ownership of `peak`, `stop` and `ready_tx` into
                // the new thread.
                .spawn(move || {
                    let stream = match open(peak) {
                        Ok(s) => s,
                        Err(e) => {
                            // `{e:#}` prints the error with its full cause chain.
                            log::warn!("mikrofon ölçer açılamadı: {e:#}");
                            let _ = ready_tx.send(false);
                            return;
                        }
                    };
                    let _ = ready_tx.send(true);
                    // Keep the stream alive until asked to stop. (`Relaxed` ordering
                    // is enough for a simple flag like this.)
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    // Dropping the stream closes the microphone.
                    drop(stream);
                })
                // `spawn` returns a Result; `.ok()?` returns None from `start` if
                // the thread couldn't be created.
                .ok()?;
        }
        // Wait up to 5 seconds for the thread's answer.
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(true) => Some(Self { peak, stop }),
            // `_` matches everything else: failure, timeout or a closed channel.
            _ => {
                stop.store(true, Ordering::Relaxed);
                None
            }
        }
    }

    /// The highest level (0..1) since the previous call; reading resets it to 0.
    /// `swap` atomically stores 0 and returns the old value in one step, so no
    /// sample can sneak in between "read" and "reset".
    pub fn take_level(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

// `Drop` is Rust's destructor: `drop` runs automatically when a `Meter` goes out
// of scope. We use it to stop the background thread (and thus the microphone).
impl Drop for Meter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Opens the default input device and starts a stream that records the peak
/// level of every audio buffer into `peak`.
///
/// `anyhow::Result<T>` is a Result whose error can be any error type — convenient
/// for application code where we only want to report errors, not inspect them.
fn open(peak: Arc<AtomicU32>) -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();
    // `ok_or_else` turns `None` into an error so `?` can propagate it.
    let device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("mikrofon yok"))?;
    let config = device.default_input_config()?;
    log::info!("mikrofon ölçer: {} ({:?})", device.name().unwrap_or_default(), config);
    // Error callback for the stream (called on cpal's audio thread).
    let err = |e| log::warn!("mikrofon ölçer hatası: {e}");
    let fmt = config.sample_format();
    // `: cpal::StreamConfig` is a type annotation telling `.into()` what to produce.
    let cfg: cpal::StreamConfig = config.into();

    // A function defined inside a function: visible only here.
    fn record(peak: &AtomicU32, p: f32) {
        // Store `p` only if it's larger than the current value, so the highest
        // level survives until the reader resets it. `fetch_update` retries
        // automatically if another thread changed the value in between.
        // `cond.then_some(x)` = `Some(x)` if cond is true, else `None` (= "don't update").
        let _ = peak.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            (p > f32::from_bits(old)).then_some(p.to_bits())
        });
    }

    // Microphones deliver samples in different formats. For each one, build a
    // stream whose callback converts samples to -1..1 floats and records the peak
    // (largest absolute value). `fold` walks all samples keeping the running max.
    // The callbacks run on a real-time audio thread, so they must be fast.
    let stream = match fmt {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &cfg,
            move |d: &[f32], _: &_| record(&peak, d.iter().fold(0.0f32, |m, s| m.max(s.abs()))),
            err,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &cfg,
            move |d: &[i16], _: &_| {
                // `*s` dereferences the borrowed sample; `as f32` converts it.
                record(&peak, d.iter().fold(0.0f32, |m, s| m.max((*s as f32 / 32768.0).abs())))
            },
            err,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            &cfg,
            move |d: &[i32], _: &_| {
                record(&peak, d.iter().fold(0.0f32, |m, s| m.max((*s as f32 / 2147483648.0).abs())))
            },
            err,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &cfg,
            move |d: &[u16], _: &_| {
                // Unsigned samples are centered on 32768 instead of 0.
                record(&peak, d.iter().fold(0.0f32, |m, s| m.max(((*s as f32 - 32768.0) / 32768.0).abs())))
            },
            err,
            None,
        )?,
        // `bail!` returns an error immediately.
        other => anyhow::bail!("desteklenmeyen örnek biçimi: {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}
