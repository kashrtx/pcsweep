use eframe::egui::{
    self, vec2, Color32, FontData, FontDefinitions, FontFamily, FontId, Rounding, Stroke,
    TextStyle, Visuals,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Skin {
    #[default]
    Slate,
    Frost,
    Aero,
    Classic,
    Cobalt,
    Terminal,
    Circuit,
    Sakura,
    Contrast,
}

impl Skin {
    pub const ALL: [Self; 9] = [
        Self::Slate,
        Self::Frost,
        Self::Aero,
        Self::Classic,
        Self::Cobalt,
        Self::Terminal,
        Self::Circuit,
        Self::Sakura,
        Self::Contrast,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Slate => "Slate",
            Self::Frost => "Frosted glass",
            Self::Aero => "Skyline Aero",
            Self::Classic => "Retro desktop",
            Self::Cobalt => "Cobalt desktop",
            Self::Terminal => "Green terminal",
            Self::Circuit => "Neon circuit",
            Self::Sakura => "Sakura",
            Self::Contrast => "High contrast",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Slate => "slate",
            Self::Frost => "frost",
            Self::Aero => "aero",
            Self::Classic => "classic",
            Self::Cobalt => "cobalt",
            Self::Terminal => "terminal",
            Self::Circuit => "circuit",
            Self::Sakura => "sakura",
            Self::Contrast => "contrast",
        }
    }

    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|skin| skin.key() == key)
            .unwrap_or_default()
    }

    fn light(self) -> bool {
        matches!(self, Self::Aero | Self::Classic | Self::Cobalt)
    }

    pub fn samples(self) -> [Color32; 3] {
        let palette = self.palette();
        [palette.surface, palette.raised, palette.primary]
    }

    fn palette(self) -> Palette {
        match self {
            Self::Slate => Palette {
                canvas: c(0x12151c),
                sidebar: c(0x161a22),
                surface: c(0x1b202a),
                raised: c(0x232936),
                outline: c(0x3b4559),
                ink: c(0xe7eaf0),
                muted: c(0xb1b9cb),
                secondary: c(0x9da8bd),
                accent: c(0xa5b8ff),
                primary: c(0x4b5fc1),
                selected: c(0x2a3559),
                success: c(0x59dbb4),
                warning: c(0xffc966),
                error: c(0xff979b),
            },
            Self::Frost => Palette {
                canvas: c(0x141c31),
                sidebar: c(0x1b2742),
                surface: c(0x222e4a),
                raised: c(0x303e5f),
                outline: c(0x506586),
                ink: c(0xf0f7ff),
                muted: c(0xc3d3ed),
                secondary: c(0xb2c4e3),
                accent: c(0xbae9ff),
                primary: c(0x3664a1),
                selected: c(0x334a70),
                success: c(0x81ebcf),
                warning: c(0xffd18a),
                error: c(0xffa7b7),
            },
            Self::Aero => Palette {
                canvas: c(0xe5f1fd),
                sidebar: c(0xd5e8f8),
                surface: c(0xf7fbff),
                raised: c(0xe3effb),
                outline: c(0x7a9db9),
                ink: c(0x122b43),
                muted: c(0x334e67),
                secondary: c(0x415971),
                accent: c(0x144b85),
                primary: c(0x185b9b),
                selected: c(0xc9e3fa),
                success: c(0x12604d),
                warning: c(0x744306),
                error: c(0x951f3d),
            },
            Self::Classic => Palette {
                canvas: c(0xd9d9d9),
                sidebar: c(0xc8c8c8),
                surface: c(0xeeeeee),
                raised: c(0xdedede),
                outline: c(0x727272),
                ink: c(0x1a1a1a),
                muted: c(0x3c3c3c),
                secondary: c(0x454545),
                accent: c(0x173475),
                primary: c(0x1b3877),
                selected: c(0xc3cee7),
                success: c(0x154c39),
                warning: c(0x654000),
                error: c(0x851733),
            },
            Self::Cobalt => Palette {
                canvas: c(0xf1eee1),
                sidebar: c(0xdfe9fa),
                surface: c(0xfffcf4),
                raised: c(0xe7effb),
                outline: c(0x8f9bc0),
                ink: c(0x1c2948),
                muted: c(0x3a4b6c),
                secondary: c(0x445676),
                accent: c(0x214b9c),
                primary: c(0x2157b8),
                selected: c(0xcfdff7),
                success: c(0x175c37),
                warning: c(0x6c4600),
                error: c(0x9d2135),
            },
            Self::Terminal => Palette {
                canvas: c(0x0b1612),
                sidebar: c(0x101e17),
                surface: c(0x14261d),
                raised: c(0x1b3427),
                outline: c(0x43735a),
                ink: c(0xe5f8e8),
                muted: c(0xb8ddc0),
                secondary: c(0xa1cbb0),
                accent: c(0x9afaac),
                primary: c(0x285c3b),
                selected: c(0x264632),
                success: c(0x88edb7),
                warning: c(0xf3d78a),
                error: c(0xffa5a5),
            },
            Self::Circuit => Palette {
                canvas: c(0x0a131e),
                sidebar: c(0x101e2c),
                surface: c(0x112738),
                raised: c(0x17354b),
                outline: c(0x3b758c),
                ink: c(0xe5faff),
                muted: c(0xb9deed),
                secondary: c(0xa6cddd),
                accent: c(0x80edff),
                primary: c(0x176481),
                selected: c(0x1b4055),
                success: c(0x75e8c6),
                warning: c(0xffc583),
                error: c(0xffa1a6),
            },
            Self::Sakura => Palette {
                canvas: c(0x23182c),
                sidebar: c(0x30213c),
                surface: c(0x392941),
                raised: c(0x4b3652),
                outline: c(0x755678),
                ink: c(0xfff0f7),
                muted: c(0xeccee5),
                secondary: c(0xdcbdd8),
                accent: c(0xffb4de),
                primary: c(0x824162),
                selected: c(0x5e3b5d),
                success: c(0xa2ebce),
                warning: c(0xffd88b),
                error: c(0xffb0b7),
            },
            Self::Contrast => Palette {
                canvas: Color32::BLACK,
                sidebar: c(0x090909),
                surface: c(0x101010),
                raised: c(0x252525),
                outline: c(0x999999),
                ink: Color32::WHITE,
                muted: c(0xe5e5e5),
                secondary: c(0xd7d7d7),
                accent: c(0x8affff),
                primary: c(0x174567),
                selected: c(0x253c43),
                success: c(0x98ffb9),
                warning: c(0xffe369),
                error: c(0xffa5ad),
            },
        }
    }
}

