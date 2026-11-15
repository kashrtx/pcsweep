//! Headless layout regression checks and reviewable CPU-rendered UI previews.
//! These tests send no input events, poll no workers, and invoke no system changes.
use crate::app::{Action, App, Modal, Page};
use crate::theme;
use eframe::egui::{self, Color32, Frame, ImageData, Margin, Pos2, Rect, TextureId};
use egui::epaint::{ClippedPrimitive, ImageDelta, Primitive, Shape, Vertex};
use image::{Rgba, RgbaImage};
use std::collections::HashMap;
use std::path::Path;

struct Texture {
    width: usize,
    height: usize,
    pixels: Vec<Color32>,
}

#[derive(Default)]
struct CpuPainter {
    textures: HashMap<TextureId, Texture>,
}

impl CpuPainter {
    fn set_texture(&mut self, id: TextureId, delta: &ImageDelta) {
        let [width, height] = delta.image.size();
        assert!(width > 0 && height > 0 && width * height <= 16_777_216);
        let pixels: Vec<_> = match &delta.image {
            ImageData::Color(image) => image.pixels.clone(),
            ImageData::Font(image) => image.srgba_pixels(None).collect(),
        };
        assert_eq!(pixels.len(), width * height);
        if let Some([x, y]) = delta.pos {
            let old = self
                .textures
                .get_mut(&id)
                .expect("partial patch must have an existing texture");
            assert!(x + width <= old.width && y + height <= old.height);
            for row in 0..height {
                old.pixels[(y + row) * old.width + x..(y + row) * old.width + x + width]
                    .copy_from_slice(&pixels[row * width..(row + 1) * width]);
            }
        } else {
            self.textures.insert(
                id,
                Texture {
                    width,
                    height,
                    pixels,
                },
            );
        }
    }

    fn render(&self, size: [u32; 2], primitives: &[ClippedPrimitive]) -> RgbaImage {
        let mut image = RgbaImage::from_pixel(size[0], size[1], Rgba(theme::BG.to_array()));
        let screen = Rect::from_min_max(Pos2::ZERO, egui::pos2(size[0] as f32, size[1] as f32));
        let mut triangles = 0;
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                panic!("page previews must not depend on a GPU paint callback");
            };
            assert!(mesh.is_valid(), "egui mesh must have valid indices");
            assert!(
                mesh.vertices.iter().all(|v| v.pos.x.is_finite()
                    && v.pos.y.is_finite()
                    && v.uv.x.is_finite()
                    && v.uv.y.is_finite()),
                "every UI mesh vertex and UV must be finite, including clipped geometry"
            );
            let texture = self
                .textures
                .get(&mesh.texture_id)
                .expect("all drawn textures must be uploaded");
            let clip = primitive.clip_rect.intersect(screen);
            if !clip.is_positive() {
                continue;
            }
            assert_eq!(mesh.indices.len() % 3, 0);
            triangles += mesh.indices.len() / 3;
            assert!(triangles < 500_000, "unexpectedly large UI mesh");
            for indices in mesh.indices.chunks_exact(3) {
                triangle(
                    &mut image,
                    clip,
                    texture,
                    [
                        mesh.vertices[indices[0] as usize],
                        mesh.vertices[indices[1] as usize],
                        mesh.vertices[indices[2] as usize],
                    ],
                );
            }
        }
        assert!(
            triangles > 100,
            "a complete page should render more than a background"
        );
        image
    }
}

