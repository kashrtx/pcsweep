//! Headless layout regression checks and reviewable CPU-rendered UI previews.
//! Rendering fixtures poll no workers and invoke no system changes. Interaction
//! checks exercise only local selection state or cancel an inert confirmation.
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

fn navigation_label(page: Page) -> Option<&'static str> {
    Some(match page {
        Page::Dashboard => "Overview",
        Page::Cleaner => "Junk cleaner",
        Page::Analyzer => "Space analyzer",
        Page::Duplicates => "Duplicates",
        Page::Apps => "Installed apps",
        Page::Startup => "Startup apps",
        Page::Memory => "Memory and apps",
        Page::Privacy => "Privacy and ads",
        Page::Security => "Security check",
        Page::Background => "Background activity",
        Page::Ads => "Ads and tracking",
        Page::Network => "Connection health",
        Page::Power => "Battery and power",
        Page::Updates => "Updates",
        Page::Tools => "Tools",
        Page::Activity => "Activity log",
        Page::Storage => "Disk health",
        Page::WindowsControls => "Windows controls",
        Page::Speed => "Internet speed test",
        Page::FileChanges => "Windows file changes",
    })
}

/// Populate pages that previously only had empty/loading preview states. No
/// workers are polled and these paths are never read from or written to disk.
fn populate_page_fixture(app: &mut App, page: Page) {
    use crate::analyzer::{FileCat, FileRec, Node, NodeKind, ScanResult};
    use crate::duplicates::{DupFile, DupGroup};
    use crate::startup::{Loc, StartupItem};
    use std::time::Duration;
    app.cleaner.scanned_once = true;
    for result in &mut app.cleaner.results {
        *result = Some(Default::default());
    }
    for (i, size) in [220_000_000u64, 810_000_000, 76_000_000]
        .into_iter()
        .enumerate()
    {
        app.cleaner.results[i] = Some(crate::cleaner::CatResult {
            files: vec![crate::cleaner::JunkFile {
                path: format!(r"C:\Users\Preview\AppData\Local\Temp\review-{i}.tmp").into(),
                size,
                modified: None,
                created: None,
            }],
            bytes: size,
            count: 1,
            roots_found: 1,
            ..Default::default()
        });
    }
    if page == Page::Analyzer {
        let gb = 1024u64.pow(3);
        let children: Vec<Node> = [
            ("Users", 208),
            ("Program Files", 48),
            ("Windows", 41),
            ("ProgramData", 31),
            ("Program Files (x86)", 22),
        ]
        .into_iter()
        .map(|(name, size)| Node {
            name: name.into(),
            size: size * gb,
            files: size * 1000,
            kind: NodeKind::Dir,
            modified: None,
            children: Vec::new(),
        })
        .collect();
        app.analyzer.result = Some(ScanResult {
            root_path: r"C:\".into(),
            root: Node {
                name: "C:".into(),
                size: 350 * gb,
                files: 2_202_656,
                kind: NodeKind::Dir,
                modified: None,
                children,
            },
            big: vec![FileRec {
                path: r"C:\Users\Preview\Videos\recording.mp4".into(),
                lower: r"c:\users\preview\videos\recording.mp4".into(),
                size: gb,
                modified: None,
                block: None,
            }],
            exts: Vec::new(),
            cats: vec![(FileCat::Video, gb, 1)],
            elapsed: Duration::from_millis(97_500),
            denied: 23,
            dirs: 343_873,
            cancelled: false,
        });
    }
    if page == Page::Duplicates {
        app.dups.roots = vec![r"C:\Users\Preview\Videos".into()];
        app.dups.groups = Some(vec![DupGroup {
            size: 124_000_000,
            files: ["original.mp4", "backup-copy.mp4", "another-copy.mp4"]
                .into_iter()
                .map(|name| DupFile {
                    path: format!(r"C:\Users\Preview\Videos\{name}").into(),
                    modified: None,
                    block: None,
                })
                .collect(),
        }]);
    }
    if page == Page::Startup {
        app.startup.items = [
            ("Music app", "music.exe", true),
            ("Project sync", "sync.exe --background", false),
        ]
        .into_iter()
        .map(|(name, command, enabled)| StartupItem {
            key: name.into(),
            display: name.into(),
            command: command.into(),
            loc: Loc::UserRun,
            enabled,
            file: None,
        })
        .collect();
    }
}

