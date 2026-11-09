//! Small, reusable UI pieces (buttons, avatar, spinner, window controls, ...).
//!
//! How a custom egui widget works (immediate mode):
//!   1. Reserve space: `ui.allocate_exact_size(size, Sense::click())` returns the
//!      `Rect` we got and a `Response` describing this frame's interaction
//!      (hovered? clicked? dragged?).
//!   2. Paint: `ui.painter()` draws shapes/text inside that rect.
//!   3. Return the `Response` so the caller can write `if widget(...).clicked() {}`.
//! Everything runs again next frame; egui remembers nothing about the widget
//! except small bits of state keyed by its id (like animation progress).

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Key, Rect, Response, Sense, Stroke, Ui, Vec2, ViewportCommand,
};
use egui_phosphor::regular as ph;

use crate::theme::*;

/// Visual variant of `icon_button`.
/// Deriving `PartialEq, Eq` lets us compare tones with `==`; `Copy` lets us pass
/// them around by value without `.clone()`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    /// "Off" (e.g. microphone muted): drawn in red.
    Off,
}

/// Round icon-only button (32x32), e.g. microphone / headphones / settings.
///
/// Parameters are borrowed: `&mut Ui` (we add things to the UI) and `&str`
/// (we only read the texts).
pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str, tone: Tone) -> Response {
    let size = Vec2::splat(32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let s = press_scale(ui, &resp);
    let rect = scaled(rect, s);
    let hovered = resp.hovered();
    let (bg, fg) = match tone {
        Tone::Off => (if hovered { HOVER } else { Color32::TRANSPARENT }, RED),
        _ if hovered => (HOVER, TEXT_STRONG),
        _ => (Color32::TRANSPARENT, MUTED),
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(16), bg);
    p.text(rect.center(), Align2::CENTER_CENTER, icon, icon_font(17.0 * s), fg);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tooltip)
}

/// Small icon button (the "+" next to channel categories).
pub fn small_icon_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let rect = scaled(rect, press_scale(ui, &resp));
    let fg = if resp.hovered() { TEXT_STRONG } else { MUTED };
    if resp.hovered() {
        ui.painter().circle_filled(rect.center(), rect.width() / 2.0, HOVER);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, icon_font(14.0), fg);
    resp.on_hover_text(tooltip)
}

/// Blue primary button (HeroUI "solid" primary): pill-shaped, shrinks slightly
/// while pressed.
/// While `loading`, a spinner shows next to the text and it can't be clicked.
pub fn accent_button(ui: &mut Ui, text: &str, width: f32, enabled: bool, loading: bool) -> Response {
    solid_button(ui, text, width, enabled, loading, ACCENT, ACCENT_HOVER)
}

/// Red button for destructive actions.
/// HeroUI "danger" color: lab(50.91% 62.27 37.59) = #DB3B3E.
/// On hover it is mixed with 10% white in oklab (#E15451).
pub fn danger_button(ui: &mut Ui, text: &str, width: f32) -> Response {
    solid_button(ui, text, width, true, false, Color32::from_rgb(0xdb, 0x3b, 0x3e), Color32::from_rgb(0xe1, 0x54, 0x51))
}

/// Height shared by all text buttons.
pub const BUTTON_H: f32 = 36.0;

/// Shared implementation of the solid (filled) buttons. Private (no `pub`),
/// so other files use `accent_button` / `danger_button` instead.
fn solid_button(
    ui: &mut Ui,
    text: &str,
    width: f32,
    enabled: bool,
    loading: bool,
    color: Color32,
    hover: Color32,
) -> Response {
    let clickable = enabled && !loading;
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(width, BUTTON_H), if clickable { Sense::click() } else { Sense::hover() });
    let s = if clickable { press_scale(ui, &resp) } else { 1.0 };
    let rect = scaled(rect, s);
    let bg = if !enabled || loading {
        color.gamma_multiply(0.5)
    } else if resp.hovered() {
        hover
    } else {
        color
    };
    let fg = if !enabled { Color32::from_white_alpha(140) } else { Color32::WHITE };
    ui.painter().rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), bg);
    label_with_spinner(ui, rect, text, font(14.0 * s), fg, loading);
    if clickable && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// How much a button shrinks while pressed (HeroUI style: down to ~96%, then
/// springs back). `animate_bool_with_time` gives a value that moves smoothly
/// from 0 to 1 (and back) over 0.08 s whenever `down` changes; egui stores the
/// animation state under the widget's id.
pub fn press_scale(ui: &Ui, resp: &Response) -> f32 {
    let down = resp.is_pointer_button_down_on();
    let t = ui.ctx().animate_bool_with_time(resp.id.with("press"), down, 0.08);
    1.0 - 0.04 * t
}

