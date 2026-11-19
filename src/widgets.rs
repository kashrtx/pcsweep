use crate::theme::*;
use eframe::egui::{
    self, pos2, text::LayoutJob, vec2, Align, Align2, Button, Color32, FontFamily, FontId, Frame,
    Layout, Margin, Rect, Response, RichText, Rounding, Sense, Shape, Stroke, TextFormat, Ui, Vec2,
    WidgetText,
};

/// Segoe Fluent Icons / Segoe MDL2 Assets code points.
pub mod ic {
    pub const HOME: char = '\u{E80F}';
    pub const CLEAN: char = '\u{E74D}';
    pub const DRIVE: char = '\u{EDA2}';
    pub const PIE: char = '\u{EB05}';
    pub const COPY: char = '\u{E8C8}';
    pub const POWER: char = '\u{E7E8}';
    pub const APPS: char = '\u{E71D}';
    pub const SPEED: char = '\u{EC4A}';
    pub const TOOLS: char = '\u{E90F}';
    pub const HISTORY: char = '\u{E81C}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const FILE: char = '\u{E8A5}';
    pub const REFRESH: char = '\u{E72C}';
    pub const SEARCH: char = '\u{E721}';
    pub const CHECK: char = '\u{E73E}';
    pub const CLOSE: char = '\u{E711}';
    pub const WARN: char = '\u{E7BA}';
    pub const INFO: char = '\u{E946}';
    pub const LOCK: char = '\u{E72E}';
    pub const BACK: char = '\u{E72B}';
    pub const CHEVRON: char = '\u{E76C}';
    pub const OPEN: char = '\u{E8A7}';
    pub const SHIELD: char = '\u{EA18}';
    pub const BOLT: char = '\u{E945}';
    pub const GLOBE: char = '\u{E774}';
    pub const CHAT: char = '\u{E8BD}';
    pub const CODE: char = '\u{E943}';
    pub const PHOTO: char = '\u{EB9F}';
    pub const BUG: char = '\u{EBE8}';
    pub const UPDATE: char = '\u{E895}';
    pub const GAME: char = '\u{E7FC}';
    pub const CHIP: char = '\u{E950}';
    pub const DOWNLOAD: char = '\u{E896}';
    pub const PLAY: char = '\u{E768}';
    pub const STOP: char = '\u{E71A}';
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn light(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("light".into()))
}

pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

pub fn icon(c: char, size: f32, color: Color32) -> RichText {
    RichText::new(c.to_string())
        .font(icon_font(size))
        .color(color)
}

/// Icon + label in one piece of text (for buttons and chips).
pub fn icon_text(c: char, text: &str, size: f32, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(
        &c.to_string(),
        0.0,
        TextFormat {
            font_id: icon_font(size),
            color,
            valign: Align::Center,
            ..Default::default()
        },
    );
    job.append(
        text,
        8.0,
        TextFormat {
            font_id: FontId::proportional(size),
            color,
            valign: Align::Center,
            ..Default::default()
        },
    );
    job
}

pub fn dim(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(color_muted())
}

pub fn faint(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .color(color_secondary())
        .size(12.5)
}

pub fn strong(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into())
        .font(semibold(size))
        .color(color_ink())
}

fn motion(ui: &Ui, id: egui::Id, target: bool) -> f32 {
    let duration = ui.style().animation_time;
    if duration <= 0.0 {
        if target {
            1.0
        } else {
            0.0
        }
    } else {
        ui.ctx().animate_bool_with_time(id, target, duration)
    }
}

pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).font(semibold(28.0)).color(color_ink()));
    ui.label(dim(subtitle));
    ui.add_space(14.0);
}

pub fn section(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(text)
            .font(semibold(15.0))
            .color(color_muted()),
    );
    ui.add_space(2.0);
}

pub fn card_frame() -> Frame {
    Frame::none()
        .fill(color_surface())
        .rounding(corners(14.0))
        .stroke(Stroke::new(1.0_f32, color_outline()))
        .inner_margin(Margin::same(18.0))
}