fn render_page(page: Page, size: [u32; 2], path: &Path, pending_dns: bool) {
    let ctx = egui::Context::default();
    theme::setup_fonts(&ctx);
    theme::apply(&ctx);
    // No App::new, poll, clicks, or eframe::App::update: fixtures contain data
    // only, so rendering cannot start scans, execute commands or alter settings.
    let mut app = App::test_fixture();
    app.page = page;
    populate_page_fixture(&mut app, page);
    if pending_dns {
        app.ads = crate::ads::trial_preview_fixture();
    }
    let mut painter = CpuPainter::default();
    for frame in 0..8 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(size[0] as f32, size[1] as f32) / ctx.zoom_factor(),
            )),
            time: Some(frame as f64 * 0.1),
            ..egui::RawInput::default()
        };
        let output = ctx.run(input, |ctx| {
            theme::fit_to_viewport(ctx);
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
            app.mutations_running() == pending_dns,
            "only the inert pending DNS fixture may reserve the operation guard"
        );
        assert!(!output.shapes.is_empty(), "page must draw content");
        for (id, delta) in &output.textures_delta.set {
            painter.set_texture(*id, delta);
        }
        if frame == 7 {
            if pending_dns {
                for expected in ["Keep these DNS settings", "Restore now"] {
                    let mut found = Vec::new();
                    for shape in &output.shapes {
                        matching_text_rects(&shape.shape, shape.clip_rect, expected, &mut found);
                    }
                    assert_eq!(found.len(), 1, "{expected} must appear in the trial card");
                    let (text, clip) = found[0];
                    assert!(
                        clip.contains_rect(text),
                        "{expected} must be visible without scrolling"
                    );
                }
            }
            if let Some(selected) = navigation_label(page) {
                let mut found = Vec::new();
                for shape in &output.shapes {
                    matching_text_rects(&shape.shape, shape.clip_rect, selected, &mut found);
                }
                assert!(
                    found
                        .iter()
                        .any(|(text, clip)| text.left() < 236.0 && clip.contains_rect(*text)),
                    "selected navigation item {selected} must be visible"
                );
            }
            if page == Page::Storage {
                let mut found = Vec::new();
                for shape in &output.shapes {
                    matching_text_rects(
                        &shape.shape,
                        shape.clip_rect,
                        "Refresh storage reports",
                        &mut found,
                    );
                }
                assert!(
                    found.iter().any(|(text, clip)| clip.contains_rect(*text)),
                    "storage toolbar must not be horizontally clipped"
                );
            }
            let mut primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            scale_primitives(&mut primitives, output.pixels_per_point);
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
        ("overview", Page::Dashboard),
        ("cleaner", Page::Cleaner),
        ("analyzer", Page::Analyzer),
        ("duplicates", Page::Duplicates),
        ("startup", Page::Startup),
        ("tools", Page::Tools),
        ("activity", Page::Activity),
        ("storage", Page::Storage),
        ("windows-controls", Page::WindowsControls),
        ("internet-speed", Page::Speed),
        ("file-changes", Page::FileChanges),
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
        for size in [[900, 520], [1040, 680], [1320, 860], [1920, 1080]] {
            let path = directory.join(format!("{name}-{}x{}.png", size[0], size[1]));
            render_page(page, size, &path, false);
        }
    }
    for size in [[1040, 680], [1320, 860]] {
        let path = directory.join(format!("dns-trial-{}x{}.png", size[0], size[1]));
        render_page(Page::Ads, size, &path, true);
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

fn collect_visible_file_labels(shape: &Shape, clip: Rect, found: &mut Vec<String>) {
    match shape {
        Shape::Text(text) if text.galley.job.text.starts_with("file-") => {
            let rect = text.galley.rect.translate(text.pos.to_vec2());
            if clip.contains_rect(rect) {
                found.push(text.galley.job.text.clone());
            }
        }
        Shape::Vec(shapes) => {
            for shape in shapes {
                collect_visible_file_labels(shape, clip, found);
            }
        }
        _ => {}
    }
}

fn text_rect(output: &egui::FullOutput, expected: &str) -> Rect {
    let mut found = Vec::new();
    for clipped in &output.shapes {
        matching_text_rects(&clipped.shape, clipped.clip_rect, expected, &mut found);
    }
    let (text, clip) = found
        .into_iter()
        .find(|(text, clip)| clip.contains_rect(*text))
        .unwrap_or_else(|| panic!("{expected} must be fully visible"));
    assert!(clip.contains_rect(text));
    text
}

fn scale_primitives(primitives: &mut [ClippedPrimitive], scale: f32) {
    for primitive in primitives {
        primitive.clip_rect = Rect::from_min_max(
            primitive.clip_rect.min * scale,
            primitive.clip_rect.max * scale,
        );
        if let Primitive::Mesh(mesh) = &mut primitive.primitive {
            for vertex in &mut mesh.vertices {
                vertex.pos = egui::pos2(vertex.pos.x * scale, vertex.pos.y * scale);
            }
        }
    }
}

fn click_at(position: Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(position),
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

#[test]
fn bulk_memory_actions_stay_reachable_and_clear_only_selection_at_scaled_sizes() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui-previews");
    std::fs::create_dir_all(&directory).unwrap();
    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        let ctx = egui::Context::default();
        theme::setup_fonts(&ctx);
        theme::apply(&ctx);
        ctx.set_pixels_per_point(scale);
        let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(1040.0, 680.0));
        let mut app = App::test_fixture();
        app.page = Page::Memory;
        app.mem = crate::processes::bulk_preview_fixture();
        let mut painter = CpuPainter::default();
        let mut clear = None;
        for frame in 0..8 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(frame as f64 * 0.1),
                    ..Default::default()
                },
                |ctx| {
                    egui::SidePanel::left("memory_navigation")
                        .exact_width(236.0)
                        .resizable(false)
                        .show(ctx, |ui| app.sidebar(ui));
                    egui::CentralPanel::default()
                        .frame(
                            Frame::none()
                                .fill(theme::BG)
                                .inner_margin(Margin::symmetric(30.0, 22.0)),
                        )
                        .show(ctx, |ui| app.render_page(ui));
                },
            );
            assert!(app.modal.is_none());
            assert!(
                !app.mutations_running(),
                "previews cannot start app-close operations"
            );
            for (id, delta) in &output.textures_delta.set {
                painter.set_texture(*id, delta);
            }
            if frame == 7 {
                for expected in ["2 apps selected", "Close selected…", "Clear"] {
                    assert!(
                        screen.contains_rect(text_rect(&output, expected)),
                        "{expected} must stay on screen at {scale}x DPI"
                    );
                }
                clear = Some(text_rect(&output, "Clear").center());
                let mut primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
                for primitive in &mut primitives {
                    primitive.clip_rect = Rect::from_min_max(
                        primitive.clip_rect.min * scale,
                        primitive.clip_rect.max * scale,
                    );
                    if let Primitive::Mesh(mesh) = &mut primitive.primitive {
                        for vertex in &mut mesh.vertices {
                            vertex.pos = egui::pos2(vertex.pos.x * scale, vertex.pos.y * scale);
                        }
                    }
                }
                painter
                    .render(
                        [(1040.0 * scale) as u32, (680.0 * scale) as u32],
                        &primitives,
                    )
                    .save(directory.join(format!("memory-bulk-dpi-{}.png", (scale * 100.0) as u32)))
                    .unwrap();
            }
            for id in output.textures_delta.free {
                painter.textures.remove(&id);
            }
        }
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                events: click_at(clear.unwrap()),
                ..Default::default()
            },
            |ctx| {
                egui::SidePanel::left("memory_navigation")
                    .exact_width(236.0)
                    .resizable(false)
                    .show(ctx, |ui| app.sidebar(ui));
                egui::CentralPanel::default()
                    .frame(
                        Frame::none()
                            .fill(theme::BG)
                            .inner_margin(Margin::symmetric(30.0, 22.0)),
                    )
                    .show(ctx, |ui| app.render_page(ui));
            },
        );
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                egui::SidePanel::left("memory_navigation")
                    .exact_width(236.0)
                    .resizable(false)
                    .show(ctx, |ui| app.sidebar(ui));
                egui::CentralPanel::default()
                    .frame(
                        Frame::none()
                            .fill(theme::BG)
                            .inner_margin(Margin::symmetric(30.0, 22.0)),
                    )
                    .show(ctx, |ui| app.render_page(ui));
            },
        );
        let mut selected = Vec::new();
        for clipped in &output.shapes {
            matching_text_rects(
                &clipped.shape,
                clipped.clip_rect,
                "Close selected…",
                &mut selected,
            );
        }
        assert!(
            selected.is_empty(),
            "Clear must remove the bulk selection bar"
        );
        assert!(
            app.modal.is_none() && !app.mutations_running(),
            "clearing selection must not close any app"
        );
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
        for frame in 0..8 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(frame as f64 * 0.1),
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
            if frame == 7 {
                let primitives = ctx.tessellate(output.shapes.clone(), output.pixels_per_point);
                painter
                    .render(size, &primitives)
                    .save(directory.join(format!("confirmation-{}x{}.png", size[0], size[1])))
                    .unwrap();
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
            }
            for id in output.textures_delta.free {
                painter.textures.remove(&id);
            }
        }
    }
}