/// Scales a rectangle around its center.
pub fn scaled(rect: Rect, s: f32) -> Rect {
    Rect::from_center_size(rect.center(), rect.size() * s)
}

/// Draws text centered in `rect`; with `loading`, a small spinner sits to its left.
fn label_with_spinner(ui: &Ui, rect: Rect, text: &str, font: egui::FontId, color: Color32, loading: bool) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let gap = if loading { 14.0 + 8.0 } else { 0.0 };
    let total = galley.size().x + gap;
    let left = rect.center().x - total / 2.0;
    if loading {
        let r = Rect::from_center_size(egui::pos2(left + 7.0, rect.center().y), Vec2::splat(14.0));
        spin(ui, r, color);
    }
    ui.painter().galley(egui::pos2(left + gap, rect.center().y - galley.size().y / 2.0), galley, color);
}

/// The spinner: a fixed-length arc rotating over a faint ring.
/// It asks for a repaint every frame while visible, so the animation keeps going.
pub fn spin(ui: &Ui, rect: Rect, color: Color32) {
    ui.ctx().request_repaint();
    let r = rect.width().min(rect.height()) / 2.0 - 1.5;
    let w = (r * 0.28).clamp(1.5, 3.0);
    let c = rect.center();
    ui.painter().circle_stroke(c, r, Stroke::new(w, color.gamma_multiply(0.2)));
    let t = ui.input(|i| i.time) as f32;
    let start = t * std::f32::consts::TAU * 1.1;
    let n = 24;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let a = start + (i as f32 / n as f32) * std::f32::consts::FRAC_PI_2 * 1.4;
            c + r * egui::vec2(a.cos(), a.sin())
        })
        .collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(w, color)));
}

/// A spinner that takes up space in the layout.
pub fn spinner(ui: &mut Ui, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    spin(ui, rect, color);
    resp
}

/// A spinner in a button-sized slot (shown instead of an icon button while busy).
pub fn spinner_slot(ui: &mut Ui, size: f32, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    spin(ui, Rect::from_center_size(rect.center(), Vec2::splat(16.0)), MUTED);
    resp.on_hover_text(tooltip)
}

/// A spinner centered in the available area, with a caption below it.
pub fn loading_center(ui: &mut Ui, text: &str) {
    let rect = ui.available_rect_before_wrap();
    paint_loading(ui, rect, text);
    ui.allocate_rect(rect, Sense::hover());
}

pub fn paint_loading(ui: &Ui, rect: Rect, text: &str) {
    let c = rect.center();
    spin(ui, Rect::from_center_size(egui::pos2(c.x, c.y - 14.0), Vec2::splat(26.0)), ACCENT);
    ui.painter().text(egui::pos2(c.x, c.y + 18.0), Align2::CENTER_CENTER, text, font(13.5), MUTED);
}

/// Secondary button (HeroUI "flat"): gray fill, no border.
pub fn secondary_button(ui: &mut Ui, text: &str, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, BUTTON_H), Sense::click());
    let s = press_scale(ui, &resp);
    let rect = scaled(rect, s);
    let bg = if resp.hovered() { ACTIVE } else { HOVER };
    ui.painter().rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font(14.0 * s), TEXT_STRONG);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Small pill button (like "Güncelle"). While `loading`, a spinner shows inside.
pub fn pill_button(ui: &mut Ui, text: &str, loading: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), bold_font(13.0), Color32::WHITE);
    let extra = if loading { 20.0 } else { 0.0 };
    let size = egui::vec2(galley.size().x + 24.0 + extra, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, if loading { Sense::hover() } else { Sense::click() });
    let rect = scaled(rect, press_scale(ui, &resp));
    let bg = if loading {
        ACCENT.gamma_multiply(0.7)
    } else if resp.hovered() {
        ACCENT_HOVER
    } else {
        ACCENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
    label_with_spinner(ui, rect, text, bold_font(13.0), Color32::WHITE, loading);
    resp
}

/// Round avatar with the name's first letter. If `speaking`, a green ring is drawn.
pub fn avatar(ui: &mut Ui, name: &str, size: f32, speaking: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_avatar(ui, rect, name, speaking);
    resp
}

pub fn paint_avatar(ui: &Ui, rect: Rect, name: &str, speaking: bool) {
    let p = ui.painter();
    let r = rect.width() / 2.0;
    p.circle_filled(rect.center(), r, avatar_color(name));
    let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    p.text(rect.center(), Align2::CENTER_CENTER, initial, bold_font(r * 0.95), Color32::WHITE);
    if speaking {
        p.circle_stroke(rect.center(), r + 1.5, Stroke::new(2.0, GREEN));
    }
}

