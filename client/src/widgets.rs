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

/// Mavi ana düğme. `loading` iken yanında dönen gösterge çıkar ve tıklanamaz.
pub fn accent_button(ui: &mut Ui, text: &str, width: f32, enabled: bool, loading: bool) -> Response {
    let height = 36.0;
    let clickable = enabled && !loading;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), if clickable { Sense::click() } else { Sense::hover() });
    let bg = if !enabled {
        ACCENT.gamma_multiply(0.45)
    } else if loading {
        ACCENT.gamma_multiply(0.8)
    } else if resp.hovered() {
        ACCENT_HOVER
    } else {
        ACCENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    label_with_spinner(ui, rect, text, bold_font(14.0), Color32::WHITE, loading);
    if clickable && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Yazıyı alanın ortasına çizer; `loading` ise solunda küçük dönen gösterge olur.
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

/// Dönen gösterge: soluk bir halka üzerinde sabit uzunlukta dönen yay.
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

/// Yerleşimde yer kaplayan dönen gösterge.
pub fn spinner(ui: &mut Ui, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    spin(ui, rect, color);
    resp
}

/// Dönen gösterge, düğme boyutunda bir yuvada (ikon düğmesinin yerine).
pub fn spinner_slot(ui: &mut Ui, size: f32, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    spin(ui, Rect::from_center_size(rect.center(), Vec2::splat(16.0)), MUTED);
    resp.on_hover_text(tooltip)
}

/// Kullanılabilir alanın ortasında dönen gösterge ve altında açıklama.
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

/// Gri ikincil düğme.
pub fn secondary_button(ui: &mut Ui, text: &str, width: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 36.0), Sense::click());
    let bg = if resp.hovered() { HOVER } else { BG_ELEV };
    ui.painter().rect(rect, CornerRadius::same(8), bg, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, font(14.0), TEXT_STRONG);
    resp
}

/// Yuvarlak "hap" düğme (Güncelle gibi). `loading` iken içinde dönen gösterge olur.
pub fn pill_button(ui: &mut Ui, text: &str, loading: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), bold_font(13.0), Color32::WHITE);
    let extra = if loading { 20.0 } else { 0.0 };
    let size = egui::vec2(galley.size().x + 26.0 + extra, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, if loading { Sense::hover() } else { Sense::click() });
    let bg = if loading {
        ACCENT.gamma_multiply(0.7)
    } else if resp.hovered() {
        ACCENT_HOVER
    } else {
        ACCENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(13), bg);
    label_with_spinner(ui, rect, text, bold_font(13.0), Color32::WHITE, loading);
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
    ui.label(egui::RichText::new(text.replace('i', "İ").to_uppercase()).font(bold_font(11.5)).color(MUTED));
}

/// Açma/kapama anahtarı.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(38.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool(resp.id, *on);
    let bg = if *on { ACCENT } else { ACTIVE };
    ui.painter().rect_filled(rect, CornerRadius::same(11), bg);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, Color32::WHITE);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Discord'daki gibi giriş hassasiyeti çubuğu: anlık mikrofon seviyesi ve
/// sürüklenebilir eşik. Değerler dB (-100..0).
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
    // Eşiğin solu (iletilmez) sarı, sağı (iletilir) yeşil; soluk zemin.
    p.rect_filled(Rect::from_min_max(bar.min, egui::pos2(tx, bar.max.y)), CornerRadius::same(5), amber.gamma_multiply(0.25));
    p.rect_filled(Rect::from_min_max(egui::pos2(tx, bar.min.y), bar.max), CornerRadius::same(5), GREEN.gamma_multiply(0.25));
    // Anlık seviye
    let lx = to_x(level_db).clamp(bar.left(), bar.right());
    if lx > bar.left() + 1.0 {
        let left_end = lx.min(tx);
        p.rect_filled(Rect::from_min_max(bar.min, egui::pos2(left_end, bar.max.y)), CornerRadius::same(5), amber);
        if lx > tx {
            p.rect_filled(Rect::from_min_max(egui::pos2(tx, bar.min.y), egui::pos2(lx, bar.max.y)), CornerRadius::same(5), GREEN);
        }
    }
    // Eşik tutamacı
    let handle = Rect::from_center_size(egui::pos2(tx, rect.center().y), egui::vec2(6.0, 22.0));
    let hc = if editable { Color32::WHITE } else { MUTED };
    p.rect_filled(handle, CornerRadius::same(3), hc);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    resp
}
