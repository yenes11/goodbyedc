//! macOS: vertically centers the window buttons (red/yellow/green "traffic
//! lights") inside our custom top bar.
//!
//! AppKit moves the buttons back to their default spot after some events
//! (resizing, entering full screen), so this runs every frame and only moves the
//! buttons when they are not where we want them.
//!
//! This file talks to Apple's Objective-C APIs through the `objc2` crates, which
//! is why it contains `unsafe` blocks (see below).

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use objc2_app_kit::{NSView, NSWindowButton};
use objc2_foundation::NSPoint;

/// `left`: x position of the first button; `bar_height`: height of our top bar.
pub fn place_traffic_lights(frame: &eframe::Frame, left: f64, bar_height: f64) {
    // Get the native window handle from eframe. `let Ok(x) = ... else { return };`
    // exits early if something isn't available (e.g. the window isn't ready yet).
    let Ok(handle) = frame.window_handle() else { return };
    // The handle is an enum over all platforms; we only care about the AppKit one.
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };

    // `unsafe` marks code where the compiler can't check memory safety for us and
    // we promise it is correct. Here we turn a raw pointer into a reference.
    // SAFETY: the view given by eframe stays valid while the window is open, and
    // this function is called on the main thread (from the UI loop).
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else { return };
    // An array of three `Option<button>` values.
    let buttons = [
        window.standardWindowButton(NSWindowButton::CloseButton),
        window.standardWindowButton(NSWindowButton::MiniaturizeButton),
        window.standardWindowButton(NSWindowButton::ZoomButton),
    ];
    // Destructure the array; bail out unless all three buttons exist.
    let [Some(close), Some(mini), Some(zoom)] = buttons else { return };

    // The buttons' container (the title bar view): make it as tall as our top bar,
    // anchored to the top of the window. (AppKit's y axis points *up*, so "top" is
    // `window height - bar height`.)
    // SAFETY: we read the view hierarchy on the main thread.
    let Some(container) = (unsafe { close.superview().and_then(|v| v.superview()) }) else { return };
    let win_h = window.frame().size.height;
    let mut cf = container.frame();
    // Only touch it if it's off by more than half a point (avoids redundant work).
    if (cf.size.height - bar_height).abs() > 0.5 || (cf.origin.y - (win_h - bar_height)).abs() > 0.5 {
        cf.size.height = bar_height;
        cf.origin.y = win_h - bar_height;
        container.setFrame(cf);
    }

    // Keep the system's spacing between buttons (fall back to 20 points).
    // A block `{ ... }` is an expression: its last line is its value.
    let spacing = {
        let d = mini.frame().origin.x - close.frame().origin.x;
        if d > 10.0 { d } else { 20.0 }
    };
    // `.enumerate()` yields (index, item) pairs.
    for (i, b) in [close, mini, zoom].iter().enumerate() {
        let bf = b.frame();
        let parent_h = unsafe { b.superview() }.map(|s| s.frame().size.height).unwrap_or(bar_height);
        // `i as f64` converts the integer index to a float (Rust never converts
        // numeric types implicitly).
        let target = NSPoint::new(left + i as f64 * spacing, ((parent_h - bf.size.height) / 2.0).round());
        if (bf.origin.x - target.x).abs() > 0.5 || (bf.origin.y - target.y).abs() > 0.5 {
            b.setFrameOrigin(target);
        }
    }
}
