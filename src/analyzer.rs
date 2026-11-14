use crate::app::{Action, App, Modal, Origin, Page, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_age, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontId, Label, Layout, Rect, RichText, Rounding,
    ScrollArea, Sense, Ui,
};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Files at least this big get their own entry in the folder tree.
const CHILD_FILE: u64 = 1024 * 1024;
/// Files at least this big are tracked for the "Largest" and "Old" views.
const BIG_FILE: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    SmallFiles,
    Denied,
}

pub struct Node {
    pub name: String,
    pub size: u64,
    pub files: u64,
    pub kind: NodeKind,
    pub modified: Option<SystemTime>,
    pub children: Vec<Node>,
}

pub struct FileRec {
    pub path: PathBuf,
    pub lower: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub block: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileCat {
    Video,
    Image,
    Audio,
    Document,
    Archive,
    DiskImage,
    Installer,
    Program,
    Code,
    Game,
    Other,
}

impl FileCat {
    pub fn label(self) -> &'static str {
        match self {
            FileCat::Video => "Videos",
            FileCat::Image => "Pictures",
            FileCat::Audio => "Music and audio",
            FileCat::Document => "Documents",
            FileCat::Archive => "Archives",
            FileCat::DiskImage => "Disk images and virtual machines",
            FileCat::Installer => "Installers",
            FileCat::Program => "Programs and libraries",
            FileCat::Code => "Code and data",
            FileCat::Game => "Game data",
            FileCat::Other => "Other files",
        }
    }
    pub fn color(self) -> Color32 {
        match self {
            FileCat::Video => PALETTE[4],
            FileCat::Image => PALETTE[2],
            FileCat::Audio => PALETTE[7],
            FileCat::Document => PALETTE[1],
            FileCat::Archive => PALETTE[3],
            FileCat::DiskImage => PALETTE[6],
            FileCat::Installer => PALETTE[5],
            FileCat::Program => PALETTE[0],
            FileCat::Code => Color32::from_rgb(120, 140, 170),
            FileCat::Game => Color32::from_rgb(170, 110, 200),
            FileCat::Other => Color32::from_rgb(92, 100, 120),
        }
    }
}

pub fn classify(ext: &str) -> FileCat {
    match ext {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg" | "ts" | "m2ts" | "3gp" => FileCat::Video,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff" | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" | "svg" | "ico" => FileCat::Image,
        "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "wma" | "opus" | "aiff" => FileCat::Audio,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "txt" | "rtf" | "epub" | "md" | "csv" | "pst" | "ost" => FileCat::Document,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "cab" | "tgz" => FileCat::Archive,
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "vdi" | "qcow2" | "wim" | "esd" | "avhdx" => FileCat::DiskImage,
        "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "msp" | "msu" => FileCat::Installer,
        "exe" | "dll" | "sys" | "so" | "lib" | "ocx" | "node" | "pyd" | "jar" => FileCat::Program,
        "rs" | "c" | "cpp" | "h" | "cs" | "js" | "ts" | "py" | "java" | "json" | "xml" | "db" | "sqlite" | "sql" | "log" | "pdb" | "obj" | "rlib" | "o" | "pack" | "idx" => FileCat::Code,
        "pak" | "bsa" | "ba2" | "vpk" | "bundle" | "assets" | "ucas" | "utoc" | "forge" | "big" | "dat" => FileCat::Game,
        _ => FileCat::Other,
    }
}

fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 12 => ext.to_lowercase(),
        _ => "(none)".to_string(),
    }
}

pub struct ExtStat {
    pub ext: String,
    pub bytes: u64,
    pub count: u64,
    pub cat: FileCat,
}

pub struct ScanResult {
    pub root_path: PathBuf,
    pub root: Node,
    pub big: Vec<FileRec>,
    pub exts: Vec<ExtStat>,
    pub cats: Vec<(FileCat, u64, u64)>,
    pub elapsed: Duration,
    pub denied: u64,
    pub dirs: u64,
    pub cancelled: bool,
}

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

struct Shared<'a> {
    progress: &'a Progress,
    big: Mutex<Vec<FileRec>>,
    ext: Mutex<HashMap<String, (u64, u64)>>,
    denied: AtomicU64,
}

