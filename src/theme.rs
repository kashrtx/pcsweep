use eframe::egui::{
    self, vec2, Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, Stroke,
    TextStyle, Visuals,
};
use std::path::PathBuf;

// Palette: a cool slate base that stays quiet, so the space map can be loud.
pub const BG: Color32 = Color32::from_rgb(18, 21, 28); // ink
pub const SIDEBAR: Color32 = Color32::from_rgb(22, 26, 34);
pub const CARD: Color32 = Color32::from_rgb(27, 32, 42); // slate
pub const CARD_HI: Color32 = Color32::from_rgb(35, 41, 54);
pub const BORDER: Color32 = Color32::from_rgb(45, 52, 68); // line
pub const TEXT: Color32 = Color32::from_rgb(231, 234, 240); // fog
pub const TEXT_DIM: Color32 = Color32::from_rgb(152, 161, 179); // mist
pub const TEXT_FAINT: Color32 = Color32::from_rgb(105, 114, 134);
pub const ACCENT: Color32 = Color32::from_rgb(143, 168, 255); // signal
pub const ACCENT_STRONG: Color32 = Color32::from_rgb(98, 124, 235);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(38, 46, 80);
pub const GOOD: Color32 = Color32::from_rgb(79, 209, 165); // fresh
pub const WARN: Color32 = Color32::from_rgb(245, 185, 63); // amber
pub const BAD: Color32 = Color32::from_rgb(242, 120, 120); // coral
pub const DANGER_FILL: Color32 = Color32::from_rgb(196, 72, 82);

/// Rich colors used only by the space map and type charts.
pub const PALETTE: [Color32; 8] = [
    Color32::from_rgb(86, 110, 226),
    Color32::from_rgb(34, 146, 196),
    Color32::from_rgb(32, 158, 118),
    Color32::from_rgb(196, 146, 38),
    Color32::from_rgb(194, 86, 146),
    Color32::from_rgb(126, 96, 206),
    Color32::from_rgb(206, 106, 58),
    Color32::from_rgb(38, 156, 156),
];

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| -> u8 {
        (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Loads Windows' own Segoe UI family so the app feels native on Windows 11,
/// plus the Segoe Fluent Icons font for crisp system icons.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".to_string()))
        .join("Fonts");
    let load = |file: &str| std::fs::read(dir.join(file)).ok();

    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let mut body = defaults.clone();
    if let Some(bytes) = load("segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe".to_owned(), FontData::from_owned(bytes));
        body.insert(0, "segoe".to_owned());
    }

    let mut semi = body.clone();
    if let Some(bytes) = load("seguisb.ttf") {
        fonts
            .font_data
            .insert("segoe_sb".to_owned(), FontData::from_owned(bytes));
        semi.insert(0, "segoe_sb".to_owned());
    }

    let mut light = body.clone();
    if let Some(bytes) = load("segoeuil.ttf") {
        fonts
            .font_data
            .insert("segoe_light".to_owned(), FontData::from_owned(bytes));
        light.insert(0, "segoe_light".to_owned());
    }

    let mut icons = body.clone();
    for file in ["SegoeIcons.ttf", "segmdl2.ttf"] {
        if let Some(bytes) = load(file) {
            fonts
                .font_data
                .insert("icons".to_owned(), FontData::from_owned(bytes));
            icons.insert(0, "icons".to_owned());
            break;
        }
    }

    fonts.families.insert(FontFamily::Proportional, body);
    fonts.families.insert(FontFamily::Name("semibold".into()), semi);
    fonts.families.insert(FontFamily::Name("light".into()), light);
    fonts.families.insert(FontFamily::Name("icons".into()), icons);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Heading,
            FontId::new(26.0, FontFamily::Name("semibold".into())),
        ),
        (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
    ]
    .into();
    style.spacing.item_spacing = vec2(8.0, 8.0);
    style.spacing.button_padding = vec2(14.0, 7.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.icon_width = 18.0;
    style.spacing.icon_width_inner = 10.0;
    style.spacing.icon_spacing = 6.0;

    let mut v = Visuals::dark();
    v.dark_mode = true;
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_rounding = Rounding::same(14.0);
    v.menu_rounding = Rounding::same(10.0);
    v.extreme_bg_color = Color32::from_rgb(13, 15, 21);
    v.faint_bg_color = CARD_HI;
    v.code_bg_color = Color32::from_rgb(13, 15, 21);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT_SOFT;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.warn_fg_color = WARN;
    v.error_fg_color = BAD;

    let r = Rounding::same(8.0);
    {
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.rounding = r;

        w.inactive.bg_fill = CARD_HI;
        w.inactive.weak_bg_fill = CARD_HI;
        w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.rounding = r;

        w.hovered.bg_fill = Color32::from_rgb(44, 51, 68);
        w.hovered.weak_bg_fill = Color32::from_rgb(44, 51, 68);
        w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(70, 80, 104));
        w.hovered.fg_stroke = Stroke::new(1.5, TEXT);
        w.hovered.rounding = r;

        w.active.bg_fill = Color32::from_rgb(52, 60, 80);
        w.active.weak_bg_fill = Color32::from_rgb(52, 60, 80);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(2.0, TEXT);
        w.active.rounding = r;

        w.open = w.active.clone();
    }
    style.visuals = v;
    ctx.set_style(style);
}