const fn c(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

struct Palette {
    canvas: Color32,
    sidebar: Color32,
    surface: Color32,
    raised: Color32,
    outline: Color32,
    ink: Color32,
    muted: Color32,
    secondary: Color32,
    accent: Color32,
    primary: Color32,
    selected: Color32,
    success: Color32,
    warning: Color32,
    error: Color32,
}

thread_local! {
    static ACTIVE: std::cell::Cell<Skin> = const { std::cell::Cell::new(Skin::Slate) };
}

pub fn select(ctx: &egui::Context, skin: Skin) {
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("pcsweep.skin"), skin));
    apply(ctx);
}

pub fn activate(ctx: &egui::Context) {
    let skin = ctx
        .data(|data| data.get_temp::<Skin>(egui::Id::new("pcsweep.skin")))
        .unwrap_or_default();
    ACTIVE.set(skin);
}

macro_rules! colors {
    ($($getter:ident : $role:ident),* $(,)?) => {
        $(pub fn $getter() -> Color32 { ACTIVE.get().palette().$role })*
    }
}
colors! {
    color_canvas: canvas, color_sidebar: sidebar, color_surface: surface, color_raised: raised,
    color_outline: outline, color_ink: ink, color_muted: muted, color_secondary: secondary,
    color_accent: accent, color_primary: primary, color_selected: selected, color_success: success,
    color_warning: warning, color_error: error,
}
pub fn color_danger() -> Color32 {
    c(0xa83449)
}
pub fn color_input() -> Color32 {
    if ACTIVE.get().light() {
        c(0xfcfdff)
    } else {
        mix(color_canvas(), Color32::BLACK, 0.22)
    }
}

pub fn corners(radius: f32) -> Rounding {
    Rounding::same(match ACTIVE.get() {
        Skin::Classic | Skin::Contrast => 1.0,
        Skin::Terminal | Skin::Circuit => radius * 0.40,
        Skin::Frost | Skin::Sakura => radius * 1.28,
        _ => radius,
    })
}

