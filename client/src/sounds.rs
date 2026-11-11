//! Short notification sounds ("someone joined / left the voice channel").
//!
//! The sounds aren't audio files: they're synthesized on the fly as a short
//! three-note arpeggio on a soft, bell-like "instrument" (rising for "join",
//! falling for "leave") and played on the default speakers with `cpal`. That keeps the exe small and needs
//! no file loading.

use std::f32::consts::TAU;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

#[derive(Clone, Copy, Debug)]
pub enum Sound {
    Join,
    Leave,
}

/// Plays `sound` in the background; returns immediately. Errors (no speaker,
/// device busy) are only logged: a missing chime isn't worth bothering the user.
pub fn play(sound: Sound) {
    let _ = std::thread::Builder::new().name("bildirim-sesi".into()).spawn(move || {
        if let Err(e) = play_blocking(sound) {
            log::warn!("bildirim sesi çalınamadı: {e:#}");
        }
    });
}

/// Peak loudness of the finished sound (0..1). Kept low so it doesn't startle anyone.
const VOLUME: f32 = 0.2;
/// Time between the starts of consecutive notes (they overlap, like a strum).
const STEP: f32 = 0.075;
/// How long each note is rendered; it has faded out well before this.
const RING: f32 = 0.55;

/// The notes in Hz. Both sounds use the A major chord (A, C#, E), which sounds
/// bright and friendly:
/// - join: a rising arpeggio, A5 → C#6 → E6 ("hello!"),
/// - leave: a falling one an octave lower, E5 → C#5 → A4 ("bye"), so it reads
///   as softer and is easy to tell apart.
fn notes(sound: Sound) -> &'static [f32] {
    match sound {
        Sound::Join => &[880.0, 1108.73, 1318.51],
        Sound::Leave => &[659.25, 554.37, 440.0],
    }
}

/// One partial (overtone) of a note: frequency as a multiple of the note's
/// frequency, its loudness, and how fast it dies away (higher = shorter).
struct Partial {
    ratio: f32,
    gain: f32,
    decay: f32,
}

/// The "instrument": a soft mallet / kalimba-like tone. A real struck bar has
/// overtones that are *not* exact multiples of the base frequency (that's what
/// makes it sound like a bell or a marimba instead of an organ), and the higher
/// ones fade faster, so the attack sparkles and the tail is a pure, warm tone.
const PARTIALS: [Partial; 4] = [
    Partial { ratio: 1.0, gain: 1.0, decay: 7.0 },
    Partial { ratio: 2.0, gain: 0.30, decay: 11.0 },
    Partial { ratio: 3.98, gain: 0.12, decay: 22.0 },
    Partial { ratio: 6.27, gain: 0.05, decay: 40.0 },
];

/// Builds the mono samples of the whole sound at `rate` samples per second.
fn render(sound: Sound, rate: f32) -> Vec<f32> {
    let notes = notes(sound);
    // Total length: the last note's start + its ring time + room for the echo.
    let total = ((STEP * (notes.len() - 1) as f32 + RING + ECHO_DELAY * 2.0) * rate) as usize;
    let mut out = vec![0.0f32; total];

    // 1) Mix the notes. `enumerate` gives (index, value) pairs.
    for (n, &freq) in notes.iter().enumerate() {
        let offset = (n as f32 * STEP * rate) as usize;
        // Later notes slightly quieter, so the arpeggio "settles".
        let note_gain = 1.0 - 0.15 * n as f32;
        for i in 0..(RING * rate) as usize {
            let t = i as f32 / rate;
            // 3 ms fade-in: avoids a click but still feels like a soft strike.
            let attack = (t / 0.003).min(1.0);
            let mut v = 0.0;
            for p in &PARTIALS {
                v += p.gain * (-t * p.decay).exp() * (TAU * freq * p.ratio * t).sin();
            }
            out[offset + i] += v * attack * note_gain;
        }
    }

    // 2) A short, quiet echo gives a sense of space (like a small room)
    //    instead of the sound stopping dead. Each sample gets a copy of the one
    //    `ECHO_DELAY` earlier mixed in; going forwards makes the echo repeat
    //    and fade by itself (feedback).
    let d = (ECHO_DELAY * rate) as usize;
    for i in d..out.len() {
        out[i] += out[i - d] * ECHO_GAIN;
    }

    // 3) Scale so the loudest sample is exactly VOLUME, whatever the mix adds up to.
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        for s in &mut out {
            *s *= VOLUME / peak;
        }
    }
    out
}

/// Echo settings: delay in seconds and how loud each repeat is.
const ECHO_DELAY: f32 = 0.11;
const ECHO_GAIN: f32 = 0.22;

/// Opens the default output device, plays the sound and waits until it's done.
fn play_blocking(sound: Sound) -> anyhow::Result<()> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("hoparlör yok"))?;
    let config = device.default_output_config()?;
    let rate = config.sample_rate().0 as f32;
    let channels = config.channels() as usize;
    let samples = render(sound, rate);
    let length = Duration::from_secs_f32(samples.len() as f32 / rate);
    let fmt = config.sample_format();
    let cfg: cpal::StreamConfig = config.into();
    let stream = match fmt {
        cpal::SampleFormat::F32 => build::<f32>(&device, &cfg, channels, samples)?,
        cpal::SampleFormat::I16 => build::<i16>(&device, &cfg, channels, samples)?,
        cpal::SampleFormat::U16 => build::<u16>(&device, &cfg, channels, samples)?,
        cpal::SampleFormat::I32 => build::<i32>(&device, &cfg, channels, samples)?,
        other => anyhow::bail!("desteklenmeyen örnek biçimi: {other:?}"),
    };
    stream.play()?;
    // Keep the stream alive until the sound has played (plus a little margin for
    // the device's own buffer); dropping it at the end of this function stops it.
    std::thread::sleep(length + Duration::from_millis(150));
    Ok(())
}

/// Builds an output stream for sample type `T` (see micproc.rs `build` for how
/// the generic `<T>` + `where` clause works).
fn build<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    channels: usize,
    samples: Vec<f32>,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = channels.max(1);
    // Position of the next sample to play; moved into the callback, which owns it.
    let mut pos = 0usize;
    let stream = device.build_output_stream(
        cfg,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            // The output is interleaved (L R L R ...): write the same mono sample
            // to every channel of each frame; silence once the sound is over.
            for frame in out.chunks_mut(channels) {
                let s = samples.get(pos).copied().unwrap_or(0.0);
                pos += 1;
                for slot in frame {
                    *slot = T::from_sample(s);
                }
            }
        },
        |e| log::warn!("bildirim sesi hatası: {e}"),
        None,
    )?;
    Ok(stream)
}
