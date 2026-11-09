//! Mikrofon testi ("kendi sesini duy"): varsayılan mikrofonu varsayılan
//! hoparlöre/kulaklığa geri çalar ve seviyesini ölçer.
//!
//! Ses kanalından bağımsızdır (WebRTC'ye dokunmaz); ayarlar penceresinde
//! açılır, pencere kapanınca ya da test durdurulunca bırakılır.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

/// Tamponda tutulacak en fazla ses (gecikme büyümesin).
const MAX_BUFFER: Duration = Duration::from_millis(80);

pub struct MicTest {
    peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

impl MicTest {
    pub fn start() -> Result<Self> {
        let peak = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        {
            let peak = peak.clone();
            let stop = stop.clone();
            // cpal akışları bazı platformlarda iş parçacıkları arasında taşınamıyor.
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
                drop(streams);
                log::info!("mikrofon testi durdu");
            })?;
        }
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { peak, stop }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err(anyhow!("ses aygıtı yanıt vermedi"))
            }
        }
    }

    /// Son okumadan bu yana en yüksek mikrofon seviyesi (0..1).
    pub fn take_level(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }
}

impl Drop for MicTest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

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

    let in_rate = in_cfg.sample_rate().0;
    let out_rate = out_cfg.sample_rate().0;
    let max = (in_rate as f32 * MAX_BUFFER.as_secs_f32()) as usize;
    // Mikrofondan gelen tek kanallı (mono) örnekler.
    let buf = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(max * 2)));

    let in_stream = match in_cfg.sample_format() {
        cpal::SampleFormat::F32 => build_input::<f32>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        cpal::SampleFormat::I16 => build_input::<i16>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        cpal::SampleFormat::I32 => build_input::<i32>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        cpal::SampleFormat::U16 => build_input::<u16>(&input, &in_cfg.into(), buf.clone(), peak, max)?,
        other => bail!("desteklenmeyen mikrofon biçimi: {other:?}"),
    };
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
        move |data: &[T], _: &_| {
            let mut p = 0.0f32;
            let mut b = buf.lock().unwrap();
            for frame in data.chunks(channels) {
                // Kanalların ortalaması (mono).
                let s = frame.iter().map(|x| x.to_sample::<f32>()).sum::<f32>() / frame.len() as f32;
                p = p.max(s.abs());
                b.push_back(s);
            }
            if b.len() > max {
                let extra = b.len() - max;
                b.drain(..extra);
            }
            drop(b);
            let _ = peak.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                (p > f32::from_bits(old)).then_some(p.to_bits())
            });
        },
        |e| log::warn!("mikrofon testi (giriş) hatası: {e}"),
        None,
    )?)
}

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
    // Doğrusal örnekleme hızı dönüştürme durumu.
    let mut phase = 1.0f32;
    let mut cur = 0.0f32;
    let mut next = 0.0f32;
    Ok(device.build_output_stream(
        cfg,
        move |data: &mut [T], _: &_| {
            let mut b = buf.lock().unwrap();
            for frame in data.chunks_mut(channels) {
                while phase >= 1.0 {
                    cur = next;
                    // Veri bitince sessizliğe doğru sön (çıtırtı olmasın).
                    next = b.pop_front().unwrap_or(next * 0.9);
                    phase -= 1.0;
                }
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
