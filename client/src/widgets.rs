//! Küçük, tekrar kullanılan arayüz parçaları.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, Key, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, ViewportCommand,
};
use egui_phosphor::regular as ph;

use crate::theme::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    /// Kapalı (ör. mikrofon susturulmuş): kırmızı
    Off,
    /// Açık (ör. ekran paylaşılıyor): yeşil
    On,
    /// Tehlikeli işlem: üzerine gelince kırmızı zemin
    Danger,
}

/// Kare ikon düğmesi.
pub fn icon_button(ui: &mut Ui, icon: &str, tooltip: &str, tone: Tone) -> Response {
    let size = Vec2::splat(32.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    let (bg, fg) = match tone {
        Tone::Danger if hovered => (RED, Color32::WHITE),
        Tone::On => (GREEN.gamma_multiply(0.18), GREEN),
        Tone::Off => (if hovered { HOVER } else { Color32::TRANSPARENT }, RED),
        _ if hovered => (HOVER, TEXT_STRONG),
        Tone::Danger => (Color32::TRANSPARENT, TEXT),
        _ => (Color32::TRANSPARENT, MUTED),
    };
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(8), bg);
    p.text(rect.center(), Align2::CENTER_CENTER, icon, icon_font(18.0), fg);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text(tooltip)
}

/// Küçük ikon düğmesi (kategori "+" ve kanal silme gibi).
pub fn small_icon_button(ui: &mut Ui, icon: &str, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
    let fg = if resp.hovered() { TEXT_STRONG } else { MUTED };
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, icon_font(14.0), fg);
    resp.on_hover_text(tooltip)
}

/// Mavi ana düğme.
pub fn accent_button(ui: &mut Ui, text: &str, width: f32, enabled: bool) -> Response {
    let height = 36.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), if enabled { Sense::click() } else { Sense::hover() });
    let bg = if !enabled {
        ACCENT.gamma_multiply(0.45)
    } else if resp.hovered() {
        ACCENT_HOVER
    } else {
        ACCENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, bold_font(14.0), Color32::WHITE);
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Gri ikincil düğme.
pub fn secondary_button(ui: &mut Ui, text: &str, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 36.0), Sense::click());
    let bg = if resp.hovered() { HOVER } else { BG_ELEV };
    ui.painter().rect(rect, CornerRadius::same(8), bg, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font(14.0), TEXT_STRONG);
    resp
}

/// Yuvarlak "hap" düğme (Güncelle gibi).
pub fn pill_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), bold_font(13.0), Color32::WHITE);
    let size = egui::vec2(galley.size().x + 26.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let bg = if !enabled {
        ACCENT.gamma_multiply(0.5)
    } else if resp.hovered() {
        ACCENT_HOVER
    } else {
        ACCENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(13), bg);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
    resp
}

/// Baş harfli yuvarlak avatar. `speaking` ise yeşil halka çizilir.
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

/// Bir alanı pencere sürükleme bölgesi yapar (çift tıklayınca büyüt/küçült).
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

/// Windows/Linux: küçült, büyüt, kapat düğmeleri.
pub fn window_controls(ui: &mut Ui, height: f32) {
    let ctx = ui.ctx().clone();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let buttons = [
        (ph::MINUS, "Küçült", 0),
        (if maximized { ph::COPY } else { ph::SQUARE }, if maximized { "Önceki boyut" } else { "Büyüt" }, 1),
        (ph::X, "Kapat", 2),
    ];
    // Sağdan sola düzende ilk eklenen en sağa gelir: Kapat en sağda kalsın diye sırayı çevir.
    let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
    let order: Vec<_> = if rtl { buttons.into_iter().rev().collect() } else { buttons.into_iter().collect() };
    for (icon, tip, kind) in order {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(46.0, height), Sense::click());
        let (bg, fg) = match (kind, resp.hovered()) {
            (2, true) => (Color32::from_rgb(0xe8, 0x11, 0x23), Color32::WHITE),
            (_, true) => (HOVER, TEXT_STRONG),
            _ => (Color32::TRANSPARENT, MUTED),
        };
        ui.painter().rect_filled(rect, 0.0, bg);
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

/// Çerçevesiz pencerede kenarlardan boyutlandırma (Windows/Linux).
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

/// Enter'a basıldı mı (odak bu alandayken)?
pub fn enter_pressed(ui: &Ui, resp: &Response) -> bool {
    resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
}

/// Küçük, kalın, büyük harfli başlık ("METİN KANALLARI").
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text.to_uppercase()).font(bold_font(11.5)).color(MUTED));
}
