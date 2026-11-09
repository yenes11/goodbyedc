//! Renkler, yazı tipleri ve egui görünüm ayarları.

use eframe::egui::{
    self,
    epaint::text::{HintingTarget, SmoothHinting, VariationCoords}, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Margin, Shadow, Stroke, TextStyle,
};

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

// Renkler: Claude masaüstü uygulamasının koyu teması, vurgu rengi mavi (#2373F4).
// (Biçim HeroUI tarzında: hap butonlar, büyük yuvarlaklık, çerçevesiz dolgulu yüzeyler.)
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
/// Modal, kart ve bildirim yüzeyi.
pub const SURFACE: Color32 = hex(0x252627);

/// Köşe yuvarlaklıkları (HeroUI radius: small 8, medium 12, large 14).
pub const R_SM: u8 = 12; // düğme, input (medium)
pub const R_LG: u8 = 14; // modal, kart (large)

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

/// İkonlar için ayrı aile: Inter, Phosphor'un kullandığı özel kod noktalarında
/// kendi harflerini taşıdığı için ikonlar normal ailede yanlış görünüyordu.
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
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

/// Yazı tiplerini yükler. macOS'ta sistem yazı tipi (SF Pro) keskin göründüğü için
/// o kullanılır; Windows/Linux'ta uygulamaya gömülü Inter kullanılır (Segoe UI'nin
/// satır ölçüleri yazıları dikey olarak kaydırıyordu). İkonlar Phosphor'dan gelir.
///
/// `GOODBYEDC_FONT=inter` macOS'ta da Inter'i dener.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let force_inter = std::env::var("GOODBYEDC_FONT").is_ok_and(|v| v == "inter");
    let sf = if cfg!(target_os = "macos") && !force_inter { std::fs::read("/System/Library/Fonts/SFNS.ttf").ok() } else { None };
    match sf {
        Some(data) => {
            // Değişken yazı tipi: aynı dosyadan iki kalınlık.
            let mut reg = FontData::from_owned(data.clone());
            reg.tweak.coords = VariationCoords::new([("wght", 400.0)]);
            let mut bol = FontData::from_owned(data);
            bol.tweak.coords = VariationCoords::new([("wght", 640.0)]);
            fonts.font_data.insert("ui".into(), reg.into());
            fonts.font_data.insert("ui-bold".into(), bol.into());
        }
        None => {
            fonts.font_data.insert("ui".into(), crisp(FontData::from_static(include_bytes!("../fonts/Inter-Regular.ttf"))).into());
            fonts.font_data.insert("ui-bold".into(), crisp(FontData::from_static(include_bytes!("../fonts/Inter-SemiBold.ttf"))).into());
        }
    }

    let prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut regular = prop.clone();
    regular.insert(0, "ui".into());
    let mut boldf = prop;
    boldf.insert(0, "ui-bold".into());
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
    if let Some(icon) = fonts.families.get(&bold()).and_then(|f| f.iter().find(|n| n.contains("phosphor")).cloned()) {
        fonts.families.insert(FontFamily::Name("icons".into()), vec![icon, "ui".into()]);
    }
    ctx.set_fonts(fonts);
}

/// Düşük DPI ekranlarda (Windows) keskin yazı: harf kenarlarını piksellere oturt.
fn crisp(mut f: FontData) -> FontData {
    f.tweak.hinting = Some(true);
    f.tweak.subpixel_binning = Some(false);
    f.tweak.hinting_target =
        HintingTarget::Smooth(SmoothHinting { light: false, symmetric_rendering: false, preserve_linear_metrics: false });
    f
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
        v.window_fill = SURFACE;
        v.window_stroke = Stroke::NONE;
        v.window_corner_radius = CornerRadius::same(R_LG);
        v.menu_corner_radius = CornerRadius::same(R_SM);
        v.window_shadow = Shadow { offset: [0, 12], blur: 36, spread: 0, color: Color32::from_black_alpha(160) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(150) };
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.extreme_bg_color = BG_INPUT;
        v.text_edit_bg_color = Some(BG_INPUT);
        v.faint_bg_color = BG_ELEV;
        v.code_bg_color = BG_ELEV;
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        // Odaklanan input: HeroUI'de çerçeve yok; hafif bir mavi kenar yeterli.
        v.selection.stroke = Stroke::new(1.5, ACCENT.gamma_multiply(0.7));
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);

        // Inputlar (ve egui'nin kendi çizdiği diğer öğeler) hap şeklinde.
        let r = CornerRadius::same(20);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BG_MAIN;
        w.noninteractive.weak_bg_fill = BG_MAIN;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER); // ayırıcılar
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = ACTIVE; // slider rayı vb.
        w.inactive.weak_bg_fill = Color32::TRANSPARENT; // düğmeler: outline
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::NONE;
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