fn edge(a: Pos2, b: Pos2, p: Pos2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}
fn top_left(a: Pos2, b: Pos2) -> bool {
    b.y < a.y || (b.y == a.y && b.x > a.x)
}
fn covered(value: f32, a: Pos2, b: Pos2) -> bool {
    value > 0.0 || (value == 0.0 && top_left(a, b))
}
fn triangle(image: &mut RgbaImage, clip: Rect, texture: &Texture, mut vertices: [Vertex; 3]) {
    assert!(vertices.iter().all(|v| v.pos.x.is_finite()
        && v.pos.y.is_finite()
        && v.uv.x.is_finite()
        && v.uv.y.is_finite()));
    let mut area = edge(vertices[0].pos, vertices[1].pos, vertices[2].pos);
    if area.abs() < 0.00001 {
        return;
    }
    if area < 0.0 {
        vertices.swap(1, 2);
        area = -area;
    }
    let [a, b, c] = vertices;
    let min_x = a
        .pos
        .x
        .min(b.pos.x)
        .min(c.pos.x)
        .max(clip.min.x)
        .floor()
        .max(0.0) as u32;
    let max_x = a
        .pos
        .x
        .max(b.pos.x)
        .max(c.pos.x)
        .min(clip.max.x)
        .ceil()
        .min(image.width() as f32) as u32;
    let min_y = a
        .pos
        .y
        .min(b.pos.y)
        .min(c.pos.y)
        .max(clip.min.y)
        .floor()
        .max(0.0) as u32;
    let max_y = a
        .pos
        .y
        .max(b.pos.y)
        .max(c.pos.y)
        .min(clip.max.y)
        .ceil()
        .min(image.height() as f32) as u32;
    let colors = [a.color.to_array(), b.color.to_array(), c.color.to_array()];
    for y in min_y..max_y {
        for x in min_x..max_x {
            let p = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
            if !clip.contains(p) {
                continue;
            }
            let edges = [
                edge(b.pos, c.pos, p),
                edge(c.pos, a.pos, p),
                edge(a.pos, b.pos, p),
            ];
            if !covered(edges[0], b.pos, c.pos)
                || !covered(edges[1], c.pos, a.pos)
                || !covered(edges[2], a.pos, b.pos)
            {
                continue;
            }
            let weights = [edges[0] / area, edges[1] / area, edges[2] / area];
            let uv = [
                a.uv.x * weights[0] + b.uv.x * weights[1] + c.uv.x * weights[2],
                a.uv.y * weights[0] + b.uv.y * weights[1] + c.uv.y * weights[2],
            ];
            let texel = sample(texture, uv);
            let mut source = [0.0; 4];
            for channel in 0..4 {
                source[channel] = (colors[0][channel] as f32 * weights[0]
                    + colors[1][channel] as f32 * weights[1]
                    + colors[2][channel] as f32 * weights[2])
                    * texel[channel]
                    / 255.0;
            }
            let target = image.get_pixel_mut(x, y);
            let remaining = 1.0 - source[3] / 255.0;
            for channel in 0..4 {
                target.0[channel] = (source[channel] + target.0[channel] as f32 * remaining)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
}
fn sample(texture: &Texture, uv: [f32; 2]) -> [f32; 4] {
    let x = (uv[0] * texture.width as f32 - 0.5).clamp(0.0, texture.width.saturating_sub(1) as f32);
    let y =
        (uv[1] * texture.height as f32 - 0.5).clamp(0.0, texture.height.saturating_sub(1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(texture.width - 1);
    let y1 = (y0 + 1).min(texture.height - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let pixels = [
        texture.pixels[y0 * texture.width + x0].to_array(),
        texture.pixels[y0 * texture.width + x1].to_array(),
        texture.pixels[y1 * texture.width + x0].to_array(),
        texture.pixels[y1 * texture.width + x1].to_array(),
    ];
    let mut output = [0.0; 4];
    for channel in 0..4 {
        output[channel] = pixels[0][channel] as f32 * (1.0 - fx) * (1.0 - fy)
            + pixels[1][channel] as f32 * fx * (1.0 - fy)
            + pixels[2][channel] as f32 * (1.0 - fx) * fy
            + pixels[3][channel] as f32 * fx * fy;
    }
    output
}

fn render_page(page: Page, size: [u32; 2], path: &Path) {
    let ctx = egui::Context::default();
    theme::setup_fonts(&ctx);
    theme::apply(&ctx);
    // No App::new, poll, clicks, or eframe::App::update: fixtures contain data
    // only, so rendering cannot start scans, execute commands or alter settings.
    let mut app = App::test_fixture();
    app.page = page;
    let mut painter = CpuPainter::default();
    for frame in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(size[0] as f32, size[1] as f32),
            )),
            time: Some(frame as f64 / 60.0),
            ..egui::RawInput::default()
        };
        let output = ctx.run(input, |ctx| {
            egui::SidePanel::left("nav")
                .exact_width(236.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    Frame::none()
                        .fill(theme::SIDEBAR)
                        .inner_margin(Margin::symmetric(14.0, 20.0)),
                )
                .show(ctx, |ui| app.sidebar(ui));
            egui::CentralPanel::default()
                .frame(
                    Frame::none()
                        .fill(theme::BG)
                        .inner_margin(Margin::symmetric(30.0, 22.0)),
                )
                .show(ctx, |ui| app.render_page(ui));
        });
        assert!(
            app.modal.is_none(),
            "rendering a page must not open a confirmation without input"
        );
        assert!(
            !app.mutations_running(),
            "preview fixtures must never start a mutation"
        );
        assert!(!output.shapes.is_empty(), "page must draw content");
        for (id, delta) in &output.textures_delta.set {
            painter.set_texture(*id, delta);
        }
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        if frame == 2 {
            let image = painter.render(size, &primitives);
            let background = theme::BG.to_array();
            let content_pixels = image.pixels().filter(|p| p.0 != background).count();
            assert!(
                content_pixels > (size[0] * size[1] / 8) as usize,
                "preview must contain the sidebar and page content"
            );
            image
                .save(path)
                .expect("save a PNG preview for visual review");
        }
        for id in output.textures_delta.free {
            painter.textures.remove(&id);
        }
    }
}

#[test]
fn all_management_pages_render_at_minimum_and_default_window_sizes() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ui-previews");
    std::fs::create_dir_all(&directory).unwrap();
    for (name, page) in [
        ("privacy", Page::Privacy),
        ("security", Page::Security),
        ("background", Page::Background),
        ("memory", Page::Memory),
        ("apps", Page::Apps),
        ("updates", Page::Updates),
        ("ads", Page::Ads),
        ("network", Page::Network),
        ("power", Page::Power),
    ] {
        for size in [[1040, 680], [1320, 860]] {
            let path = directory.join(format!("{name}-{}x{}.png", size[0], size[1]));
            render_page(page, size, &path);
        }
    }
}

#[test]
fn partial_font_atlas_updates_preserve_existing_texels() {
    let mut painter = CpuPainter::default();
    let id = TextureId::Managed(0);
    painter.set_texture(
        id,
        &ImageDelta::full(
            egui::ColorImage::new([2, 2], Color32::WHITE),
            egui::TextureOptions::LINEAR,
        ),
    );
    painter.set_texture(
        id,
        &ImageDelta::partial(
            [1, 0],
            egui::ColorImage::new([1, 1], Color32::BLACK),
            egui::TextureOptions::LINEAR,
        ),
    );
    assert_eq!(
        painter.textures[&id].pixels,
        vec![
            Color32::WHITE,
            Color32::BLACK,
            Color32::WHITE,
            Color32::WHITE
        ]
    );
}

#[test]
fn cpu_renderer_respects_clip_and_triangle_winding() {
    let mut painter = CpuPainter::default();
    let id = TextureId::Managed(0);
    painter.set_texture(
        id,
        &ImageDelta::full(
            egui::ColorImage::new([1, 1], Color32::WHITE),
            egui::TextureOptions::LINEAR,
        ),
    );
    let texture = &painter.textures[&id];
    let vertices = [
        Vertex {
            pos: egui::pos2(0.0, 0.0),
            uv: Pos2::ZERO,
            color: Color32::RED,
        },
        Vertex {
            pos: egui::pos2(8.0, 0.0),
            uv: Pos2::ZERO,
            color: Color32::RED,
        },
        Vertex {
            pos: egui::pos2(0.0, 8.0),
            uv: Pos2::ZERO,
            color: Color32::RED,
        },
    ];
    let clip = Rect::from_min_max(egui::pos2(2.0, 2.0), egui::pos2(6.0, 6.0));
    let mut normal = RgbaImage::new(8, 8);
    let mut reverse = RgbaImage::new(8, 8);
    triangle(&mut normal, clip, texture, vertices);
    triangle(
        &mut reverse,
        clip,
        texture,
        [vertices[2], vertices[1], vertices[0]],
    );
    assert_eq!(normal, reverse);
    assert_eq!(normal.get_pixel(0, 0).0, [0, 0, 0, 0]);
    assert_eq!(normal.get_pixel(2, 2).0, Color32::RED.to_array());
    assert_eq!(normal.get_pixel(6, 2).0, [0, 0, 0, 0]);
}

fn matching_text_rects(shape: &Shape, clip: Rect, expected: &str, found: &mut Vec<(Rect, Rect)>) {
    match shape {
        Shape::Text(text) if text.galley.job.text == expected => {
            found.push((text.galley.rect.translate(text.pos.to_vec2()), clip));
        }
        Shape::Vec(shapes) => {
            for shape in shapes {
                matching_text_rects(shape, clip, expected, found);
            }
        }
        _ => {}
    }
}

#[test]
fn long_confirmation_content_keeps_both_actions_visible_at_supported_sizes() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ui-previews");
    std::fs::create_dir_all(&directory).unwrap();
    for size in [[1040, 680], [1320, 860]] {
        let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32));
        let ctx = egui::Context::default();
        theme::setup_fonts(&ctx);
        theme::apply(&ctx);
        let mut app = App::test_fixture();
        app.page = Page::Privacy;
        app.modal = Some(Modal {
            title: "Review every affected setting before confirming a change with a deliberately long title".repeat(3),
            body: "This preview describes a reversible settings change. Read the affected values and any restart requirements before you choose an action. ".repeat(80),
            details: (0..80).map(|i| format!("Setting {i}: C:\\Users\\Example\\AppData\\Local\\Vendor\\a-long-folder-name-for-preview\\setting.json" )).collect(),
            note: Some("Your previous values will be saved for Undo. Managed settings are kept. Save your work if a restart will be needed. ".repeat(30)),
            confirm: "Confirm preview change".into(),
            danger: false,
            // Even an accidental execution cannot launch a real tool: this index
            // is intentionally invalid. The test sends no interaction events.
            action: Action::RunTool(usize::MAX),
        });
        let mut painter = CpuPainter::default();
        for frame in 0..3 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(Frame::none().fill(theme::BG))
                        .show(ctx, |ui| {
                            ui.label("PCSweep confirmation preview");
                        });
                    app.modal_ui(ctx);
                },
            );
            assert!(app.modal.is_some(), "without input a modal must stay open");
            assert!(
                !app.mutations_running(),
                "rendering a modal must not execute its action"
            );
            for (id, delta) in &output.textures_delta.set {
                painter.set_texture(*id, delta);
            }
            if frame == 2 {
                for expected in ["Cancel", "Confirm preview change"] {
                    let mut found = Vec::new();
                    for shape in &output.shapes {
                        matching_text_rects(&shape.shape, shape.clip_rect, expected, &mut found);
                    }
                    assert_eq!(
                        found.len(),
                        1,
                        "the {expected} action must be drawn exactly once"
                    );
                    let (text, clip) = found[0];
                    assert!(screen.contains_rect(text.expand(8.0)), "{expected} action must remain reachable in the {size:?} viewport; text={text:?}");
                    assert!(
                        clip.contains_rect(text),
                        "{expected} action must not be hidden by the content scroll clip"
                    );
                }
                let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
                let image = painter.render(size, &primitives);
                image
                    .save(directory.join(format!("confirmation-{}x{}.png", size[0], size[1])))
                    .unwrap();
            }
            for id in output.textures_delta.free {
                painter.textures.remove(&id);
            }
        }
    }
}