/// Makes an area drag the window (our frameless window has no title bar of its
/// own). Double-click toggles maximize.
pub fn drag_region(ui: &Ui, rect: Rect, id: &str) {
    let resp = ui.interact(rect, ui.id().with(id), Sense::click_and_drag());
    if resp.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!max));
    }
}

/// Windows/Linux: minimize, maximize and close buttons (we draw our own title bar).
pub fn window_controls(ui: &mut Ui, height: f32) {
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let buttons = [
        (ph::MINUS, "Küçült", 0),
        (if maximized { ph::COPY } else { ph::SQUARE }, if maximized { "Önceki boyut" } else { "Büyüt" }, 1),
        (ph::X, "Kapat", 2),
    ];
    // In a right-to-left layout the first item added ends up rightmost, so reverse
    // the order to keep "Close" on the far right.
    let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
    let order: Vec<_> = if rtl { buttons.into_iter().rev().collect() } else { buttons.into_iter().collect() };
    for (icon, tip, kind) in order {
        let (slot, resp) = ui.allocate_exact_size(egui::vec2(40.0, height), Sense::click());
        let (bg, fg) = match (kind, resp.hovered()) {
            (2, true) => (Color32::from_rgb(0xe8, 0x11, 0x23), Color32::WHITE),
            (_, true) => (HOVER, TEXT_STRONG),
            _ => (Color32::TRANSPARENT, MUTED),
        };
        // Consistent with the other buttons: the hover background is a pill.
        let rect = scaled(Rect::from_center_size(slot.center(), egui::vec2(36.0, 28.0)), press_scale(ui, &resp));
        ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, icon_font(15.0), fg);
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            match kind {
                0 => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                1 => ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized)),
                _ => ctx.send_viewport_cmd(ViewportCommand::Close),
            }
        }
    }
}

/// Resizing a frameless window by dragging its edges (Windows/Linux).
/// When the pointer is within 5 px of an edge we show a resize cursor, and on
/// mouse-down ask the OS to start a native resize in that direction.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn resize_edges(ctx: &egui::Context) {
    use egui::{CursorIcon, ResizeDirection as D};
    if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let screen = ctx.content_rect();
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let m = 5.0;
    let left = pos.x <= screen.left() + m;
    let right = pos.x >= screen.right() - m;
    let top = pos.y <= screen.top() + m;
    let bottom = pos.y >= screen.bottom() - m;
    let dir = match (left, right, top, bottom) {
        (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNorthWest)),
        (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNorthEast)),
        (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeSouthWest)),
        (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeSouthEast)),
        (true, ..) => Some((D::West, CursorIcon::ResizeWest)),
        (_, true, ..) => Some((D::East, CursorIcon::ResizeEast)),
        (_, _, true, _) => Some((D::North, CursorIcon::ResizeNorth)),
        (_, _, _, true) => Some((D::South, CursorIcon::ResizeSouth)),
        _ => None,
    };
    if let Some((d, cursor)) = dir {
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(d));
        }
    }
}

/// Was Enter pressed while this text field had focus?
pub fn enter_pressed(ui: &Ui, resp: &Response) -> bool {
    resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
}

/// Group title in the sidebar (like shadcn's SidebarGroupLabel: small, muted).
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(bold_font(12.0)).color(MUTED));
}

/// Form field label (like shadcn's Label: text-sm, font-medium).
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).font(bold_font(13.5)).color(TEXT_STRONG));
}

/// On/off switch. `on: &mut bool` lets the switch flip the caller's variable directly.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(46.0, 28.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let bg = if *on { ACCENT } else { ACTIVE };
    ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
    let x = egui::lerp(rect.left() + 14.0..=rect.right() - 14.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y + 1.0), 10.0, Color32::from_black_alpha(60));
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 10.0, Color32::WHITE);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Input sensitivity bar like Discord's: live microphone level plus a draggable
/// threshold handle.
/// Values are in dB (-100..0).
pub fn sensitivity_bar(ui: &mut Ui, threshold_db: &mut f32, level_db: f32, editable: bool) -> Response {
    let (rect, mut resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), if editable { Sense::click_and_drag() } else { Sense::hover() });
    let to_x = |db: f32| egui::lerp(rect.left()..=rect.right(), (db + 100.0) / 100.0);
    if editable {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let v = (t * 100.0 - 100.0).round();
            if v != *threshold_db {
                *threshold_db = v;
                resp.mark_changed();
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
    let bar = Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 10.0));
    let p = ui.painter();
    let tx = to_x(*threshold_db).clamp(bar.left(), bar.right());
    let amber = Color32::from_rgb(0xd8, 0xa3, 0x3a);
    // Left of the threshold (not transmitted) is amber, right (transmitted) is green; faint background.
    p.rect_filled(Rect::from_min_max(bar.min, egui::pos2(tx, bar.max.y)), CornerRadius::same(5), amber.gamma_multiply(0.25));
    p.rect_filled(Rect::from_min_max(egui::pos2(tx, bar.min.y), bar.max), CornerRadius::same(5), GREEN.gamma_multiply(0.25));
    // Live level
    let lx = to_x(level_db).clamp(bar.left(), bar.right());
    if lx > bar.left() + 1.0 {
        let left_end = lx.min(tx);
        p.rect_filled(Rect::from_min_max(bar.min, egui::pos2(left_end, bar.max.y)), CornerRadius::same(5), amber);
        if lx > tx {
            p.rect_filled(Rect::from_min_max(egui::pos2(tx, bar.min.y), egui::pos2(lx, bar.max.y)), CornerRadius::same(5), GREEN);
        }
    }
    // Threshold handle
    let handle = Rect::from_center_size(egui::pos2(tx, rect.center().y), egui::vec2(6.0, 22.0));
    let hc = if editable { Color32::WHITE } else { MUTED };
    p.rect_filled(handle, CornerRadius::same(3), hc);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    resp
}