fn scan_dir(path: &Path, name: String, sh: &Shared<'_>, depth: u32) -> Node {
    let mut node = Node {
        name,
        size: 0,
        files: 0,
        kind: NodeKind::Dir,
        modified: None,
        children: Vec::new(),
    };
    if depth > 200 || sh.progress.cancel.load(Ordering::Relaxed) {
        return node;
    }
    let rd = match fs::read_dir(path) {
        Ok(r) => r,
        Err(_) => {
            sh.denied.fetch_add(1, Ordering::Relaxed);
            node.kind = NodeKind::Denied;
            return node;
        }
    };
    let mut subdirs: Vec<(PathBuf, String)> = Vec::new();
    let mut small_size = 0u64;
    let mut small_count = 0u64;
    let mut local_ext: HashMap<String, (u64, u64)> = HashMap::new();
    let mut local_big: Vec<FileRec> = Vec::new();
    for entry in rd.flatten() {
        let Ok(md) = entry.metadata() else { continue };
        let attrs = md.file_attributes();
        // Never follow junctions or symlinks (avoids loops and double counting).
        if attrs & util::ATTR_REPARSE != 0 || md.file_type().is_symlink() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().into_owned();
        if md.is_dir() {
            subdirs.push((entry.path(), fname));
            continue;
        }
        let size = if attrs & util::CLOUD_MASK != 0 { 0 } else { md.len() };
        let modified = md.modified().ok();
        node.modified = node.modified.max(modified);
        node.files += 1;
        node.size += size;
        let e = local_ext.entry(ext_of(&fname)).or_insert((0, 0));
        e.0 += size;
        e.1 += 1;
        if size >= BIG_FILE {
            let path = entry.path();
            local_big.push(FileRec {
                lower: path.to_string_lossy().to_lowercase(),
                path,
                size,
                modified,
                block: None,
            });
        }
        if size >= CHILD_FILE {
            node.children.push(Node {
                name: fname,
                size,
                files: 1,
                kind: NodeKind::File,
                modified,
                children: Vec::new(),
            });
        } else {
            small_size += size;
            small_count += 1;
        }
    }

    let p = sh.progress;
    p.files.fetch_add(node.files, Ordering::Relaxed);
    p.bytes.fetch_add(node.size, Ordering::Relaxed);
    p.dirs.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut c) = p.current.try_lock() {
        *c = path.display().to_string();
    }
    if !local_ext.is_empty() {
        if let Ok(mut m) = sh.ext.lock() {
            for (k, (b, n)) in local_ext {
                let e = m.entry(k).or_insert((0, 0));
                e.0 += b;
                e.1 += n;
            }
        }
    }
    if !local_big.is_empty() {
        if let Ok(mut v) = sh.big.lock() {
            v.extend(local_big);
        }
    }
    if small_count > 0 {
        node.children.push(Node {
            name: format!("{} smaller files", fmt_num(small_count)),
            size: small_size,
            files: small_count,
            kind: NodeKind::SmallFiles,
            modified: None,
            children: Vec::new(),
        });
    }

    let subs: Vec<Node> = subdirs
        .into_par_iter()
        .map(|(p, n)| scan_dir(&p, n, sh, depth + 1))
        .collect();
    for s in subs {
        node.size += s.size;
        node.files += s.files;
        node.modified = node.modified.max(s.modified);
        node.children.push(s);
    }
    node.children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    node
}

pub fn scan(root_path: PathBuf, progress: Arc<Progress>) -> ScanResult {
    let start = Instant::now();
    let sh = Shared {
        progress: &*progress,
        big: Mutex::new(Vec::new()),
        ext: Mutex::new(HashMap::new()),
        denied: AtomicU64::new(0),
    };
    let root = scan_dir(&root_path, root_path.display().to_string(), &sh, 0);
    let denied = sh.denied.load(Ordering::Relaxed);
    let mut big = sh.big.into_inner().unwrap_or_else(|e| e.into_inner());
    big.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    for f in big.iter_mut() {
        f.block = util::block_reason_fast(&f.path);
    }
    let ext_map = sh.ext.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut cat_map: HashMap<FileCat, (u64, u64)> = HashMap::new();
    let mut exts: Vec<ExtStat> = ext_map
        .into_iter()
        .map(|(ext, (bytes, count))| {
            let cat = classify(&ext);
            let e = cat_map.entry(cat).or_insert((0, 0));
            e.0 += bytes;
            e.1 += count;
            ExtStat { ext, bytes, count, cat }
        })
        .collect();
    exts.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
    let mut cats: Vec<(FileCat, u64, u64)> = cat_map.into_iter().map(|(c, (b, n))| (c, b, n)).collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1));
    ScanResult {
        root_path,
        root,
        big,
        exts,
        cats,
        elapsed: start.elapsed(),
        denied,
        dirs: progress.dirs.load(Ordering::Relaxed),
        cancelled: progress.cancel.load(Ordering::Relaxed),
    }
}

fn node_at<'a>(root: &'a Node, nav: &[usize]) -> &'a Node {
    let mut n = root;
    for &i in nav {
        match n.children.get(i) {
            Some(c) => n = c,
            None => break,
        }
    }
    n
}

fn path_at(root_path: &Path, root: &Node, nav: &[usize]) -> PathBuf {
    let mut p = root_path.to_path_buf();
    let mut n = root;
    for &i in nav {
        match n.children.get(i) {
            Some(c) => {
                p.push(&c.name);
                n = c;
            }
            None => break,
        }
    }
    p
}

// ---------------------------------------------------------------- treemap

