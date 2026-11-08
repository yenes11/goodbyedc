//! Ekran yakalama (gönderen taraf) ve gelen ekran görüntüsünü JPEG'e
//! çevirme (izleyen taraf).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use libwebrtc::desktop_capturer::{
    DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions, DesktopFrame,
};
use libwebrtc::native::yuv_helper;
use libwebrtc::prelude::*;
use libwebrtc::video_source::native::NativeVideoSource;
use serde::Serialize;


/// Gönderilen görüntünün en büyük genişliği (1080p).
const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;
pub const FPS: u64 = 30;

#[derive(Serialize, Clone)]
pub struct SourceInfo {
    pub id: u64,
    pub title: String,
    pub kind: String, // "screen" | "window"
}

fn source_type(kind: &str) -> DesktopCaptureSourceType {
    if kind == "window" {
        DesktopCaptureSourceType::Window
    } else {
        DesktopCaptureSourceType::Screen
    }
}

fn options(kind: &str) -> DesktopCapturerOptions {
    #[allow(unused_mut)]
    let mut o = DesktopCapturerOptions::new(source_type(kind));
    o.set_include_cursor(true);
    #[cfg(target_os = "macos")]
    o.set_sck_system_picker(false);
    o
}

pub fn list_sources() -> Vec<SourceInfo> {
    let mut out = Vec::new();
    for kind in ["screen", "window"] {
        if let Some(cap) = DesktopCapturer::new(options(kind)) {
            for (i, s) in cap.get_source_list().into_iter().enumerate() {
                let title = if s.title().is_empty() {
                    format!("Ekran {}", i + 1)
                } else {
                    s.title()
                };
                out.push(SourceInfo { id: s.id(), title, kind: kind.to_string() });
            }
        }
    }
    out
}

/// Çalışan bir ekran yakalama. Drop edildiğinde durur.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(source_id: u64, kind: String, video: NativeVideoSource) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();

        // DesktopCapturer kendi thread'inde yaşar.
        let thread = std::thread::Builder::new()
            .name("screen-capture".into())
            .spawn(move || {
                let Some(mut cap) = DesktopCapturer::new(options(&kind)) else {
                    let _ = init_tx.send(Err("Ekran yakalayıcı oluşturulamadı".into()));
                    return;
                };
                let source = cap.get_source_list().into_iter().find(|s| s.id() == source_id);
                if source.is_none() {
                    let _ = init_tx.send(Err("Seçilen ekran/pencere bulunamadı".into()));
                    return;
                }
                let start = Instant::now();
                cap.start_capture(source, move |res| match res {
                    Ok(frame) => push_frame(&video, &frame, start),
                    Err(e) => log::debug!("capture error: {e:?}"),
                });
                let _ = init_tx.send(Ok(()));

                let interval = Duration::from_millis(1000 / FPS);
                while !stop2.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    cap.capture_frame();
                    if let Some(rest) = interval.checked_sub(t.elapsed()) {
                        std::thread::sleep(rest);
                    }
                }
            })?;

        init_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow!("Ekran yakalama başlatılamadı (zaman aşımı)"))?
            .map_err(|e| anyhow!(e))?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn push_frame(video: &NativeVideoSource, frame: &DesktopFrame, start: Instant) {
    // I420 için genişlik/yükseklik çift olmalı.
    let w = (frame.width() as u32) & !1;
    let h = (frame.height() as u32) & !1;
    if w == 0 || h == 0 {
        return;
    }
    let mut buf = I420Buffer::new(w, h);
    let (sy, su, sv) = buf.strides();
    let (y, u, v) = buf.data_mut();
    // WebRTC DesktopFrame BGRA'dır; libyuv bunu "ARGB" olarak adlandırır.
    yuv_helper::argb_to_i420(frame.data(), frame.stride(), y, sy, u, su, v, sv, w as i32, h as i32);

    if w > MAX_WIDTH || h > MAX_HEIGHT {
        let scale = f64::min(MAX_WIDTH as f64 / w as f64, MAX_HEIGHT as f64 / h as f64);
        let nw = ((w as f64 * scale) as i32) & !1;
        let nh = ((h as f64 * scale) as i32) & !1;
        buf = buf.scale(nw, nh);
    }

    let vf = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us: start.elapsed().as_micros() as i64,
        frame_metadata: None,
        buffer: buf,
    };
    video.capture_frame(&vf);
}

/// Gelen bir video karesini JPEG'e çevirir (arayüzde göstermek için).
/// Uzak ekran karesini RGBA piksellere çevirir (arayüzde doku olarak çizilir).
pub fn frame_to_rgba(frame: &BoxVideoFrame) -> Option<(usize, usize, Vec<u8>)> {
    let i420 = frame.buffer.to_i420();
    let (w, h) = (i420.width(), i420.height());
    if w == 0 || h == 0 {
        return None;
    }
    let (sy, su, sv) = i420.strides();
    let (y, u, v) = i420.data();
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    // libyuv "ABGR" = bellekte R,G,B,A sırası.
    yuv_helper::i420_to_abgr(y, sy, u, su, v, sv, &mut rgba, w * 4, w as i32, h as i32);
    Some((w as usize, h as usize, rgba))
}
