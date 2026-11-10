//! Short notification sounds ("someone joined / left the voice channel").
//!
//! The sounds aren't audio files: they're generated on the fly as two short
//! sine-wave notes (a rising pair for "join", a falling pair for "leave") and
//! played on the default speakers with `cpal`. That keeps the exe small and needs
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

/// Overall loudness (0..1). Kept low so it doesn't startle anyone.
const VOLUME: f32 = 0.18;
/// Length of each of the two notes.
const NOTE: f32 = 0.11;

/// Plays `sound` in the background; returns immediately. Errors (no speaker,
/// device busy) are only logged: a missing chime isn't worth bothering the user.
pub fn play(sound: Sound) {
    let _ = std::thread::Builder::new().name("bildirim-sesi".into()).spawn(move || {
        if let Err(e) = play_blocking(sound) {
            log::warn!("bildirim sesi çalınamadı: {e:#}");
        }
    });
}

/// The two note frequencies in Hz (E5 = 659 Hz, A5 = 880 Hz: a perfect fourth,
/// which sounds friendly). Join goes up, leave goes down.
fn notes(sound: Sound) -> [f32; 2] {
    match sound {
        Sound::Join => [659.25, 880.0],
        Sound::Leave => [880.0, 659.25],
    }
}

/// Builds the mono samples of the whole sound at `rate` samples per second.
fn render(sound: Sound, rate: f32) -> Vec<f32> {
    let per_note = (NOTE * rate) as usize;
    let mut out = Vec::with_capacity(per_note * 2);
    for freq in notes(sound) {
        for i in 0..per_note {
            let t = i as f32 / rate;
            // Envelope: a 5 ms fade-in (avoids a click), then an exponential decay
            // so each note "rings" like a soft bell instead of a flat beep.
            let attack = (t / 0.005).min(1.0);
            let decay = (-t * 18.0).exp();
            // Sine wave plus a quiet octave above it for a slightly richer tone.
            let wave = (TAU * freq * t).sin() + 0.25 * (TAU * 2.0 * freq * t).sin();
            out.push(wave * attack * decay * VOLUME);
        }
    }
    out
}

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