fn worst(row: &[f64], short: f64) -> f64 {
    let s: f64 = row.iter().sum();
    let mx = row.iter().cloned().fold(0.0, f64::max);
    let mn = row.iter().cloned().fold(f64::INFINITY, f64::min);
    if s <= 0.0 || mn <= 0.0 {
        return f64::INFINITY;
    }
    let s2 = s * s;
    let w2 = short * short;
    (w2 * mx / s2).max(s2 / (w2 * mn))
}

/// Squarified treemap layout.
fn squarify(items: &[(usize, f64)], rect: Rect) -> Vec<(usize, Rect)> {
    let total: f64 = items.iter().map(|i| i.1).sum();
    let mut out = Vec::with_capacity(items.len());
    if total <= 0.0 || rect.width() <= 1.0 || rect.height() <= 1.0 {
        return out;
    }
    let scale = (rect.width() as f64 * rect.height() as f64) / total;
    let areas: Vec<f64> = items.iter().map(|i| i.1 * scale).collect();
    let (mut x, mut y) = (rect.min.x as f64, rect.min.y as f64);
    let (mut w, mut h) = (rect.width() as f64, rect.height() as f64);
    let mut i = 0;
    while i < areas.len() {
        let short = w.min(h);
        if short <= 0.5 {
            break;
        }
        let mut end = i + 1;
        let mut best = worst(&areas[i..end], short);
        while end < areas.len() {
            let candidate = worst(&areas[i..end + 1], short);
            if candidate <= best {
                best = candidate;
                end += 1;
            } else {
                break;
            }
        }
        let row = &areas[i..end];
        let sum: f64 = row.iter().sum();
        if w >= h {
            let strip = sum / h;
            let mut yy = y;
            for (k, a) in row.iter().enumerate() {
                let rh = a / strip;
                out.push((
                    items[i + k].0,
                    Rect::from_min_size(pos2(x as f32, yy as f32), vec2(strip as f32, rh as f32)),
                ));
                yy += rh;
            }
            x += strip;
            w -= strip;
        } else {
            let strip = sum / w;
            let mut xx = x;
            for (k, a) in row.iter().enumerate() {
                let rw = a / strip;
                out.push((
                    items[i + k].0,
                    Rect::from_min_size(pos2(xx as f32, y as f32), vec2(rw as f32, strip as f32)),
                ));
                xx += rw;
            }
            y += strip;
            h -= strip;
        }
        i = end;
    }
    out
}

fn node_color(child: &Node, k: usize) -> Color32 {
    match child.kind {
        NodeKind::Dir => PALETTE[k % PALETTE.len()],
        NodeKind::File => classify(&ext_of(&child.name)).color(),
        NodeKind::SmallFiles => Color32::from_rgb(62, 70, 88),
        NodeKind::Denied => Color32::from_rgb(48, 52, 64),
    }
}

/// Draws the space map for a folder. Returns the child index the user clicked into.
fn treemap(ui: &mut Ui, node: &Node, height: f32) -> Option<usize> {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, Rounding::same(12.0), CARD);
    let items: Vec<(usize, f64)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| c.size > 0)
        .take(150)
        .map(|(i, c)| (i, c.size as f64))
        .collect();
    if items.is_empty() {
        painter.text(rect.center(), Align2::CENTER_CENTER, "This folder is empty", FontId::proportional(14.0), TEXT_DIM);
        return None;
    }
    let mut clicked = None;
    for (k, (idx, r)) in squarify(&items, rect.shrink(4.0)).into_iter().enumerate() {
        let child = &node.children[idx];
        let r = r.shrink(1.5);
        if r.width() < 1.0 || r.height() < 1.0 {
            continue;
        }
        let base = node_color(child, k);
        let resp = ui.interact(r, ui.id().with(("treemap_cell", idx)), Sense::click());
        let hovered = resp.hovered();
        let fill = if hovered { mix(base, Color32::WHITE, 0.18) } else { base };
        painter.rect_filled(r, Rounding::same(6.0), fill);
        if r.width() > 64.0 && r.height() > 40.0 {
            let p = painter.with_clip_rect(r.shrink(4.0));
            p.text(r.left_top() + vec2(9.0, 8.0), Align2::LEFT_TOP, &child.name, widgets::semibold(13.5), Color32::WHITE);
            p.text(
                r.left_top() + vec2(9.0, 27.0),
                Align2::LEFT_TOP,
                fmt_bytes(child.size),
                FontId::proportional(12.5),
                mix(Color32::WHITE, base, 0.3),
            );
        } else if r.width() > 46.0 && r.height() > 20.0 {
            let p = painter.with_clip_rect(r.shrink(3.0));
            p.text(r.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, &child.name, FontId::proportional(11.5), Color32::WHITE);
        }
        let enterable = child.kind == NodeKind::Dir && !child.children.is_empty();
        let tip = format!(
            "{}\n{} in {} files{}",
            child.name,
            fmt_bytes(child.size),
            fmt_num(child.files),
            if enterable { "\nClick to open" } else { "" }
        );
        let resp = resp.on_hover_text(tip);
        if enterable {
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                clicked = Some(idx);
            }
        }
    }
    clicked
}

