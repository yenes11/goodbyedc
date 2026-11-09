//! Microphone test ("hear yourself"): plays the default microphone back through
//! the default speakers/headphones and measures its level.
//!
//! It is independent of the voice channel (doesn't touch WebRTC); it's started from
//! the settings window and released when the window closes or the test is stopped.
//!
//! How it works: two cpal audio streams run at the same time. The *input* stream's
//! callback pushes microphone samples into a shared queue; the *output* stream's
//! callback pops them and plays them. The queue is shared between the two audio
//! threads through `Arc<Mutex<...>>`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

/// Maximum audio kept in the queue (so the delay doesn't keep growing).
const MAX_BUFFER: Duration = Duration::from_millis(80);

/// Running test. Dropping it stops the test (see `impl Drop` below).
pub struct MicTest {
    /// Highest level since last read, stored as f32 bits (see meter.rs).
    peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

impl MicTest {
    /// Opens the microphone and speakers and starts playing back.
    /// Returns an error (e.g. "Mikrofon bulunamadı") if a device can't be opened.
    pub fn start() -> Result<Self> {
        let peak = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        // `channel::<Result<()>>()` spells out the message type with "turbofish"
        // syntax: the thread sends back either Ok(()) or the opening error.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        {
            let peak = peak.clone();
            let stop = stop.clone();
            // On some platforms cpal streams can't move between threads, so they
            // live on their own thread from start to finish.
            std::thread::Builder::new().name("mikrofon-testi".into()).spawn(move || {
                let streams = match open(peak) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                // Dropping the streams closes the devices.
                drop(streams);
                log::info!("mikrofon testi durdu");
            })?;
        }
        // Nested patterns: the outer Ok/Err is "did we get an answer in time?",
        // the inner Ok/Err is "did opening the devices work?".
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { peak, stop }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err(anyhow!("ses aygıtı yanıt vermedi"))
            }
        }
    }

    /// Highest microphone level (0..1) since the previous call.
    pub fn take_level(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Opens both streams and returns them as a tuple `(input, output)`.
fn open(peak: Arc<AtomicU32>) -> Result<(cpal::Stream, cpal::Stream)> {
    let host = cpal::default_host();
    let input = host.default_input_device().ok_or_else(|| anyhow!("Mikrofon bulunamadı"))?;
    let output = host.default_output_device().ok_or_else(|| anyhow!("Hoparlör bulunamadı"))?;
    let in_cfg = input.default_input_config()?;
    let out_cfg = output.default_output_config()?;
    log::info!(
        "mikrofon testi: {} ({:?}) -> {} ({:?})",
        input.name().unwrap_or_default(),
        in_cfg,
        output.name().unwrap_or_default(),
        out_cfg
    );

    // Sample rates can differ (e.g. mic 48000 Hz, speakers 44100 Hz), so the
    // output side converts between them (see `build_output`).
    let in_rate = in_cfg.sample_rate().0;
    let out_rate = out_cfg.sample_rate().0;
    // Queue size limit in samples.
    let max = (in_rate as f32 * MAX_BUFFER.as_secs_f32()) as usize;
    // Mono samples from the microphone. `Mutex` = a lock: only one thread may
    // access the queue at a time. `VecDeque` = a double-ended queue (push at the
    // back, pop at the front).
    let buf = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(max * 2)));

    // `build_input::<f32>` calls the generic function with T = f32, and so on
    // for each sample format the device might use.
    let in_stream = match in_cfg.sample_format() {
        cpal::SampleFormat::F32 => build_input::<f32>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        cpal::SampleFormat::I16 => build_input::<i16>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        cpal::SampleFormat::I32 => build_input::<i32>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        cpal::SampleFormat::U16 => build_input::<u16>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        other => bail!("desteklenmeyen mikrofon biçimi: {other:?}"),
    };
    // How many input samples to advance per output sample.
    let step = in_rate as f32 / out_rate as f32;
    let out_stream = match out_cfg.sample_format() {
        cpal::SampleFormat::F32 => build_output::<f32>(&output, &out_cfg.into(), buf, step)?,
        cpal::SampleFormat::I16 => build_output::<i16>(&output, &out_cfg.into(), buf, step)?,
        cpal::SampleFormat::I32 => build_output::<i32>(&output, &out_cfg.into(), buf, step)?,
        cpal::SampleFormat::U16 => build_output::<u16>(&output, &out_cfg.into(), buf, step)?,
        other => bail!("desteklenmeyen hoparlör biçimi: {other:?}"),
    };
    in_stream.play()?;
    out_stream.play()?;
    Ok((in_stream, out_stream))
}

/// Builds the microphone stream. Generic over the sample type `T`.
///
/// The `where` clause lists what `T` must support:
///   - `SizedSample`: T is one of cpal's sample types.
///   - `f32: FromSample<T>`: a T can be converted into an f32.
/// The compiler generates a separate copy of this function for each T used.
fn build_input<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    buf: Arc<Mutex<VecDeque<f32>>>,
    peak: Arc<AtomicU32>,
    max: usize,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = cfg.channels.max(1) as usize;
    Ok(device.build_input_stream(
        cfg,
        // Called on the audio thread with a slice of interleaved samples.
        move |data: &[T], _: &_| {
            let mut p = 0.0f32;
            // `lock()` waits until no other thread holds the lock and returns a
            // guard; `unwrap()` panics if a thread crashed while holding it.
            let mut b = buf.lock().unwrap();
            // `chunks(channels)` yields one frame (all channels of one instant) at a time.
            for frame in data.chunks(channels) {
                // Average the channels into one mono sample.
                let s = frame.iter().map(|x| x.to_sample::<f32>()).sum::<f32>() / frame.len() as f32;
                p = p.max(s.abs());
                b.push_back(s);
            }
            // Drop the oldest samples if the queue got too long.
            if b.len() > max {
                let extra = b.len() - max;
                b.drain(..extra);
            }
            // Release the lock as early as possible (the guard unlocks when dropped).
            drop(b);
            let _ = peak.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                (p > f32::from_bits(old)).then_some(p.to_bits())
            });
        },
        |e| log::warn!("mikrofon testi (giriş) hatası: {e}"),
        None,
    )?)
}

/// Builds the speaker stream: takes mono samples from the queue, converts the
/// sample rate by linear interpolation and writes the same value to every channel.
fn build_output<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    buf: Arc<Mutex<VecDeque<f32>>>,
    step: f32,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = cfg.channels.max(1) as usize;
    // State for linear sample-rate conversion. These variables are captured by
    // the `move` closure below and keep their values between callbacks.
    let mut phase = 1.0f32;
    let mut cur = 0.0f32;
    let mut next = 0.0f32;
    Ok(device.build_output_stream(
        cfg,
        // `&mut [T]`: the buffer we must fill with samples to play.
        move |data: &mut [T], _: &_| {
            let mut b = buf.lock().unwrap();
            for frame in data.chunks_mut(channels) {
                // Advance through the input samples as needed.
                while phase >= 1.0 {
                    cur = next;
                    // If the queue runs dry, fade towards silence (avoids clicks).
                    next = b.pop_front().unwrap_or(next * 0.9);
                    phase -= 1.0;
                }
                // Interpolate between the two neighbouring input samples.
                let s = cur + (next - cur) * phase;
                phase += step;
                let v = T::from_sample(s.clamp(-1.0, 1.0));
                frame.iter_mut().for_each(|o| *o = v);
            }
        },
        |e| log::warn!("mikrofon testi (çıkış) hatası: {e}"),
        None,
    )?)
}