/// HeroUI-style modal header: large title on the left, round close button on the
/// top right, muted description below. Returns `true` if the close button was clicked.
pub fn modal_header(ui: &mut Ui, title: &str, desc: &str) -> bool {
    let desc = (!desc.is_empty()).then(|| egui::RichText::new(desc).size(14.5).color(MUTED).into());
    modal_header_rich(ui, title, desc)
}

/// Modal header with a formatted description (e.g. some words in bold).
/// `egui::WidgetText` can hold either plain/rich text or a pre-built LayoutJob.
pub fn modal_header_rich(ui: &mut Ui, title: &str, desc: Option<egui::WidgetText>) -> bool {
    let mut close = false;
    let w = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(w, 34.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(title).font(bold_font(20.0)).color(TEXT_STRONG));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
            let s = press_scale(ui, &resp);
            let bg = if resp.hovered() { ACTIVE } else { HOVER };
            ui.painter().circle_filled(rect.center(), 16.0 * s, bg);
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, ph::X, icon_font(15.0 * s), MUTED);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            close = resp.on_hover_text("Kapat").clicked();
        });
    });
    if let Some(d) = desc {
        ui.add_space(8.0);
        ui.add(egui::Label::new(d).wrap());
    }
    ui.add_space(20.0);
    close
}

/// Button width that fits the text (18 px padding on each side, minimum 80).
pub fn button_width(ui: &Ui, text: &str) -> f32 {
    let g = ui.painter().layout_no_wrap(text.to_string(), font(14.0), Color32::WHITE);
    (g.size().x + 36.0).max(80.0)
}

/// Labelled pill button ("Ekran", "Ses" in the voice panel): icon + text, filled.
/// Green-tinted while `on`; a spinner replaces the icon while `loading`.
pub fn action_pill(ui: &mut Ui, icon: &str, label: &str, width: f32, on: bool, loading: bool) -> Response {
    let (slot, resp) = ui.allocate_exact_size(egui::vec2(width, 32.0), if loading { Sense::hover() } else { Sense::click() });
    let s = if loading { 1.0 } else { press_scale(ui, &resp) };
    let rect = scaled(slot, s);
    let (bg, fg) = if on {
        (GREEN.gamma_multiply(if resp.hovered() { 0.26 } else { 0.16 }), GREEN)
    } else if resp.hovered() {
        (ACTIVE, TEXT_STRONG)
    } else {
        (HOVER, TEXT)
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(16), bg);
    let text = p.layout_no_wrap(label.to_string(), bold_font(13.0 * s), fg);
    let total = 16.0 + 6.0 + text.size().x;
    let x = rect.center().x - total / 2.0;
    let icon_c = egui::pos2(x + 8.0, rect.center().y);
    if loading {
        spin(ui, Rect::from_center_size(icon_c, Vec2::splat(14.0)), fg);
    } else {
        p.text(icon_c, Align2::CENTER_CENTER, icon, icon_font(15.0 * s), fg);
    }
    p.galley(egui::pos2(x + 22.0, rect.center().y - text.size().y / 2.0), text, fg);
    if !loading && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Red-tinted round button (disconnect): always red, solid red on hover.
pub fn danger_circle_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    let (slot, resp) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    let rect = scaled(slot, press_scale(ui, &resp));
    let (bg, fg) = if resp.hovered() { (RED, Color32::WHITE) } else { (RED.gamma_multiply(0.18), RED) };
    ui.painter().circle_filled(rect.center(), rect.width() / 2.0, bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, icon_font(17.0), fg);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tooltip)
}