// ---------------------------------------------------------------- state & page

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ATab {
    Folders,
    Largest,
    Types,
    Old,
}

pub struct AnalyzerState {
    pub scan: Task<ScanResult>,
    pub progress: Option<Arc<Progress>>,
    pub scan_root: Option<PathBuf>,
    pub started: Option<Instant>,
    pub result: Option<ScanResult>,
    pub nav: Vec<usize>,
    pub tab: ATab,
    pub filter: String,
    pub selected: HashSet<PathBuf>,
    pub removed: HashSet<PathBuf>,
    pub old_months: u32,
    pub old_min: u64,
    pub recycle: Task<util::RecycleReport>,
}

impl Default for AnalyzerState {
    fn default() -> Self {
        Self {
            scan: Task::default(),
            progress: None,
            scan_root: None,
            started: None,
            result: None,
            nav: Vec::new(),
            tab: ATab::Folders,
            filter: String::new(),
            selected: HashSet::new(),
            removed: HashSet::new(),
            old_months: 12,
            old_min: 100 * 1024 * 1024,
            recycle: Task::default(),
        }
    }
}

enum AAct {
    Scan(PathBuf),
    PickFolder,
    Cancel,
    Tab(ATab),
    Up,
    Truncate(usize),
    Enter(usize),
    Show(PathBuf),
    Open(PathBuf),
    Copy(String),
    Select(PathBuf, bool),
    SelectMany(Vec<PathBuf>),
    ClearSel,
    AskRecycle(Vec<(PathBuf, u64)>),
    OldMonths(u32),
    OldMin(u64),
    Filter(String),
}

impl App {
    pub fn start_analyze(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.page = Page::Analyzer;
        if self.analyzer.scan.running() {
            self.toast(ToastKind::Info, "A scan is already running.");
            return;
        }
        let progress = Arc::new(Progress::default());
        let p2 = progress.clone();
        let root = path.clone();
        self.analyzer.scan = Task::spawn(ctx, move || scan(root, p2));
        self.analyzer.progress = Some(progress);
        self.analyzer.scan_root = Some(path.clone());
        self.analyzer.started = Some(Instant::now());
        self.analyzer.result = None;
        self.analyzer.nav.clear();
        self.analyzer.selected.clear();
        self.analyzer.removed.clear();
        self.analyzer.filter.clear();
    }

    pub fn poll_analyzer(&mut self) {
        if let Some(r) = self.analyzer.scan.poll() {
            match r {
                Ok(res) => {
                    util::log(format!(
                        "Analyzed {}: {} in {} files ({:.1}s)",
                        res.root_path.display(),
                        fmt_bytes(res.root.size),
                        fmt_num(res.root.files),
                        res.elapsed.as_secs_f32()
                    ));
                    self.analyzer.result = Some(res);
                }
                Err(e) => self.toast(ToastKind::Error, format!("Scan failed: {e}")),
            }
            self.analyzer.progress = None;
        }
        if let Some(r) = self.analyzer.recycle.poll() {
            match r {
                Ok(rep) => {
                    let n = rep.done.len();
                    for p in rep.done {
                        self.analyzer.selected.remove(&p);
                        self.analyzer.removed.insert(p);
                    }
                    if n > 0 {
                        self.toast(ToastKind::Success, format!("Moved {} item(s), {}, to the Recycle Bin", n, fmt_bytes(rep.bytes)));
                    }
                    if let Some((p, why)) = rep.failed.first() {
                        self.toast(
                            ToastKind::Error,
                            format!("{} item(s) not moved. {}: {}", rep.failed.len(), p.display(), why),
                        );
                    }
                    self.sys.refresh_drives();
                }
                Err(e) => self.toast(ToastKind::Error, format!("Recycle failed: {e}")),
            }
        }
    }

    pub fn page_analyzer(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        self.render_analyzer(ui, &mut acts);
        for a in acts {
            match a {
                AAct::Scan(p) => self.start_analyze(p, &ctx),
                AAct::PickFolder => {
                    if let Some(p) = rfd::FileDialog::new().set_title("Choose a folder to analyze").pick_folder() {
                        self.start_analyze(p, &ctx);
                    }
                }
                AAct::Cancel => {
                    if let Some(p) = &self.analyzer.progress {
                        p.cancel.store(true, Ordering::Relaxed);
                    }
                }
                AAct::Tab(t) => self.analyzer.tab = t,
                AAct::Up => {
                    self.analyzer.nav.pop();
                }
                AAct::Truncate(n) => self.analyzer.nav.truncate(n),
                AAct::Enter(i) => self.analyzer.nav.push(i),
                AAct::Show(p) => util::show_in_explorer(&p),
                AAct::Open(p) => util::open_path(&p),
                AAct::Copy(s) => ctx.output_mut(|o| o.copied_text = s),
                AAct::Select(p, on) => {
                    if on {
                        self.analyzer.selected.insert(p);
                    } else {
                        self.analyzer.selected.remove(&p);
                    }
                }
                AAct::SelectMany(v) => self.analyzer.selected.extend(v),
                AAct::ClearSel => self.analyzer.selected.clear(),
                AAct::AskRecycle(items) => self.ask_recycle(items, Origin::Analyzer),
                AAct::OldMonths(m) => self.analyzer.old_months = m,
                AAct::OldMin(b) => self.analyzer.old_min = b,
                AAct::Filter(s) => self.analyzer.filter = s,
            }
        }
    }

