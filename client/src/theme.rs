//! Colors, fonts and egui's global look ("visuals").
//!
//! egui is an *immediate mode* UI library: instead of creating button objects once
//! and keeping them around, the app redraws the whole UI every frame by calling
//! functions like `ui.label(...)`. The settings in this file tell egui how those
//! built-in widgets should look; our custom widgets (widgets.rs) use the color
//! constants directly.

use eframe::egui::{
    self,
    epaint::text::{HintingTarget, SmoothHinting, VariationCoords}, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
    FontId, Margin, Shadow, Stroke, TextStyle,
};

/// Builds a color from a hex number like `0x2373f4`.
///
/// `const fn` can run at compile time, which is what lets us use it in the
/// `pub const` color definitions below. `>>` shifts bits right and `as u8` keeps
/// the lowest 8 bits, extracting the red, green and blue bytes.
const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

// Colors: the dark theme of the Claude desktop app, with blue (#2373F4) as accent.
// (Shapes follow HeroUI: pill buttons, large corner radii, filled borderless surfaces.)
// `pub const` = a public compile-time constant, usable from other modules after
// `use crate::theme::*;`.
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
/// Surface of modals, cards and toasts.
pub const SURFACE: Color32 = hex(0x252627);

/// Corner radii (HeroUI radius scale: small 8, medium 12, large 14).
/// `u8` because egui stores corner radii as small integers.
pub const R_SM: u8 = 12; // buttons, inputs (medium)
pub const R_LG: u8 = 14; // modals, cards (large)

/// A separate font family for bold text (egui has no built-in "bold" flag; each
/// weight is its own font).
pub fn bold() -> FontFamily {
    // `.into()` converts the `&str` into the type the function expects.
    FontFamily::Name("bold".into())
}

/// Regular text font of the given size (in points).
pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Bold (semibold) text font of the given size.
pub fn bold_font(size: f32) -> FontId {
    FontId::new(size, bold())
}

/// A separate family for icons. Inter has its own glyphs at the same private
/// code points that the Phosphor icon font uses, so in the normal text family
/// icons showed up as wrong characters (e.g. "Ü"). In this family Phosphor comes
/// first, so its icons win.
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

/// Avatar background color: always the same color for the same username.
pub fn avatar_color(name: &str) -> Color32 {
    // A fixed-size array: `[Type; length]`.
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
    // A tiny hash of the name. `fold` walks the characters carrying an
    // accumulator `h` (starting at 0). `wrapping_mul/add` let the number overflow
    // silently instead of panicking (in debug builds Rust panics on overflow).
    let h = name.chars().fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
    // Pick a color with the remainder; arrays are indexed with `usize`.
    COLORS[(h % COLORS.len() as u32) as usize]
}

