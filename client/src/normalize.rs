//! Paylaşılan uygulama sesini (müzik) sabit bir yüksekliğe getirir.
//!
//! Yayın yapanın uygulamadaki ses ayarı ne olursa olsun herkese aynı
//! yükseklikte gitsin diye:
//! - Ortalama yükseklik birkaç saniyelik bir pencerede ölçülür ve kazanç ona göre
//!   yavaşça değişir; şarkı içindeki sesli/sessiz anlar (dinamik) korunur.
//! - Sessiz anlar ölçüme katılmaz; duraklatınca kazanç yükselip sonra patlamaz.
//! - Tepe sınırlayıcı yalnızca kazançtan sonra kırpılacak tepelerde devreye girer.
//!
//! Örnekler -1..1 aralığında, araya karışık (interleaved) float olarak işlenir.

/// Hedef ortalama yükseklik (RMS, dBFS). Müzik servislerinin normalleştirmesine yakın.
const TARGET_DB: f32 = -18.0;
/// Bunun altındaki dilimler sessizlik sayılır ve ölçüme katılmaz.
const SILENCE_DB: f32 = -55.0;
/// Kazanç sınırları: en fazla +18 dB yükselt, en fazla -12 dB kıs.
const MAX_GAIN: f32 = 7.94;
const MIN_GAIN: f32 = 0.25;
/// Sınırlayıcının tavanı (kırpmadan önce bırakılan pay).
const CEILING: f32 = 0.93;

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub struct Normalizer {
    /// Ortalama güç tahmini (RMS²); henüz ölçüm yoksa `None`.
    power: Option<f32>,
    /// Ölçüme katılan dilim sayısı (başta hızlı oturması için).
    measured: u32,
    /// O anki kazanç.
    gain: f32,
    /// Sınırlayıcının o anki kazancı (≤ 1).
    limit: f32,
}

impl Default for Normalizer {
    fn default() -> Self {
        Self { power: None, measured: 0, gain: 1.0, limit: 1.0 }
    }
}

impl Normalizer {
    /// Bir dilimi (≈10 ms) yerinde işler.
    pub fn process(&mut self, buf: &mut [f32]) {
        if buf.is_empty() {
            return;
        }
        // 1) Yükseklik ölçümü (sessizlik hariç).
        let ms = buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32;
        if ms > db_to_lin(SILENCE_DB).powi(2) {
            self.measured = self.measured.saturating_add(1);
            // İlk saniyede düz ortalama, sonra ~3 sn'lik üstel ortalama.
            let alpha = (1.0 / self.measured as f32).max(0.0033);
            self.power = Some(match self.power {
                Some(p) => p + (ms - p) * alpha,
                None => ms,
            });
        }

        // 2) Kazanç hedefe yavaşça (~0,5 sn) yaklaşır; dilim içinde doğrusal geçiş.
        let target = match self.power {
            Some(p) if p > 0.0 => (db_to_lin(TARGET_DB) / p.sqrt()).clamp(MIN_GAIN, MAX_GAIN),
            _ => 1.0,
        };
        let from = self.gain;
        let to = from + (target - from) * 0.02;
        self.gain = to;

        // 3) Tepe sınırlayıcı: kazançtan sonra tavanı aşacaksa hemen kıs, sonra yavaş bırak.
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs())) * from.max(to);
        let needed = if peak > CEILING { CEILING / peak } else { 1.0 };
        let released = self.limit + (1.0 - self.limit) * 0.05; // ~200 ms
        self.limit = released.min(needed);

        let n = buf.len() as f32;
        for (i, s) in buf.iter_mut().enumerate() {
            let g = from + (to - from) * (i as f32 / n);
            *s = (*s * g * self.limit).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, n: usize, offset: usize) -> Vec<f32> {
        (0..n).map(|i| amp * ((i + offset) as f32 * 0.0577).sin()).collect()
    }

    fn rms_db(b: &[f32]) -> f32 {
        let ms = b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32;
        10.0 * ms.log10()
    }

    /// Birkaç saniye sonra hem kısık hem yüksek ses hedefe yaklaşır.
    #[test]
    fn reaches_target() {
        // 0.02 ≈ -37 dB (çok kısık), 0.5 ≈ -9 dB (yüksek). Daha yükseği en fazla 12 dB kısılır.
        for amp in [0.02f32, 0.1, 0.5] {
            let mut n = Normalizer::default();
            let mut last = Vec::new();
            for k in 0..800 {
                let mut b = sine(amp, 960, k * 960);
                n.process(&mut b);
                last = b;
            }
            let db = rms_db(&last);
            assert!((db - TARGET_DB).abs() < 1.5, "genlik {amp}: {db:.1} dB");
        }
    }

    /// Hiçbir örnek tavanı aşmaz (ani yüksek ses gelse bile).
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

    /// Sessizlik kazancı büyütmez.
    #[test]
    fn silence_keeps_gain() {
        let mut n = Normalizer::default();
        for k in 0..300 {
            let mut b = sine(0.3, 960, k * 960);
            n.process(&mut b);
        }
        let g = n.gain;
        for _ in 0..500 {
            let mut b = vec![0.0; 960];
            n.process(&mut b);
        }
        assert!((n.gain - g).abs() < 0.05);
    }
}