    /// Shared confirmation for moving user files to the Recycle Bin.
    pub fn ask_recycle(&mut self, items: Vec<(PathBuf, u64)>, origin: Origin) {
        let items: Vec<(PathBuf, u64)> = items
            .into_iter()
            .filter(|(p, _)| util::block_reason_fast(p).is_none())
            .collect();
        if items.is_empty() {
            self.toast(ToastKind::Info, "Nothing to move. Protected items can't be selected.");
            return;
        }
        let total: u64 = items.iter().map(|i| i.1).sum();
        let mut details: Vec<String> = items
            .iter()
            .take(12)
            .map(|(p, s)| format!("{}  ({})", p.display(), fmt_bytes(*s)))
            .collect();
        if items.len() > 12 {
            details.push(format!("…and {} more", items.len() - 12));
        }
        let appdata = items.iter().any(|(p, _)| util::in_appdata(p));
        let any_dir = items.iter().any(|(p, _)| p.is_dir());
        let mut notes = Vec::new();
        if appdata {
            notes.push("Some items are inside AppData and may belong to an installed app.");
        }
        if any_dir {
            notes.push("Whole folders are included.");
        }
        self.modal = Some(Modal {
            title: format!("Move {} to the Recycle Bin?", fmt_bytes(total)),
            body: format!(
                "{} item(s) will be moved to the Recycle Bin. You can restore them from there. Space is freed once you empty the Recycle Bin.",
                items.len()
            ),
            details,
            note: if notes.is_empty() { None } else { Some(notes.join(" ")) },
            confirm: "Move to Recycle Bin".into(),
            danger: true,
            action: Action::Recycle { items, origin },
        });
    }

