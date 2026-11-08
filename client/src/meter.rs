//! Mikrofon seviye ölçer.
//!
//! WebRTC, mikrofon track'i kapalıyken (hassasiyet eşiğinin altında) ses
//! seviyesini ölçmeyi bırakıyor; o yüzden eşik bir kez kapanınca bir daha
//! açılamıyordu. Bu ölçer varsayılan mikrofonu ayrıca dinler ve track açık
//! ya da kapalı olsun her zaman seviyeyi verir.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct Meter {
    /// Son ölçülen tepe seviye (0..1), f32 bitleri olarak.
    peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

impl Meter {
    /// Varsayılan mikrofonu dinlemeye başlar. Mikrofon açılamazsa `None`.
    pub fn start() -> Option<Self> {
        let peak = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        {
            let peak = peak.clone();
            let stop = stop.clone();
            // cpal akışı bazı platformlarda iş parçacıkları arasında taşınamıyor;
            // kendi iş parçacığında açılıp kapatılır.
            std::thread::Builder::new()
                .name("mikrofon-olcer".into())
                .spawn(move || {
                    let stream = match open(peak) {
                        Ok(s) => s,
                        Err(e) => {
                            log::warn!("mikrofon ölçer açılamadı: {e:#}");
                            let _ = ready_tx.send(false);
                            return;
                        }
                    };
                    let _ = ready_tx.send(true);
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    drop(stream);
                })
                .ok()?;
        }
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(true) => Some(Self { peak, stop }),
            _ => {
                stop.store(true, Ordering::Relaxed);
                None
            }
        }
    }

    /// Son okumadan bu yana gelen en yüksek seviye (0..1). Okuyunca sıfırlanır.
    pub fn take_level(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn open(peak: Arc<AtomicU32>) -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("mikrofon yok"))?;
    let config = device.default_input_config()?;
    log::info!("mikrofon ölçer: {} ({:?})", device.name().unwrap_or_default(), config);
    let err = |e| log::warn!("mikrofon ölçer hatası: {e}");
    let fmt = config.sample_format();
    let cfg: cpal::StreamConfig = config.into();

    fn record(peak: &AtomicU32, p: f32) {
        // Önceki değerden büyükse yaz (okuyucu sıfırlayana kadar en yüksek kalır).
        let _ = peak.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            (p > f32::from_bits(old)).then_some(p.to_bits())
        });
    }

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
                record(&peak, d.iter().fold(0.0f32, |m, s| m.max(((*s as f32 - 32768.0) / 32768.0).abs())))
            },
            err,
            None,
        )?,
        other => anyhow::bail!("desteklenmeyen örnek biçimi: {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}