/// A stationary backlight under the content. Colors stay muted behind text.
pub fn backdrop(ui: &egui::Ui, sidebar: bool) {
    let skin = ACTIVE.get();
    let amount = match skin {
        Skin::Frost => 0.12,
        Skin::Aero | Skin::Sakura => 0.10,
        Skin::Cobalt | Skin::Terminal | Skin::Circuit => 0.07,
        _ => return,
    };
    let base = if sidebar {
        color_sidebar()
    } else {
        color_canvas()
    };
    let lifted = mix(base, color_accent(), amount);
    let soft = mix(base, color_surface(), 0.45);
    let rect = ui.clip_rect();
    let mid = rect.center();
    let mut mesh = egui::Mesh::default();
    for (pos, color) in [
        (rect.left_top(), lifted),
        (egui::pos2(mid.x, rect.top()), lifted),
        (rect.right_top(), soft),
        (egui::pos2(rect.left(), mid.y), soft),
        (mid, base),
        (egui::pos2(rect.right(), mid.y), soft),
        (rect.left_bottom(), base),
        (egui::pos2(mid.x, rect.bottom()), base),
        (rect.right_bottom(), base),
    ] {
        mesh.colored_vertex(pos, color);
    }
    mesh.indices.extend([
        0, 1, 4, 0, 4, 3, 1, 2, 5, 1, 5, 4, 3, 4, 7, 3, 7, 6, 4, 5, 8, 4, 8, 7,
    ]);
    ui.painter().add(egui::Shape::mesh(mesh));
}

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
        (x as f32 + (y as f32 - x as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn fitted_zoom(native_points: egui::Vec2) -> f32 {
    (native_points.x / 1040.0)
        .min(native_points.y / 680.0)
        .clamp(0.65, 1.0)
}

/// A high-DPI desktop can offer fewer logical points than the ordinary layout's
/// minimum. Fit the whole layout to that work area without shrinking normal
/// windows. Multiplying out the current zoom prevents resize feedback loops.
pub fn fit_to_viewport(ctx: &egui::Context) {
    let scale = ctx
        .data(|d| d.get_temp::<f32>(egui::Id::new("pcsweep.text_scale")))
        .unwrap_or(1.0);
    let zoom = fitted_zoom(ctx.screen_rect().size() * ctx.zoom_factor()) * scale;
    if (ctx.zoom_factor() - zoom).abs() > 0.005 {
        ctx.set_zoom_factor(zoom);
    }
}

#[cfg(test)]
mod responsive_tests {
    use super::*;
    #[test]
    fn compact_fit_is_stable_and_normal_windows_keep_their_size() {
        for native in [
            egui::vec2(900.0, 520.0),
            egui::vec2(960.0, 540.0),
            egui::vec2(1040.0, 680.0),
            egui::vec2(1920.0, 1080.0),
        ] {
            let target = fitted_zoom(native);
            let logical = native / target;
            assert!(logical.x >= 1040.0 - 0.1 && logical.y >= 680.0 - 0.1);
            assert_eq!(
                fitted_zoom(logical * target),
                target,
                "rescaling must not oscillate"
            );
            if native.x >= 1040.0 && native.y >= 680.0 {
                assert_eq!(target, 1.0);
            }
        }
    }
}

/// Loads Windows' own Segoe UI family so the app feels native on Windows 11,
/// plus the Segoe Fluent Icons font for crisp system icons.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = crate::platform::windows_directory()
        .ok()
        .map(|path| path.join("Fonts"));
    let load = |file: &str| std::fs::read(dir.as_ref()?.join(file)).ok();

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

    if ctx
        .data(|d| d.get_temp::<bool>(egui::Id::new("pcsweep.bold")))
        .unwrap_or(false)
    {
        body = semi.clone();
    }
    let mut light = body.clone();
    if !ctx
        .data(|d| d.get_temp::<bool>(egui::Id::new("pcsweep.bold")))
        .unwrap_or(false)
    {
        if let Some(bytes) = load("segoeuil.ttf") {
            fonts
                .font_data
                .insert("segoe_light".to_owned(), FontData::from_owned(bytes));
            light.insert(0, "segoe_light".to_owned());
        }
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
    fonts
        .families
        .insert(FontFamily::Name("semibold".into()), semi);
    fonts
        .families
        .insert(FontFamily::Name("light".into()), light);
    fonts
        .families
        .insert(FontFamily::Name("icons".into()), icons);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    activate(ctx);
    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(26.0, FontFamily::Name("semibold".into())),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = vec2(8.0, 8.0);
    style.spacing.button_padding = vec2(14.0, 7.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.icon_width = 18.0;
    style.spacing.icon_width_inner = 10.0;
    style.spacing.icon_spacing = 6.0;
    let motion = ctx
        .data(|data| data.get_temp::<bool>(egui::Id::new("pcsweep.motion")))
        .unwrap_or(true);
    style.animation_time = if motion { 0.14 } else { 0.0 };
    // Reserve a real gutter consistently, rather than drawing an expanding
    // floating thumb over file names or placing it against content-width text.
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 10.0,
        handle_min_length: 24.0,
        ..egui::style::ScrollStyle::solid()
    };

    let mut v = if ACTIVE.get().light() {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    v.panel_fill = color_canvas();
    v.window_fill = color_surface();
    v.window_stroke = Stroke::new(1.0_f32, color_outline());
    v.window_rounding = corners(14.0);
    v.menu_rounding = corners(10.0);
    v.extreme_bg_color = color_input();
    v.faint_bg_color = color_raised();
    v.code_bg_color = color_input();
    v.hyperlink_color = color_accent();
    v.selection.bg_fill = color_selected();
    v.selection.stroke = Stroke::new(1.0_f32, color_accent());
    v.warn_fg_color = color_warning();
    v.error_fg_color = color_error();

    let r = corners(8.0);
    {
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = color_surface();
        w.noninteractive.weak_bg_fill = color_surface();
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, color_outline());
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, color_ink());
        w.noninteractive.rounding = r;

        w.inactive.bg_fill = color_raised();
        w.inactive.weak_bg_fill = color_raised();
        w.inactive.bg_stroke = Stroke::new(1.0_f32, color_outline());
        w.inactive.fg_stroke = Stroke::new(1.0_f32, color_ink());
        w.inactive.rounding = r;

        w.hovered.bg_fill = mix(color_raised(), color_accent(), 0.10);
        w.hovered.weak_bg_fill = mix(color_raised(), color_accent(), 0.10);
        w.hovered.bg_stroke = Stroke::new(1.0_f32, color_accent());
        w.hovered.fg_stroke = Stroke::new(1.5_f32, color_ink());
        w.hovered.rounding = r;

        w.active.bg_fill = color_selected();
        w.active.weak_bg_fill = color_selected();
        w.active.bg_stroke = Stroke::new(1.0_f32, color_accent());
        w.active.fg_stroke = Stroke::new(2.0_f32, color_ink());
        w.active.rounding = r;

        w.open = w.active.clone();
    }
    style.visuals = v;
    ctx.set_style(style);
}

pub fn set_motion(ctx: &egui::Context, enabled: bool) {
    ctx.data_mut(|data| data.insert_temp(egui::Id::new("pcsweep.motion"), enabled));
    let duration = if enabled { 0.14 } else { 0.0 };
    if (ctx.style().animation_time - duration).abs() > f32::EPSILON {
        ctx.style_mut(|style| style.animation_time = duration);
    }
}

pub fn read_windows_motion(ctx: &egui::Context) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
    };
    let mut enabled = 1i32;
    if unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast(),
            0,
        )
    } != 0
    {
        set_motion(ctx, enabled != 0);
    }
}

