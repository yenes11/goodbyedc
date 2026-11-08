//! Renkler, yazı tipleri ve egui görünüm ayarları.

use eframe::egui::{
    self, epaint::text::VariationCoords, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Margin, Shadow, Stroke, TextStyle,
};

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const BG_SIDEBAR: Color32 = hex(0x252627);
pub const BG_MAIN: Color32 = hex(0x191919);
pub const BG_INPUT: Color32 = hex(0x141414);
pub const BG_ELEV: Color32 = hex(0x262626);
pub const HOVER: Color32 = hex(0x2e2f30);
pub const ACTIVE: Color32 = hex(0x36373a);
pub const BORDER: Color32 = hex(0x3b3c3d);
pub const TEXT: Color32 = hex(0xe3e3e3);
pub const TEXT_STRONG: Color32 = hex(0xefefef);
pub const MUTED: Color32 = hex(0xa1a1a2);
pub const ACCENT: Color32 = hex(0x2373f4);
pub const ACCENT_HOVER: Color32 = hex(0x1a62d6);
pub const GREEN: Color32 = hex(0x3fb36b);
pub const RED: Color32 = hex(0xe5534b);

/// Kalın yazı için ayrı yazı tipi ailesi.
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn bold_font(size: f32) -> FontId {
    FontId::new(size, bold())
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Avatar rengi: kullanıcı adından sabit bir renk.
pub fn avatar_color(name: &str) -> Color32 {
    const COLORS: [Color32; 8] = [
        hex(0x2373f4),
        hex(0x3fa66b),
        hex(0xd08a2c),
        hex(0xc9524b),
        hex(0xb45fa6),
        hex(0x3f9ea0),
        hex(0x8a7fd1),
        hex(0xc4825a),
    ];
    let h = name.chars().fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
    COLORS[(h % COLORS.len() as u32) as usize]
}

/// Sistem yazı tipini yükler (macOS: SF Pro, Windows: Segoe UI). Bulamazsa egui'nin
/// kendi yazı tipi kullanılır. İkonlar Phosphor ikon yazı tipinden gelir.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    // (yol, kalın sürüm yolu). Değişken yazı tiplerinde kalınlık "wght" ekseniyle seçilir.
    let candidates: &[(&str, Option<&str>)] = &[
        ("/System/Library/Fonts/SFNS.ttf", None),
        ("C:\\Windows\\Fonts\\SegUIVar.ttf", None),
        ("C:\\Windows\\Fonts\\segoeui.ttf", Some("C:\\Windows\\Fonts\\seguisb.ttf")),
        ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", Some("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf")),
    ];
    let mut regular_name = None;
    let mut bold_name = None;
    for (path, bold_path) in candidates {
        let Ok(data) = std::fs::read(path) else { continue };
        match bold_path {
            None => {
                // Değişken yazı tipi: aynı dosyadan iki kalınlık.
                let mut reg = FontData::from_owned(data.clone());
                reg.tweak.coords = VariationCoords::new([("wght", 400.0)]);
                let mut bol = FontData::from_owned(data);
                bol.tweak.coords = VariationCoords::new([("wght", 640.0)]);
                fonts.font_data.insert("system".into(), reg.into());
                fonts.font_data.insert("system-bold".into(), bol.into());
            }
            Some(bp) => {
                fonts.font_data.insert("system".into(), FontData::from_owned(data).into());
                if let Ok(b) = std::fs::read(bp) {
                    fonts.font_data.insert("system-bold".into(), FontData::from_owned(b).into());
                }
            }
        }
        regular_name = Some("system".to_string());
        if fonts.font_data.contains_key("system-bold") {
            bold_name = Some("system-bold".to_string());
        }
        break;
    }

    let prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut regular = prop.clone();
    if let Some(n) = &regular_name {
        regular.insert(0, n.clone());
    }
    let mut boldf = prop;
    if let Some(n) = bold_name.or(regular_name) {
        boldf.insert(0, n);
    }
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(bold(), boldf);

    // İkonlar her iki aileye de yedek olarak eklenir.
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    let icon_name = fonts
        .families
        .get(&FontFamily::Proportional)
        .and_then(|f| f.iter().find(|n| n.contains("phosphor")).cloned());
    if let Some(icon) = icon_name {
        if let Some(f) = fonts.families.get_mut(&bold()) {
            f.push(icon);
        }
    }
    ctx.set_fonts(fonts);
}

pub fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles = [
            (TextStyle::Small, font(11.5)),
            (TextStyle::Body, font(14.0)),
            (TextStyle::Button, font(14.0)),
            (TextStyle::Heading, bold_font(20.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.window_margin = Margin::same(20);
        style.spacing.scroll.floating = true;
        style.spacing.scroll.bar_width = 8.0;
        style.interaction.selectable_labels = false;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = BG_MAIN;
        v.window_fill = BG_SIDEBAR;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(14);
        v.window_shadow = Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(110) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(90) };
        v.extreme_bg_color = BG_INPUT;
        v.text_edit_bg_color = Some(BG_INPUT);
        v.faint_bg_color = BG_ELEV;
        v.code_bg_color = BG_ELEV;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);

        let r = CornerRadius::same(8);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BG_MAIN;
        w.noninteractive.weak_bg_fill = BG_MAIN;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = BG_ELEV;
        w.inactive.weak_bg_fill = BG_ELEV;
        w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, BORDER);
        w.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = ACTIVE;
        w.active.weak_bg_fill = ACTIVE;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open = w.active.clone();
    });
}