pub fn tight_frame() -> Frame {
    Frame::none()
        .fill(color_surface())
        .rounding(corners(10.0))
        .stroke(Stroke::new(1.0_f32, color_outline()))
        .inner_margin(Margin::symmetric(14.0, 10.0))
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    card_frame()
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub fn tight_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    tight_frame()
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub fn primary(text: impl Into<WidgetText>) -> Button<'static> {
    let t: WidgetText = text.into();
    Button::new(t.color(Color32::WHITE))
        .fill(color_primary())
        .rounding(corners(9.0))
        .min_size(vec2(0.0, 34.0))
}

pub fn danger(text: impl Into<WidgetText>) -> Button<'static> {
    let t: WidgetText = text.into();
    Button::new(t.color(Color32::WHITE))
        .fill(color_danger())
        .rounding(corners(9.0))
        .min_size(vec2(0.0, 34.0))
}

pub fn ghost(text: impl Into<WidgetText>) -> Button<'static> {
    Button::new(text)
        .fill(color_raised())
        .stroke(Stroke::new(1.0_f32, color_outline()))
        .rounding(corners(9.0))
        .min_size(vec2(0.0, 34.0))
}

pub fn small(text: impl Into<WidgetText>) -> Button<'static> {
    Button::new(text)
        .fill(color_raised())
        .rounding(corners(7.0))
        .min_size(vec2(0.0, 26.0))
}

pub fn chip(text: impl Into<WidgetText>, selected: bool) -> Button<'static> {
    let (fill, stroke) = if selected {
        (color_selected(), Stroke::new(1.0_f32, color_accent()))
    } else {
        (color_surface(), Stroke::new(1.0_f32, color_outline()))
    };
    Button::new(text)
        .fill(fill)
        .stroke(stroke)
        .rounding(corners(17.0))
        .min_size(vec2(0.0, 32.0))
}

/// A small rounded label. Drawn directly so it never stretches to fill a row.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::proportional(11.5), color);
    let size = galley.size() + vec2(14.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, corners(6.0), mix(color_surface(), color, 0.16));
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    resp
}

/// A fixed-width text cell, left or right aligned, truncated if too long.
pub fn cell(ui: &mut Ui, width: f32, right: bool, text: impl Into<WidgetText>) -> Response {
    let layout = if right {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    ui.allocate_ui_with_layout(vec2(width, 20.0), layout, |ui| {
        ui.set_width(width);
        ui.add(egui::Label::new(text).truncate())
    })
    .inner
}

pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

pub fn bar(ui: &mut Ui, frac: f32, color: Color32, size: Vec2) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let r = size.y / 2.0;
    ui.painter()
        .rect_filled(rect, Rounding::same(r), color_raised());
    let f = frac.clamp(0.0, 1.0);
    if f > 0.0 {
        let mut fill = rect;
        fill.set_width((rect.width() * f).max(size.y));
        ui.painter().rect_filled(fill, Rounding::same(r), color);
    }
    resp
}

/// A circular gauge with a big light numeral in the middle.
pub fn ring(
    ui: &mut Ui,
    frac: f32,
    diameter: f32,
    thickness: f32,
    color: Color32,
    big: &str,
    small_text: &str,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(diameter, diameter), Sense::hover());
    let c = rect.center();
    let rad = diameter / 2.0 - thickness / 2.0;
    let painter = ui.painter();
    painter.circle_stroke(c, rad, Stroke::new(thickness, color_raised()));
    let f = frac.clamp(0.0, 1.0);
    if f > 0.0 {
        let n = 120usize;
        let steps = ((n as f32 * f).ceil() as usize).max(1);
        let start = -std::f32::consts::FRAC_PI_2;
        let pts: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let t = (i as f32 / n as f32).min(f);
                let a = start + t * std::f32::consts::TAU;
                c + vec2(a.cos(), a.sin()) * rad
            })
            .collect();
        painter.add(Shape::line(pts, Stroke::new(thickness, color)));
    }
    painter.text(
        c - vec2(0.0, diameter * 0.04),
        Align2::CENTER_CENTER,
        big,
        light(diameter * 0.30),
        color_ink(),
    );
    painter.text(
        c + vec2(0.0, diameter * 0.20),
        Align2::CENTER_CENTER,
        small_text,
        FontId::proportional(12.0),
        color_muted(),
    );
}