#[test]
fn cleanup_confirmation_details_scroll_at_the_dialog_edge() {
    // Mirrors a real cleanup confirmation: 21 short category lines. A nested scroll
    // area used to shrink to the text width while its panel filled the dialog,
    // which left the scrollbar partway across the details.
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(1040.0, 680.0));
    let ctx = egui::Context::default();
    theme::setup_fonts(&ctx);
    theme::apply(&ctx);
    let mut app = App::test_fixture();
    let details: Vec<String> = (1..=21)
        .map(|i| format!("Category {i}: {i}.0 MB ({i} files)"))
        .collect();
    app.modal = Some(Modal {
        title: "Clean 8.07 GB?".into(),
        body: "59,645 files from 21 categories will be permanently deleted.".into(),
        details: details.clone(),
        note: Some(r"Includes C:\Users\Example\AppData\Local\Temp and similar folders.".into()),
        confirm: "Delete permanently".into(),
        danger: true,
        action: Action::RunTool(usize::MAX),
    });
    let mut last = None;
    let mut painter = CpuPainter::default();
    for frame in 0..6 {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(frame as f64 * 0.1),
                ..Default::default()
            },
            |ctx| app.modal_ui(ctx),
        );
        for (id, delta) in &output.textures_delta.set {
            painter.set_texture(*id, delta);
        }
        last = Some(output);
    }
    let output = last.expect("frames were rendered");
    assert!(
        app.modal.is_some(),
        "rendering alone must not confirm or cancel"
    );
    let panel = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            Shape::Rect(rect) if rect.fill == theme::BG => Some(rect.rect),
            _ => None,
        })
        .max_by(|a, b| a.width().total_cmp(&b.width()))
        .expect("the details panel is drawn");
    let mut checked = 0;
    for line in &details {
        let mut found = Vec::new();
        for clipped in &output.shapes {
            matching_text_rects(&clipped.shape, clipped.clip_rect, line, &mut found);
        }
        for (text, clip) in found {
            if !clip.intersects(text) {
                continue;
            }
            assert!(
                clip.right() >= panel.right() - 1.0,
                "{line} is clipped narrower than its panel: {clip:?} vs {panel:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "at least one details line must be visible");
    let bars: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            Shape::Rect(rect)
                if rect.rect.width() >= 5.0
                    && rect.rect.width() <= 12.0
                    && rect.rect.height() > 32.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .collect();
    assert!(
        !bars.is_empty(),
        "overflowing confirmation must expose a scrollbar"
    );
    for bar in bars {
        assert!(bar.left() >= panel.right() - 2.0 && bar.right() <= panel.right() + 20.0,
            "scrollbar must occupy the right gutter, not the middle of the panel: {bar:?}, panel={panel:?}");
    }
    let primitives = ctx.tessellate(output.shapes.clone(), output.pixels_per_point);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui-previews");
    std::fs::create_dir_all(&directory).unwrap();
    painter
        .render([1040, 680], &primitives)
        .save(directory.join("cleanup-confirmation-scrollbar.png"))
        .unwrap();

    let pointer = egui::pos2(panel.center().x, 340.0);
    let mut last = None;
    for frame in 0..8 {
        let events = if frame == 0 {
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -10_000.0),
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        } else {
            Vec::new()
        };
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(1.0 + frame as f64 * 0.1),
                events,
                ..Default::default()
            },
            |ctx| app.modal_ui(ctx),
        );
        last = Some(output);
    }
    let output = last.unwrap();
    text_rect(&output, "Category 21: 21.0 MB (21 files)");
    let cancel = text_rect(&output, "Cancel").center();
    text_rect(&output, "Delete permanently");
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(screen),
            events: click_at(cancel),
            ..Default::default()
        },
        |ctx| app.modal_ui(ctx),
    );
    assert!(
        app.modal.is_none(),
        "Cancel must remain reachable after scrolling to the end"
    );
    assert!(
        !app.mutations_running(),
        "canceling an inert confirmation cannot execute its action"
    );
}

