//! Brings shared app audio (music) to a steady loudness.
//!
//! So that it reaches everyone at the same loudness no matter how loud the
//! sharer's app is set:
//! - The average loudness is measured over a window of a few seconds and the gain
//!   follows it slowly, so the song's own loud/quiet parts (its dynamics) survive.
//! - Silent moments are not measured, so pausing doesn't make the gain climb and
//!   then blast when playback resumes.
//! - A peak limiter only kicks in for peaks that would clip after the gain.
//!
//! Samples are processed as interleaved floats in the range -1..1
//! (interleaved stereo = L, R, L, R, ...).
//!
//! Audio terms: "dBFS" = decibels relative to full scale (0 dBFS is the loudest
//! possible sample, -6 dBFS is about half the amplitude). "RMS" = root mean square,
//! a measure of average loudness. "Gain" = a multiplier applied to the samples.

/// Target average loudness (RMS, dBFS). Close to what music services normalize to.
const TARGET_DB: f32 = -18.0;
/// Slices quieter than this count as silence and aren't measured.
const SILENCE_DB: f32 = -55.0;
/// Gain limits: boost by at most +18 dB (×7.94), reduce by at most -12 dB (×0.25).
const MAX_GAIN: f32 = 7.94;
const MIN_GAIN: f32 = 0.25;
/// Limiter ceiling (a little headroom below 1.0 = clipping).
const CEILING: f32 = 0.93;

/// Converts decibels to a linear amplitude factor (-20 dB -> 0.1, 0 dB -> 1.0).
fn db_to_lin(db: f32) -> f32 {
    // `10f32` is the literal 10 as an f32; `powf` = raised to a float power.
    10f32.powf(db / 20.0)
}

/// State that must survive between audio slices. One `Normalizer` per stream.
pub struct Normalizer {
    /// Running estimate of the average power (RMS²); `None` until something has
    /// been measured.
    power: Option<f32>,
    /// How many slices have been measured (lets the estimate settle fast at first).
    measured: u32,
    /// Current gain.
    gain: f32,
    /// Current limiter gain (≤ 1).
    limit: f32,
}

impl Default for Normalizer {
    fn default() -> Self {
        Self { power: None, measured: 0, gain: 1.0, limit: 1.0 }
    }
}

impl Normalizer {
    /// Processes one slice (≈10 ms) *in place*: `&mut [f32]` is a mutable borrow of
    /// the caller's buffer, so we overwrite the samples directly instead of
    /// returning a new buffer.
    pub fn process(&mut self, buf: &mut [f32]) {
        if buf.is_empty() {
            return;
        }
        // 1) Measure loudness (ignoring silence).
        // Mean square = average of sample² (`sum::<f32>()` tells `sum` the result type).
        let ms = buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32;
        if ms > db_to_lin(SILENCE_DB).powi(2) {
            // `saturating_add` stops at the maximum instead of overflowing.
            self.measured = self.measured.saturating_add(1);
            // During the first second this is a plain average (alpha = 1/n); after
            // that an exponential moving average with a ~3 s memory (alpha ≈ 0.0033
            // per 10 ms slice). Each update moves the estimate a fraction `alpha`
            // of the way towards the new value.
            let alpha = (1.0 / self.measured as f32).max(0.0033);
            self.power = Some(match self.power {
                Some(p) => p + (ms - p) * alpha,
                None => ms,
            });
        }

        // 2) The gain moves slowly (~0.5 s) towards its target, and changes
        //    linearly across the slice so there are no audible steps.
        // The match guard `if p > 0.0` adds an extra condition to the pattern.
        let target = match self.power {
            Some(p) if p > 0.0 => (db_to_lin(TARGET_DB) / p.sqrt()).clamp(MIN_GAIN, MAX_GAIN),
            _ => 1.0,
        };
        let from = self.gain;
        let to = from + (target - from) * 0.02;
        self.gain = to;

        // 3) Peak limiter: if the boosted peak would exceed the ceiling, turn down
        //    immediately; afterwards release slowly back towards 1.
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs())) * from.max(to);
        let needed = if peak > CEILING { CEILING / peak } else { 1.0 };
        let released = self.limit + (1.0 - self.limit) * 0.05; // ~200 ms
        self.limit = released.min(needed);

        let n = buf.len() as f32;
        // `iter_mut()` yields mutable references; `*s = ...` writes through them.
        for (i, s) in buf.iter_mut().enumerate() {
            let g = from + (to - from) * (i as f32 / n);
            *s = (*s * g * self.limit).clamp(-1.0, 1.0);
        }
    }
}

// Unit tests. `#[cfg(test)]` = only compiled for `cargo test`, not in the app.
// Run them with: `cargo test --bin goodbyedc normalize`
#[cfg(test)]
mod tests {
    // `super::*` imports everything from the parent module (the code above).
    use super::*;

    /// A test signal: a sine wave of the given amplitude.
    fn sine(amp: f32, n: usize, offset: usize) -> Vec<f32> {
        // `.collect()` gathers the iterator's values into a Vec.
        (0..n).map(|i| amp * ((i + offset) as f32 * 0.0577).sin()).collect()
    }

    fn rms_db(b: &[f32]) -> f32 {
        let ms = b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32;
        10.0 * ms.log10()
    }

    /// After a few seconds both quiet and loud input end up near the target.
    #[test]
    fn reaches_target() {
        // 0.02 ≈ -37 dB (very quiet), 0.5 ≈ -9 dB (loud). Louder input can only be
        // reduced by 12 dB, so it isn't part of this test.
        for amp in [0.02f32, 0.1, 0.5] {
            let mut n = Normalizer::default();
            let mut last = Vec::new();
            for k in 0..800 {
                let mut b = sine(amp, 960, k * 960);
                n.process(&mut b);
                last = b;
            }
            let db = rms_db(&last);
            // `assert!` fails the test (with this message) if the condition is false.
            assert!((db - TARGET_DB).abs() < 1.5, "genlik {amp}: {db:.1} dB");
        }
    }

    /// No sample ever exceeds the ceiling (even when sudden loud audio arrives).
    #[test]
    fn never_clips() {
        let mut n = Normalizer::default();
        for k in 0..300 {
            let mut b = sine(0.01, 960, k * 960);
            n.process(&mut b);
        }
        let mut loud = sine(1.0, 960, 0);
        n.process(&mut loud);
        assert!(loud.iter().all(|s| s.abs() <= CEILING + 1e-4));
    }

    /// Silence doesn't increase the gain.
    #[test]
    fn silence_keeps_gain() {
        let mut n = Normalizer::default();
        for k in 0..300 {
            let mut b = sine(0.3, 960, k * 960);
            n.process(&mut b);
        }
        let g = n.gain;
        for _ in 0..500 {
            // `vec![0.0; 960]` = a Vec of 960 zeros.
            let mut b = vec![0.0; 960];
            n.process(&mut b);
        }
        assert!((n.gain - g).abs() < 0.05);
    }
}