/// iOS-style switch. Returns the response; check `.clicked()`.
pub fn toggle(ui: &mut Ui, on: bool, enabled: bool) -> Response {
    let size = vec2(40.0, 22.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let t = motion(ui, resp.id, on);
    let off_col = color_outline();
    let mut track = mix(off_col, color_primary(), t);
    if !enabled {
        track = mix(track, color_surface(), 0.5);
    }
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(rect.height() / 2.0), track);
    let r = rect.height() / 2.0 - 3.0;
    let x = egui::lerp((rect.left() + r + 3.0)..=(rect.right() - r - 3.0), t);
    painter.circle_filled(pos2(x, rect.center().y), r, Color32::WHITE);
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

pub fn nav_item(
    ui: &mut Ui,
    icon_char: char,
    label: &str,
    selected: bool,
    badge_text: Option<&str>,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    let hovered = resp.hovered();
    let selected_t = motion(ui, resp.id.with("selection"), selected);
    let hover_t = motion(ui, resp.id.with("hover"), hovered);
    let painter = ui.painter();
    if selected_t > 0.0 {
        painter.rect_filled(
            rect,
            corners(9.0),
            mix(color_sidebar(), color_selected(), selected_t),
        );
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.left(), rect.top() + 10.0),
                vec2(3.0, (rect.height() - 20.0) * selected_t),
            ),
            corners(2.0),
            mix(color_sidebar(), color_accent(), selected_t),
        );
    } else if hover_t > 0.0 {
        painter.rect_filled(
            rect,
            corners(9.0),
            mix(color_sidebar(), color_surface(), hover_t),
        );
    }
    if resp.has_focus() {
        painter.rect_stroke(
            rect.shrink(1.0),
            corners(9.0),
            Stroke::new(1.5_f32, color_accent()),
        );
    }
    let fg = mix(color_muted(), color_ink(), selected_t.max(hover_t));
    painter.text(
        pos2(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        icon_char,
        icon_font(16.0),
        mix(fg, color_accent(), selected_t),
    );
    painter.text(
        pos2(rect.left() + 44.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(14.5),
        fg,
    );
    if let Some(b) = badge_text {
        painter.text(
            pos2(rect.right() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            b,
            FontId::proportional(12.0),
            color_secondary(),
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A fixed-height list row with a hover highlight. Content is laid out left to right.
pub fn row<R>(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let bg = ui.painter().add(Shape::Noop);
    let width = ui.available_width();
    let inner = ui.allocate_ui_with_layout(
        vec2(width, height),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(width, height));
            ui.add_space(6.0);
            add(ui)
        },
    );
    let rect = inner.response.rect;
    let hover_t = motion(
        ui,
        inner.response.id.with("row-hover"),
        ui.rect_contains_pointer(rect),
    );
    if hover_t > 0.0 {
        ui.painter().set(
            bg,
            Shape::rect_filled(
                rect,
                corners(8.0),
                mix(color_canvas(), color_raised(), hover_t),
            ),
        );
    }
    inner.inner
}

/// A colored message strip with an icon.
pub fn banner<R>(
    ui: &mut Ui,
    color: Color32,
    icon_char: char,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    Frame::none()
        .fill(mix(color_canvas(), color, 0.10))
        .stroke(Stroke::new(1.0_f32, mix(color_canvas(), color, 0.35)))
        .rounding(corners(10.0))
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(icon(icon_char, 16.0, color));
                add(ui)
            })
            .inner
        })
        .inner
}

/// Text field with a search icon.
pub fn search_box(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    ui.horizontal(|ui| {
        ui.label(icon(ic::SEARCH, 14.0, color_secondary()));
        ui.add(
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .desired_width(width),
        )
    })
    .inner
}
