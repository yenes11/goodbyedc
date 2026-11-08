//! macOS: pencere düğmelerini (kırmızı/sarı/yeşil) üst çubuğun dikey ortasına yerleştirir.
//!
//! AppKit düğmeleri bazı olaylarda (boyutlandırma, tam ekran) varsayılan yerine
//! döndürdüğü için her karede kontrol edilir; yalnızca yer değiştiyse taşınır.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use objc2_app_kit::{NSView, NSWindowButton};
use objc2_foundation::NSPoint;

pub fn place_traffic_lights(frame: &eframe::Frame, left: f64, bar_height: f64) {
    let Ok(handle) = frame.window_handle() else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    // SAFETY: eframe'in verdiği görünüm pencere açık olduğu sürece geçerlidir ve
    // bu fonksiyon ana iş parçacığında (arayüz döngüsünde) çağrılır.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else { return };
    let buttons = [
        window.standardWindowButton(NSWindowButton::CloseButton),
        window.standardWindowButton(NSWindowButton::MiniaturizeButton),
        window.standardWindowButton(NSWindowButton::ZoomButton),
    ];
    let [Some(close), Some(mini), Some(zoom)] = buttons else { return };

    // Düğmelerin kabı (başlık çubuğu): yüksekliği bizim üst çubuğumuza eşit olsun.
    // SAFETY: görünüm hiyerarşisi ana iş parçacığında okunuyor.
    let Some(container) = (unsafe { close.superview().and_then(|v| v.superview()) }) else { return };
    let win_h = window.frame().size.height;
    let mut cf = container.frame();
    if (cf.size.height - bar_height).abs() > 0.5 || (cf.origin.y - (win_h - bar_height)).abs() > 0.5 {
        cf.size.height = bar_height;
        cf.origin.y = win_h - bar_height;
        container.setFrame(cf);
    }

    let spacing = {
        let d = mini.frame().origin.x - close.frame().origin.x;
        if d > 10.0 { d } else { 20.0 }
    };
    for (i, b) in [close, mini, zoom].iter().enumerate() {
        let bf = b.frame();
        let parent_h = unsafe { b.superview() }.map(|s| s.frame().size.height).unwrap_or(bar_height);
        let target = NSPoint::new(left + i as f64 * spacing, ((parent_h - bf.size.height) / 2.0).round());
        if (bf.origin.x - target.x).abs() > 0.5 || (bf.origin.y - target.y).abs() > 0.5 {
            b.setFrameOrigin(target);
        }
    }
}