#[test]
fn full_page_file_review_fills_the_window_and_filters_without_rebuilding_rows() {
    let size = [1040u32, 680u32];
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32));
    let ctx = egui::Context::default();
    theme::setup_fonts(&ctx);
    theme::apply(&ctx);
    let mut app = App::test_fixture();
    let file_count = 80_768u64;
    let files: Vec<crate::cleaner::JunkFile> = (0..file_count)
        .map(|i| crate::cleaner::JunkFile {
            path: format!(
                r"C:\Users\Example\AppData\Local\Temp\folder-{}\file-{i}.tmp",
                i % 7
            )
            .into(),
            size: 1024 * (i + 1),
            modified: None,
            created: None,
        })
        .collect();
    let bytes = files.iter().map(|f| f.size).sum();
    app.cleaner.results[0] = Some(crate::cleaner::CatResult {
        files,
        bytes,
        count: file_count,
        roots_found: 1,
        ..Default::default()
    });
    app.cleaner.review = Some(crate::cleaner::FileReview::new(0));
    app.page = Page::Cleaner;
    let mut painter = CpuPainter::default();
    for (pass, filter) in ["", "folder-3"].into_iter().enumerate() {
        if let Some(review) = app.cleaner.review.as_mut() {
            review.filter = filter.to_string();
        }
        for frame in 0..6 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    time: Some(frame as f64 * 0.1),
                    ..Default::default()
                },
                |ctx| {
                    egui::SidePanel::left("review_navigation")
                        .exact_width(236.0)
                        .resizable(false)
                        .show(ctx, |ui| {
                            ui.label("PCSweep");
                        });
                    egui::CentralPanel::default()
                        .frame(Frame::none().fill(theme::BG))
                        .show(ctx, |ui| app.render_page(ui));
                },
            );
            assert!(
                app.cleaner.review.is_some(),
                "rendering must not close the review"
            );
            assert!(
                app.modal.is_none(),
                "rendering must not open a confirmation"
            );
            for (id, delta) in &output.textures_delta.set {
                painter.set_texture(*id, delta);
            }
            if frame == 5 {
                let visible = |expected: &str| {
                    let mut found = Vec::new();
                    for shape in &output.shapes {
                        matching_text_rects(&shape.shape, shape.clip_rect, expected, &mut found);
                    }
                    found.iter().any(|(text, clip)| clip.contains_rect(*text))
                };
                assert!(
                    visible(&format!("{}All categories", crate::widgets::ic::BACK)),
                    "the Back control must stay visible"
                );
                if pass == 0 {
                    assert!(
                        visible("file-80767.tmp"),
                        "the largest file is listed first"
                    );
                } else {
                    assert!(visible("file-80762.tmp"), "matching files are listed");
                    assert!(!visible("file-80767.tmp"), "the filter hides other folders");
                }
                let mut rows = Vec::new();
                for shape in &output.shapes {
                    collect_visible_file_labels(&shape.shape, shape.clip_rect, &mut rows);
                }
                assert!(
                    rows.len() >= 5,
                    "the review must show substantially more than two rows; rendered {}",
                    rows.len()
                );
                assert!(
                    rows.len() < 80,
                    "the list must virtualize 80,768 files instead of painting them all"
                );
                let primitives = ctx.tessellate(output.shapes.clone(), output.pixels_per_point);
                let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("ui-previews");
                std::fs::create_dir_all(&directory).unwrap();
                painter
                    .render(size, &primitives)
                    .save(directory.join(format!("cleaner-review-{pass}.png")))
                    .unwrap();
            }
            for id in output.textures_delta.free {
                painter.textures.remove(&id);
            }
        }
    }
}

#[test]
fn cleanup_history_page_lists_every_outcome_from_a_fixture() {
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(1040.0, 680.0));
    let ctx = egui::Context::default();
    theme::setup_fonts(&ctx);
    theme::apply(&ctx);
    let mut app = App::test_fixture();
    app.history = crate::journal::preview_fixture();
    app.page = Page::Activity;
    let mut last = None;
    for frame in 0..6 {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(frame as f64 * 0.1),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(Frame::none().fill(theme::BG))
                    .show(ctx, |ui| app.render_page(ui));
            },
        );
        last = Some(output);
    }
    let output = last.expect("frames were rendered");
    assert!(
        app.history.open,
        "rendering must not leave the history view"
    );
    for expected in [
        "Cleanup history",
        "setup.log",
        "locked.tmp",
        "Not deleted",
        "Uncertain",
    ] {
        let mut found = Vec::new();
        for shape in &output.shapes {
            matching_text_rects(&shape.shape, shape.clip_rect, expected, &mut found);
        }
        assert!(
            found.iter().any(|(text, clip)| clip.contains_rect(*text)),
            "{expected} must be visible"
        );
    }
}
