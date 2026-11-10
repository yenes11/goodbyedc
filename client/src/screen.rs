//! Screen capture (sending side) and converting incoming screen frames into
//! pixels for the UI (watching side).
//!
//! Video in WebRTC is usually in "I420" format (a YUV format: one brightness plane
//! plus two smaller color planes). Screens give us BGRA pixels, so we convert
//! BGRA -> I420 when sending and I420 -> RGBA when displaying, using libyuv
//! (a fast C library bundled with libwebrtc).

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


/// Largest size we send (1080p); bigger screens are scaled down.
const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;
/// Frames per second we capture.
pub const FPS: u64 = 30;

/// A screen or window the user can share.
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
    // `mut` is only needed on macOS (the extra line below); `allow(unused_mut)`
    // silences the warning on other systems.
    #[allow(unused_mut)]
    let mut o = DesktopCapturerOptions::new(source_type(kind));
    o.set_include_cursor(true);
    // Don't show macOS's own screen picker; we have our own list.
    #[cfg(target_os = "macos")]
    o.set_sck_system_picker(false);
    o
}

/// All screens and windows that can be shared.
pub fn list_sources() -> Vec<SourceInfo> {
    let mut out = Vec::new();
    for kind in ["screen", "window"] {
        if let Some(cap) = DesktopCapturer::new(options(kind)) {
            for (i, s) in cap.get_source_list().into_iter().enumerate() {
                // Screens often have no title: name them "Ekran 1", "Ekran 2", ...
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

/// A running screen capture. Stops when dropped.
pub struct Capture {
    stop: Arc<AtomicBool>,
    /// `Option` so that `Drop` can `take()` the handle out and join the thread.
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Starts capturing `source_id` and feeding frames into `video` (the source of
    /// the WebRTC video track we're sending).
    pub fn start(source_id: u64, kind: String, video: NativeVideoSource) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        // The capture thread reports whether starting worked.
        let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();

        // The DesktopCapturer lives on its own thread for its whole life.
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
                // The callback runs for every captured frame. `match res` handles both
                // the Ok (frame) and Err cases inline.
                cap.start_capture(source, move |res| match res {
                    Ok(frame) => push_frame(&video, &frame, start),
                    Err(e) => log::debug!("capture error: {e:?}"),
                });
                let _ = init_tx.send(Ok(()));

                // Ask for a new frame FPS times per second until stopped.
                let interval = Duration::from_millis(1000 / FPS);
                while !stop2.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    cap.capture_frame();
                    // Sleep for the rest of the frame interval. `checked_sub`
                    // returns None instead of underflowing if capturing took longer.
                    if let Some(rest) = interval.checked_sub(t.elapsed()) {
                        std::thread::sleep(rest);
                    }
                }
            })?;

        // Wait (max 10 s) for the thread's verdict. Two error layers: timeout, and
        // the String error sent by the thread (converted with `anyhow!(e)`).
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
        // Wait for the capture thread to finish so the capturer is fully closed.
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Windows: the process id of the app that owns a window. For window capture,
/// WebRTC's source id *is* the window handle (HWND), so we can ask Windows who
/// owns it. Used to share that app's sound along with its window.
#[cfg(windows)]
pub fn window_pid(source_id: u64) -> Option<u32> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let mut pid = 0u32;
    // `unsafe`: a Windows API call. A stale handle just gives pid 0 (no crash).
    unsafe { GetWindowThreadProcessId(HWND(source_id as isize), Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// Other systems: no per-app audio capture, so there's nothing to look up.
#[cfg(not(windows))]
pub fn window_pid(_source_id: u64) -> Option<u32> {
    None
}

/// Converts one captured frame to I420, scales it down if needed and hands it
/// to WebRTC.
fn push_frame(video: &NativeVideoSource, frame: &DesktopFrame, start: Instant) {
    // I420 needs even width/height. `& !1` clears the lowest bit (rounds down to even).
    let w = (frame.width() as u32) & !1;
    let h = (frame.height() as u32) & !1;
    if w == 0 || h == 0 {
        return;
    }
    let mut buf = I420Buffer::new(w, h);
    // Strides = bytes per row of each plane (can be larger than the width).
    let (sy, su, sv) = buf.strides();
    let (y, u, v) = buf.data_mut();
    // WebRTC's DesktopFrame is BGRA in memory; libyuv calls that layout "ARGB".
    yuv_helper::argb_to_i420(frame.data(), frame.stride(), y, sy, u, su, v, sv, w as i32, h as i32);

    if w > MAX_WIDTH || h > MAX_HEIGHT {
        // Keep the aspect ratio: use the smaller of the two scale factors.
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

/// Converts a received remote screen frame into RGBA pixels (drawn in the UI as a
/// texture). Returns `(width, height, pixels)`, or `None` for an empty frame.
pub fn frame_to_rgba(frame: &BoxVideoFrame) -> Option<(usize, usize, Vec<u8>)> {
    let i420 = frame.buffer.to_i420();
    let (w, h) = (i420.width(), i420.height());
    if w == 0 || h == 0 {
        return None;
    }
    let (sy, su, sv) = i420.strides();
    let (y, u, v) = i420.data();
    // 4 bytes (R, G, B, A) per pixel.
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    // libyuv's "ABGR" means R, G, B, A byte order in memory (its names are
    // little-endian word order, which looks reversed).
    yuv_helper::i420_to_abgr(y, sy, u, su, v, sv, &mut rgba, w * 4, w as i32, h as i32);
    Some((w as usize, h as usize, rgba))
}