/// Loads the fonts. On macOS the system font (SF Pro) is used because it renders
/// crisply there; on Windows/Linux we use the bundled Inter font (Segoe UI's line
/// metrics shifted text vertically off-center). Icons come from Phosphor.
///
/// `GOODBYEDC_FONT=inter` forces Inter on macOS too (for testing).
pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    // `is_ok_and` = "is it Ok AND does the value satisfy this check?"
    let force_inter = std::env::var("GOODBYEDC_FONT").is_ok_and(|v| v == "inter");
    // `if` is an expression in Rust, so it can produce a value directly.
    let sf = if cfg!(target_os = "macos") && !force_inter { std::fs::read("/System/Library/Fonts/SFNS.ttf").ok() } else { None };
    match sf {
        Some(data) => {
            // SF Pro is a *variable* font: one file contains every weight, chosen
            // with the "wght" axis. We register it twice: normal (400) and bold (640).
            // `data.clone()` copies the bytes because the first FontData takes
            // ownership of its copy and we still need `data` for the second one.
            let mut reg = FontData::from_owned(data.clone());
            reg.tweak.coords = VariationCoords::new([("wght", 400.0)]);
            let mut bol = FontData::from_owned(data);
            bol.tweak.coords = VariationCoords::new([("wght", 640.0)]);
            fonts.font_data.insert("ui".into(), reg.into());
            fonts.font_data.insert("ui-bold".into(), bol.into());
        }
        None => {
            // `include_bytes!` embeds the font files into the executable at compile
            // time, so they work on any machine without installing anything.
            fonts.font_data.insert("ui".into(), crisp(FontData::from_static(include_bytes!("../fonts/Inter-Regular.ttf"))).into());
            fonts.font_data.insert("ui-bold".into(), crisp(FontData::from_static(include_bytes!("../fonts/Inter-SemiBold.ttf"))).into());
        }
    }

    // A font *family* is an ordered list of fonts; if the first one lacks a
    // character, egui falls back to the next. Put our font first in front of
    // egui's defaults (which provide emoji and other fallbacks).
    let prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut regular = prop.clone();
    regular.insert(0, "ui".into());
    let mut boldf = prop;
    boldf.insert(0, "ui-bold".into());
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts.families.insert(bold(), boldf);

    // Add the Phosphor icon font as a fallback to both families.
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
    // The dedicated "icons" family: Phosphor first, then our UI font.
    if let Some(icon) = fonts.families.get(&bold()).and_then(|f| f.iter().find(|n| n.contains("phosphor")).cloned()) {
        fonts.families.insert(FontFamily::Name("icons".into()), vec![icon, "ui".into()]);
    }
    ctx.set_fonts(fonts);
}

/// Crisper text on low-DPI screens (common on Windows): "hinting" nudges letter
/// edges onto whole pixels so they don't look blurry.
///
/// The parameter is `mut f`: we take ownership of the FontData, modify it and
/// return it (no copy is made).
fn crisp(mut f: FontData) -> FontData {
    f.tweak.hinting = Some(true);
    f.tweak.subpixel_binning = Some(false);
    f.tweak.hinting_target =
        HintingTarget::Smooth(SmoothHinting { light: false, symmetric_rendering: false, preserve_linear_metrics: false });
    f
}

/// Global look of egui's built-in widgets (labels, text inputs, sliders, menus...).
pub fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    // `style_mut_of` hands us a mutable reference to the dark style inside a
    // closure; whatever we change there is applied.
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        // Default font for each text role.
        style.text_styles = [
            (TextStyle::Small, font(11.5)),
            (TextStyle::Body, font(14.0)),
            (TextStyle::Button, font(14.0)),
            (TextStyle::Heading, bold_font(20.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        // Default spacing: gap between widgets (x, y), padding inside buttons, etc.
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.window_margin = Margin::same(20);
        // Scroll bars float over the content and only show when needed.
        style.spacing.scroll.floating = true;
        style.spacing.scroll.bar_width = 8.0;
        // Labels aren't text-selectable unless we ask for it explicitly.
        style.interaction.selectable_labels = false;

        // `let v = &mut style.visuals;` = a mutable borrow, so we can write `v.x = ...`
        // instead of `style.visuals.x = ...` each time.
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
        // No outline around a focused text input.
        v.selection.stroke = Stroke::NONE;
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);

        // Text inputs (and anything else egui draws itself) are pill-shaped.
        let r = CornerRadius::same(20);
        // Widgets have one look per state: noninteractive, inactive (idle),
        // hovered, active (being pressed/dragged) and open (e.g. an open menu).
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = BG_MAIN;
        w.noninteractive.weak_bg_fill = BG_MAIN;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER); // separators
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = ACTIVE; // slider rail etc.
        w.inactive.weak_bg_fill = Color32::TRANSPARENT; // egui buttons: no fill
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.hovered.corner_radius = r;
        // `expansion` would grow widgets slightly on hover; we don't want that.
        w.hovered.expansion = 0.0;

        w.active.bg_fill = ACTIVE;
        w.active.weak_bg_fill = ACTIVE;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        // `.clone()` because `w.active` must stay usable; assigning it directly
        // would *move* it out of the struct, which Rust doesn't allow here.
        w.open = w.active.clone();
    });
}