    fn render_analyzer(&self, ui: &mut Ui, acts: &mut Vec<AAct>) {
        let a = &self.analyzer;
        widgets::page_header(
            ui,
            "Space analyzer",
            "A map of exactly where your space goes, plus the biggest files and the ones you haven't touched in ages.",
        );
        let scanning = a.scan.running();

        // Source picker
        ui.horizontal_wrapped(|ui| {
            for d in &self.sys.drives {
                let label = format!("{}  {} free of {}", d.letter(), fmt_bytes(d.free), fmt_bytes(d.total));
                let selected = a
                    .scan_root
                    .as_ref()
                    .map(|r| util::norm(r) == util::norm(Path::new(&d.mount)))
                    .unwrap_or(false);
                if ui
                    .add_enabled(!scanning, widgets::chip(widgets::icon_text(ic::DRIVE, &label, 14.0, TEXT), selected))
                    .clicked()
                {
                    acts.push(AAct::Scan(PathBuf::from(&d.mount)));
                }
            }
            if ui
                .add_enabled(!scanning, widgets::chip(widgets::icon_text(ic::FOLDER, "Your user folder", 14.0, TEXT), false))
                .clicked()
            {
                acts.push(AAct::Scan(util::folders().profile.clone()));
            }
            if ui
                .add_enabled(!scanning, widgets::chip(widgets::icon_text(ic::SEARCH, "Choose a folder", 14.0, TEXT), false))
                .clicked()
            {
                acts.push(AAct::PickFolder);
            }
        });
        ui.add_space(8.0);

        if scanning {
            if let Some(p) = &a.progress {
                widgets::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let root = a.scan_root.as_ref().map(|r| r.display().to_string()).unwrap_or_default();
                        ui.label(widgets::strong(format!("Mapping {root}"), 17.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(widgets::ghost("Stop")).clicked() {
                                acts.push(AAct::Cancel);
                            }
                        });
                    });
                    let secs = a.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                    ui.label(RichText::new(fmt_bytes(p.bytes.load(Ordering::Relaxed))).font(widgets::light(40.0)).color(TEXT));
                    ui.label(widgets::dim(format!(
                        "{} files in {} folders so far, {}s",
                        fmt_num(p.files.load(Ordering::Relaxed)),
                        fmt_num(p.dirs.load(Ordering::Relaxed)),
                        secs
                    )));
                    let cur = p.current.lock().map(|s| s.clone()).unwrap_or_default();
                    ui.add(Label::new(widgets::faint(cur)).truncate());
                });
            }
            return;
        }

        let Some(res) = &a.result else {
            widgets::card(ui, |ui| {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(widgets::icon(ic::PIE, 40.0, ACCENT));
                    ui.add_space(6.0);
                    ui.label(widgets::strong("Pick a drive or folder to map", 18.0));
                    ui.label(widgets::dim(
                        "Scanning is read-only. A full C: drive usually takes 10 to 60 seconds on an SSD.",
                    ));
                });
                ui.add_space(20.0);
            });
            return;
        };

        // Summary line
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(fmt_bytes(res.root.size)).font(widgets::light(30.0)).color(TEXT));
            ui.label(widgets::dim(format!(
                "in {} files and {} folders, mapped in {:.1}s.",
                fmt_num(res.root.files),
                fmt_num(res.dirs),
                res.elapsed.as_secs_f32()
            )));
            if res.cancelled {
                widgets::badge(ui, "Stopped early, partial results", WARN);
            }
            if res.denied > 0 {
                widgets::badge(ui, &format!("{} folders not readable", fmt_num(res.denied)), TEXT_FAINT).on_hover_text(
                    "Windows blocks some system folders. Restarting as administrator lets PCSweep read more of them.",
                );
            }
            if let Some(d) = self.sys.drives.iter().find(|d| util::norm(Path::new(&d.mount)) == util::norm(&res.root_path)) {
                let hidden = d.used().saturating_sub(res.root.size);
                if hidden > 1024 * 1024 * 1024 {
                    widgets::badge(ui, &format!("{} used by Windows and hidden data", fmt_bytes(hidden)), TEXT_FAINT).on_hover_text(
                        "Space the scan can't see: restore points, the system reserve and protected folders. Tools can trim some of it.",
                    );
                }
            }
        });
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            for (t, label) in [
                (ATab::Folders, "Folders"),
                (ATab::Largest, "Largest files"),
                (ATab::Old, "Old and unused"),
                (ATab::Types, "File types"),
            ] {
                if ui.add(widgets::chip(label, a.tab == t)).clicked() {
                    acts.push(AAct::Tab(t));
                }
            }
        });
        ui.add_space(6.0);

        match a.tab {
            ATab::Folders => self.render_folders(ui, res, acts),
            ATab::Largest => {
                let filter = a.filter.to_lowercase();
                let idx: Vec<usize> = (0..res.big.len())
                    .filter(|&i| !a.removed.contains(&res.big[i].path))
                    .filter(|&i| filter.is_empty() || res.big[i].lower.contains(&filter))
                    .take(1000)
                    .collect();
                ui.horizontal(|ui| {
                    let mut f = a.filter.clone();
                    if widgets::search_box(ui, &mut f, "Filter by name or folder", 280.0).changed() {
                        acts.push(AAct::Filter(f));
                    }
                    ui.label(widgets::faint(format!("Files of 10 MB and up, {} shown", fmt_num(idx.len() as u64))));
                });
                self.render_file_list(ui, res, &idx, acts);
            }
            ATab::Old => {
                let min_days = a.old_months as f64 * 30.4;
                let idx: Vec<usize> = (0..res.big.len())
                    .filter(|&i| {
                        let f = &res.big[i];
                        f.size >= a.old_min
                            && !a.removed.contains(&f.path)
                            && f.modified.map(|m| util::age_days(m) >= min_days).unwrap_or(false)
                    })
                    .take(1000)
                    .collect();
                ui.horizontal_wrapped(|ui| {
                    ui.label(widgets::dim("Not changed in"));
                    for (m, label) in [(6, "6 months"), (12, "1 year"), (24, "2 years"), (36, "3 years")] {
                        if ui.add(widgets::chip(label, a.old_months == m)).clicked() {
                            acts.push(AAct::OldMonths(m));
                        }
                    }
                    ui.add_space(12.0);
                    ui.label(widgets::dim("At least"));
                    for (b, label) in [(10u64 << 20, "10 MB"), (100 << 20, "100 MB"), (1 << 30, "1 GB")] {
                        if ui.add(widgets::chip(label, a.old_min == b)).clicked() {
                            acts.push(AAct::OldMin(b));
                        }
                    }
                });
                let total: u64 = idx.iter().map(|&i| res.big[i].size).sum();
                ui.label(widgets::faint(format!(
                    "{} files, {} in total. Based on when each file was last changed. Files in Downloads are shown for reference but are never removed.",
                    fmt_num(idx.len() as u64),
                    fmt_bytes(total)
                )));
                self.render_file_list(ui, res, &idx, acts);
            }
            ATab::Types => self.render_types(ui, res),
        }
    }

    fn render_folders(&self, ui: &mut Ui, res: &ScanResult, acts: &mut Vec<AAct>) {
        let a = &self.analyzer;
        let node = node_at(&res.root, &a.nav);
        let cur_path = path_at(&res.root_path, &res.root, &a.nav);

        // Breadcrumb
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!a.nav.is_empty(), widgets::small(widgets::icon_text(ic::BACK, "Up", 13.0, TEXT)))
                .clicked()
            {
                acts.push(AAct::Up);
            }
            let root_label = res.root_path.display().to_string();
            if ui
                .add(Label::new(RichText::new(root_label).color(if a.nav.is_empty() { TEXT } else { ACCENT })).sense(Sense::click()))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                acts.push(AAct::Truncate(0));
            }
            let mut n = &res.root;
            for (depth, &i) in a.nav.iter().enumerate() {
                let Some(c) = n.children.get(i) else { break };
                n = c;
                ui.label(widgets::icon(ic::CHEVRON, 10.0, TEXT_FAINT));
                let last = depth + 1 == a.nav.len();
                if ui
                    .add(Label::new(RichText::new(&c.name).color(if last { TEXT } else { ACCENT })).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    acts.push(AAct::Truncate(depth + 1));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(widgets::small(widgets::icon_text(ic::OPEN, "Open in Explorer", 13.0, TEXT))).clicked() {
                    acts.push(AAct::Open(cur_path.clone()));
                }
            });
        });

        let map_h = (ui.available_height() * 0.45).clamp(220.0, 420.0);
        if let Some(i) = treemap(ui, node, map_h) {
            acts.push(AAct::Enter(i));
        }
        ui.add_space(6.0);

        let parent = node.size.max(1) as f32;
        ScrollArea::vertical()
            .id_source("folder_rows")
            .auto_shrink([false, false])
            .show_rows(ui, 34.0, node.children.len(), |ui, range| {
                for idx in range {
                    let child = &node.children[idx];
                    let child_path = cur_path.join(&child.name);
                    let removed = a.removed.contains(&child_path);
                    widgets::row(ui, 34.0, |ui| {
                        let (icon_c, col) = match child.kind {
                            NodeKind::Dir => (ic::FOLDER, PALETTE[idx % PALETTE.len()]),
                            NodeKind::File => (ic::FILE, classify(&ext_of(&child.name)).color()),
                            NodeKind::SmallFiles => (ic::FILE, TEXT_FAINT),
                            NodeKind::Denied => (ic::LOCK, TEXT_FAINT),
                        };
                        ui.label(widgets::icon(icon_c, 16.0, col));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            let actionable = matches!(child.kind, NodeKind::Dir | NodeKind::File) && !removed;
                            if actionable {
                                let block = util::block_reason_fast(&child_path);
                                let btn = ui.add_enabled(block.is_none(), widgets::small(widgets::icon(ic::CLEAN, 13.0, TEXT_DIM)));
                                let btn = match block {
                                    Some(why) => btn.on_disabled_hover_text(why),
                                    None => btn.on_hover_text("Move to Recycle Bin"),
                                };
                                if btn.clicked() {
                                    acts.push(AAct::AskRecycle(vec![(child_path.clone(), child.size)]));
                                }
                                if ui.add(widgets::small(widgets::icon(ic::OPEN, 13.0, TEXT_DIM))).on_hover_text("Show in Explorer").clicked() {
                                    acts.push(AAct::Show(child_path.clone()));
                                }
                            }
                            ui.add_sized([96.0, 20.0], Label::new(widgets::faint(fmt_age(child.modified))));
                            let files = if child.kind == NodeKind::Denied {
                                "no access".to_string()
                            } else {
                                format!("{} files", fmt_num(child.files))
                            };
                            ui.add_sized([96.0, 20.0], Label::new(widgets::faint(files)));
                            ui.add_sized([50.0, 20.0], Label::new(widgets::faint(format!("{:.0}%", child.size as f32 / parent * 100.0))));
                            ui.add_sized([84.0, 20.0], Label::new(widgets::strong(fmt_bytes(child.size), 14.0)));
                            widgets::bar(ui, child.size as f32 / parent, col, vec2(110.0, 6.0));
                            ui.add_space(8.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let enterable = child.kind == NodeKind::Dir && !child.children.is_empty();
                                let mut text = RichText::new(&child.name).color(if removed { TEXT_FAINT } else { TEXT });
                                if removed {
                                    text = text.strikethrough();
                                }
                                let resp = ui.add(Label::new(text).truncate().sense(Sense::click()));
                                let resp = if enterable { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
                                if enterable && resp.clicked() {
                                    acts.push(AAct::Enter(idx));
                                }
                                if actionable {
                                    let p = child_path.clone();
                                    resp.context_menu(|ui| {
                                        if ui.button("Show in Explorer").clicked() {
                                            acts.push(AAct::Show(p.clone()));
                                            ui.close_menu();
                                        }
                                        if ui.button("Copy path").clicked() {
                                            acts.push(AAct::Copy(p.display().to_string()));
                                            ui.close_menu();
                                        }
                                    });
                                }
                            });
                        });
                    });
                }
            });
    }

    fn render_file_list(&self, ui: &mut Ui, res: &ScanResult, idx: &[usize], acts: &mut Vec<AAct>) {
        let a = &self.analyzer;
        let sel_items: Vec<(PathBuf, u64)> = res
            .big
            .iter()
            .filter(|f| a.selected.contains(&f.path) && !a.removed.contains(&f.path))
            .map(|f| (f.path.clone(), f.size))
            .collect();
        let sel_total: u64 = sel_items.iter().map(|i| i.1).sum();
        ui.horizontal(|ui| {
            if ui.add(widgets::small("Select all shown")).clicked() {
                let v: Vec<PathBuf> = idx
                    .iter()
                    .map(|&i| &res.big[i])
                    .filter(|f| f.block.is_none())
                    .map(|f| f.path.clone())
                    .collect();
                acts.push(AAct::SelectMany(v));
            }
            if !sel_items.is_empty() {
                if ui.add(widgets::small("Clear selection")).clicked() {
                    acts.push(AAct::ClearSel);
                }
                ui.label(widgets::dim(format!("{} selected, {}", sel_items.len(), fmt_bytes(sel_total))));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !sel_items.is_empty() && !a.recycle.running(),
                        widgets::danger(widgets::icon_text(ic::CLEAN, "Move selected to Recycle Bin", 14.0, Color32::WHITE)),
                    )
                    .clicked()
                {
                    acts.push(AAct::AskRecycle(sel_items.clone()));
                }
                if a.recycle.running() {
                    ui.spinner();
                }
            });
        });
        if idx.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(widgets::dim("No files match.")));
            return;
        }
        ScrollArea::vertical()
            .id_source("big_files")
            .auto_shrink([false, false])
            .show_rows(ui, 44.0, idx.len(), |ui, range| {
                for &i in &idx[range] {
                    let f = &res.big[i];
                    widgets::row(ui, 44.0, |ui| {
                        let mut sel = a.selected.contains(&f.path);
                        let cb = ui.add_enabled(f.block.is_none(), egui::Checkbox::new(&mut sel, ""));
                        if let Some(why) = f.block {
                            cb.on_disabled_hover_text(why);
                        } else if cb.changed() {
                            acts.push(AAct::Select(f.path.clone(), sel));
                        }
                        let name = f.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        let cat = classify(&ext_of(&name));
                        ui.label(widgets::icon(if f.block.is_some() { ic::LOCK } else { ic::FILE }, 16.0, cat.color()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            if ui.add(widgets::small(widgets::icon(ic::OPEN, 13.0, TEXT_DIM))).on_hover_text("Show in Explorer").clicked() {
                                acts.push(AAct::Show(f.path.clone()));
                            }
                            ui.add_sized([110.0, 20.0], Label::new(widgets::faint(fmt_age(f.modified))));
                            ui.add_sized([88.0, 20.0], Label::new(widgets::strong(fmt_bytes(f.size), 14.0)));
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.add(Label::new(RichText::new(&name).color(TEXT)).truncate());
                                    let folder = f.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                                    ui.add(Label::new(widgets::faint(folder)).truncate());
                                });
                            });
                        });
                    });
                }
            });
    }

    fn render_types(&self, ui: &mut Ui, res: &ScanResult) {
        let total = res.root.size.max(1) as f32;
        ScrollArea::vertical()
            .id_source("types_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.columns(2, |cols| {
                    widgets::card(&mut cols[0], |ui| {
                        ui.label(widgets::strong("By kind", 16.0));
                        ui.add_space(6.0);
                        // A single stacked bar first: the whole drive at a glance.
                        let w = ui.available_width();
                        let (rect, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());
                        let mut x = rect.left();
                        for (cat, bytes, _) in &res.cats {
                            let seg = rect.width() * (*bytes as f32 / total);
                            if seg < 0.5 {
                                continue;
                            }
                            let r = Rect::from_min_max(pos2(x, rect.top()), pos2((x + seg).min(rect.right()), rect.bottom()));
                            ui.painter().rect_filled(r, Rounding::same(2.0), cat.color());
                            x += seg;
                        }
                        ui.add_space(10.0);
                        for (cat, bytes, count) in &res.cats {
                            ui.horizontal(|ui| {
                                widgets::dot(ui, cat.color());
                                ui.label(cat.label());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(widgets::faint(format!("{} files", fmt_num(*count))));
                                    ui.add_sized([80.0, 20.0], Label::new(widgets::strong(fmt_bytes(*bytes), 14.0)));
                                });
                            });
                        }
                    });
                    widgets::card(&mut cols[1], |ui| {
                        ui.label(widgets::strong("Top file extensions", 16.0));
                        ui.add_space(6.0);
                        let top = res.exts.first().map(|e| e.bytes).unwrap_or(1).max(1) as f32;
                        for e in res.exts.iter().take(25) {
                            ui.horizontal(|ui| {
                                ui.add_sized([70.0, 20.0], Label::new(RichText::new(format!(".{}", e.ext)).color(TEXT)));
                                let bw = (ui.available_width() - 190.0).max(20.0);
                                widgets::bar(ui, e.bytes as f32 / top, e.cat.color(), vec2(bw, 6.0));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(widgets::faint(fmt_num(e.count)));
                                    ui.add_sized([80.0, 20.0], Label::new(widgets::strong(fmt_bytes(e.bytes), 14.0)));
                                });
                            });
                        }
                    });
                });
            });
    }
}