#[cfg(test)]
mod contrast_tests {
    use super::*;

    fn contrast(a: Color32, b: Color32) -> f32 {
        let luma = |color: Color32| {
            let srgb = |value: u8| {
                let num = f32::from(value) / 255.0;
                if num <= 0.04045 {
                    num / 12.92
                } else {
                    ((num + 0.055) / 1.055).powf(2.4)
                }
            };
            srgb(color.r()) * 0.2126 + srgb(color.g()) * 0.7152 + srgb(color.b()) * 0.0722
        };
        let [a, b] = [luma(a), luma(b)];
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn every_theme_preserves_reading_status_and_button_contrast() {
        for skin in Skin::ALL {
            let p = skin.palette();
            for background in [p.canvas, p.sidebar, p.surface, p.raised, p.selected] {
                for foreground in [
                    p.ink,
                    p.muted,
                    p.secondary,
                    p.accent,
                    p.success,
                    p.warning,
                    p.error,
                ] {
                    let ratio = contrast(foreground, background);
                    assert!(
                        ratio >= 4.5,
                        "{skin:?}: {foreground:?} on {background:?} is {ratio}"
                    );
                }
            }
            for background in [p.primary, color_danger()] {
                assert!(
                    contrast(Color32::WHITE, background) >= 4.5,
                    "{skin:?} action button"
                );
            }
            assert_eq!(Skin::from_key(skin.key()), skin);
        }
        assert_eq!(Skin::from_key("unknown"), Skin::Slate);
    }
}
