use crate::app::{Action, App, Modal, Page, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_age, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontId, Label, Layout, Rect, RichText, Rounding,
    ScrollArea, Sense, Ui,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
pub struct FileRec {
    pub path: PathBuf,
    pub lower: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    #[serde(skip)]
    pub block: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg"
        | "m2ts" | "3gp" => FileCat::Video,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff"
        | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" | "svg" | "ico" => FileCat::Image,
        "mp3" | "flac" | "wav" | "aac" | "m4a" | "ogg" | "wma" | "opus" | "aiff" => FileCat::Audio,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "txt"
        | "rtf" | "epub" | "md" | "csv" | "pst" | "ost" => FileCat::Document,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "cab" | "tgz" => {
            FileCat::Archive
        }
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "vdi" | "qcow2" | "wim" | "esd" | "avhdx" => {
            FileCat::DiskImage
        }
        "msi" | "msix" | "msixbundle" | "appx" | "appxbundle" | "msp" | "msu" => FileCat::Installer,
        "exe" | "dll" | "sys" | "so" | "lib" | "ocx" | "node" | "pyd" | "jar" => FileCat::Program,
        "rs" | "c" | "cpp" | "h" | "cs" | "js" | "ts" | "py" | "java" | "json" | "xml" | "db"
        | "sqlite" | "sql" | "log" | "pdb" | "obj" | "rlib" | "o" | "pack" | "idx" => FileCat::Code,
        "pak" | "bsa" | "ba2" | "vpk" | "bundle" | "assets" | "ucas" | "utoc" | "forge" | "big"
        | "dat" => FileCat::Game,
        _ => FileCat::Other,
    }
}

fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 12 => {
            ext.to_lowercase()
        }
        _ => "(none)".to_string(),
    }
}

fn sort_file_indices(res: &ScanResult, indexes: &mut [usize], sort: ASort) {
    indexes.sort_by(|&i, &j| {
        let (left, right) = (&res.big[i], &res.big[j]);
        match sort {
            ASort::Name => left.lower.cmp(&right.lower),
            ASort::Modified => right.modified.cmp(&left.modified),
            ASort::Size | ASort::Files => right.size.cmp(&left.size),
        }
        .then_with(|| left.path.cmp(&right.path))
    });
}

#[derive(Serialize, Deserialize)]
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
    parallel: bool,
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
    // Check the chosen root too, and recheck each directory before entering it.
    // DirEntry metadata alone does not protect a folder replaced with a junction.
    let valid_dir =
        fs::symlink_metadata(path).is_ok_and(|md| md.is_dir() && !util::is_reparse(&md));
    if !valid_dir {
        sh.denied.fetch_add(1, Ordering::Relaxed);
        node.kind = NodeKind::Denied;
        return node;
    }
    if let Ok(mut current) = sh.progress.current.try_lock() {
        *current = path.display().to_string();
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
    let mut reported_files = 0;
    let mut reported_bytes = 0;
    for entry in rd.flatten() {
        if sh.progress.cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(md) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
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
        if !md.is_file() {
            continue;
        }
        let size = if attrs & util::CLOUD_MASK != 0 {
            0
        } else {
            md.len()
        };
        let modified = md.modified().ok();
        node.modified = node.modified.max(modified);
        node.files += 1;
        node.size += size;
        if node.files % 256 == 0 {
            // A single flat directory can contain hundreds of thousands of
            // entries. Publish bounded updates before it finishes, too.
            sh.progress
                .files
                .fetch_add(node.files - reported_files, Ordering::Relaxed);
            sh.progress
                .bytes
                .fetch_add(node.size - reported_bytes, Ordering::Relaxed);
            reported_files = node.files;
            reported_bytes = node.size;
        }
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
    p.files
        .fetch_add(node.files - reported_files, Ordering::Relaxed);
    p.bytes
        .fetch_add(node.size - reported_bytes, Ordering::Relaxed);
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

    let visit = |(p, n): (PathBuf, String)| scan_dir(&p, n, sh, depth + 1);
    let subs: Vec<Node> = if sh.parallel {
        subdirs.into_par_iter().map(visit).collect()
    } else {
        subdirs.into_iter().map(visit).collect()
    };
    for s in subs {
        node.size += s.size;
        node.files += s.files;
        node.modified = node.modified.max(s.modified);
        node.children.push(s);
    }
    node.children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    node
}

/// Scans on a dedicated, bounded pool so a whole-drive scan can't take every core
/// from the window, and so hard drives aren't thrashed by dozens of parallel seeks.
pub fn scan_bounded(root_path: PathBuf, progress: Arc<Progress>, threads: usize) -> ScanResult {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads.clamp(1, 8))
        .stack_size(16 * 1024 * 1024)
        .thread_name(|i| format!("pcsweep-scan-{i}"))
        .build();
    match pool {
        Ok(pool) => pool.install(|| scan(root_path, progress)),
        // Resource pressure must not silently switch to the unbounded global pool.
        Err(_) => scan_internal(root_path, progress, false),
    }
}

/// Worker count for a scan: two for spinning disks, otherwise leave a core free.
pub fn scan_threads(root_is_hdd: bool, cores: usize) -> usize {
    cores
        .saturating_sub(1)
        .max(1)
        .min(if root_is_hdd { 2 } else { 8 })
}

pub fn scan(root_path: PathBuf, progress: Arc<Progress>) -> ScanResult {
    scan_internal(root_path, progress, true)
}

fn scan_internal(root_path: PathBuf, progress: Arc<Progress>, parallel: bool) -> ScanResult {
    let start = Instant::now();
    let sh = Shared {
        progress: &*progress,
        big: Mutex::new(Vec::new()),
        ext: Mutex::new(HashMap::new()),
        denied: AtomicU64::new(0),
        parallel,
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
            ExtStat {
                ext,
                bytes,
                count,
                cat,
            }
        })
        .collect();
    exts.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes));
    let mut cats: Vec<(FileCat, u64, u64)> =
        cat_map.into_iter().map(|(c, (b, n))| (c, b, n)).collect();
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

fn path_removed(path: &Path, removed: &HashSet<PathBuf>) -> bool {
    removed.iter().any(|root| path.starts_with(root))
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
    painter.rect_filled(rect, Rounding::same(12.0), color_surface());
    let mut items: Vec<(usize, f64)> = node
        .children
        .iter()
        .enumerate()
        .filter(|(_, c)| c.size > 0)
        .take(149)
        .map(|(i, c)| (i, c.size as f64))
        .collect();
    // Preserve the area of the long tail instead of making the first 150
    // entries appear to occupy the entire folder.
    let other = Node {
        name: "Other items".into(),
        size: node.children.iter().skip(149).map(|c| c.size).sum(),
        files: node.children.iter().skip(149).map(|c| c.files).sum(),
        kind: NodeKind::SmallFiles,
        modified: None,
        children: Vec::new(),
    };
    if other.size > 0 {
        items.push((node.children.len(), other.size as f64));
    }
    if items.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "This folder is empty",
            FontId::proportional(14.0),
            color_muted(),
        );
        return None;
    }
    let mut clicked = None;
    for (k, (idx, r)) in squarify(&items, rect.shrink(4.0)).into_iter().enumerate() {
        let child = node.children.get(idx).unwrap_or(&other);
        let r = r.shrink(1.5);
        if r.width() < 1.0 || r.height() < 1.0 {
            continue;
        }
        let base = node_color(child, k);
        let resp = ui.interact(r, ui.id().with(("treemap_cell", idx)), Sense::click());
        let hovered = resp.hovered();
        let fill = if hovered {
            mix(base, Color32::WHITE, 0.18)
        } else {
            base
        };
        painter.rect_filled(r, corners(6.0), fill);
        let label = chart_text(fill);
        if r.width() > 64.0 && r.height() > 40.0 {
            let p = painter.with_clip_rect(r.shrink(4.0));
            p.text(
                r.left_top() + vec2(9.0, 8.0),
                Align2::LEFT_TOP,
                &child.name,
                widgets::semibold(13.5),
                label,
            );
            p.text(
                r.left_top() + vec2(9.0, 27.0),
                Align2::LEFT_TOP,
                fmt_bytes(child.size),
                FontId::proportional(12.5),
                label,
            );
        } else if r.width() > 46.0 && r.height() > 20.0 {
            let p = painter.with_clip_rect(r.shrink(3.0));
            p.text(
                r.left_top() + vec2(6.0, 4.0),
                Align2::LEFT_TOP,
                &child.name,
                FontId::proportional(11.5),
                label,
            );
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

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ATab {
    Folders,
    Largest,
    Types,
    Old,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ASort {
    #[default]
    Size,
    Name,
    Modified,
    Files,
}

const RECENT_MAPS: usize = 3;
const DISK_MAPS: usize = 8;
const MAP_BYTE_BUDGET: u64 = 512 * 1024 * 1024;
static MAP_INDEX_WRITER: Mutex<()> = Mutex::new(());
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MapView {
    nav: Vec<usize>,
    tab: ATab,
    filter: String,
    old_months: u32,
    old_min: u64,
    removed: HashSet<PathBuf>,
    #[serde(default)]
    sort: ASort,
}
impl MapView {
    fn capture(a: &AnalyzerState) -> Self {
        Self {
            nav: a.nav.clone(),
            tab: a.tab,
            filter: a.filter.clone(),
            old_months: a.old_months,
            old_min: a.old_min,
            removed: a.removed.clone(),
            sort: a.sort,
        }
    }
}
struct SavedMap {
    result: Arc<ScanResult>,
    stamp: u64,
    view: MapView,
    read_only: bool,
}
impl Clone for SavedMap {
    fn clone(&self) -> Self {
        Self {
            result: self.result.clone(),
            stamp: self.stamp,
            view: self.view.clone(),
            read_only: self.read_only,
        }
    }
}
#[derive(Default)]
pub struct AnalyzerCache {
    load: Task<Result<Option<LoadedMaps>, String>>,
    save: Task<Result<Vec<MapIndex>, String>>,
    requested: Option<PathBuf>,
    load_started: Option<Instant>,
    disk_index: Vec<MapIndex>,
    recent: Vec<SavedMap>,
    stamp: Option<u64>,
    read_only: bool,
    dirty: Option<Instant>,
    error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct MapIndex {
    root: PathBuf,
    stamp: u64,
    view: MapView,
}
#[derive(Serialize, Deserialize)]
struct DiskNode {
    parent: Option<usize>,
    name: String,
    size: u64,
    files: u64,
    kind: NodeKind,
    modified: Option<SystemTime>,
}
#[derive(Deserialize)]
struct DiskMap {
    root_path: PathBuf,
    nodes: Vec<DiskNode>,
    big: Vec<FileRec>,
    exts: Vec<ExtStat>,
    cats: Vec<(FileCat, u64, u64)>,
    elapsed: Duration,
    denied: u64,
    dirs: u64,
    cancelled: bool,
}
#[derive(Serialize)]
struct DiskMapRef<'a> {
    root_path: &'a Path,
    nodes: Vec<DiskNode>,
    big: &'a [FileRec],
    exts: &'a [ExtStat],
    cats: &'a [(FileCat, u64, u64)],
    elapsed: Duration,
    denied: u64,
    dirs: u64,
    cancelled: bool,
}
fn map_file(root: &Path, stamp: u64) -> String {
    format!(
        "map-{:032x}-{stamp}.json",
        xxhash_rust::xxh3::xxh3_128(util::norm(root).as_bytes())
    )
}
fn map_ref(result: &ScanResult) -> DiskMapRef<'_> {
    let mut nodes = Vec::new();
    let mut stack = vec![(None, &result.root)];
    while let Some((parent, node)) = stack.pop() {
        let id = nodes.len();
        nodes.push(DiskNode {
            parent,
            name: node.name.clone(),
            size: node.size,
            files: node.files,
            kind: node.kind,
            modified: node.modified,
        });
        stack.extend(node.children.iter().rev().map(|n| (Some(id), n)));
    }
    DiskMapRef {
        root_path: &result.root_path,
        nodes,
        big: &result.big,
        exts: &result.exts,
        cats: &result.cats,
        elapsed: result.elapsed,
        denied: result.denied,
        dirs: result.dirs,
        cancelled: result.cancelled,
    }
}
fn restore_map(disk: DiskMap) -> Result<ScanResult, String> {
    if !disk.root_path.is_absolute()
        || disk.nodes.is_empty()
        || disk.nodes.len() > 1_000_000
        || disk.big.len() > 500_000
        || disk.exts.len() > 100_000
        || disk.cats.len() > 16
    {
        return Err("Saved map is invalid or too large.".into());
    }
    let mut parents = Vec::with_capacity(disk.nodes.len());
    let mut depths = Vec::with_capacity(disk.nodes.len());
    let mut nodes = Vec::with_capacity(disk.nodes.len());
    for (i, n) in disk.nodes.into_iter().enumerate() {
        if n.name.len() > 32768
            || (i == 0 && n.parent.is_some())
            || (i > 0 && !n.parent.is_some_and(|p| p < i))
            || (i > 0
                && (n.name.is_empty()
                    || n.name == "."
                    || n.name == ".."
                    || n.name.contains(['\\', '/', ':'])))
        {
            return Err("Saved map contains an invalid folder identity.".into());
        }
        let depth = n.parent.map_or(0usize, |p| depths[p] + 1);
        if depth > 202 {
            return Err("Saved map nesting exceeds the scan limit.".into());
        }
        depths.push(depth);
        parents.push(n.parent);
        nodes.push(Some(Node {
            name: n.name,
            size: n.size,
            files: n.files,
            kind: n.kind,
            modified: n.modified,
            children: Vec::new(),
        }));
    }
    for i in (1..nodes.len()).rev() {
        let mut node = nodes[i].take().ok_or("Incomplete saved map")?;
        node.children.reverse();
        let bytes = node
            .children
            .iter()
            .try_fold(0u64, |sum, n| sum.checked_add(n.size));
        let files = node
            .children
            .iter()
            .try_fold(0u64, |sum, n| sum.checked_add(n.files));
        if bytes.is_none_or(|n| n > node.size) || files.is_none_or(|n| n > node.files) {
            return Err("Saved folder totals are inconsistent.".into());
        }
        nodes[parents[i].ok_or("Invalid saved parent")?]
            .as_mut()
            .ok_or("Invalid saved parent order")?
            .children
            .push(node);
    }
    let mut root = nodes[0].take().ok_or("Saved map has no root")?;
    root.children.reverse();
    if root
        .children
        .iter()
        .try_fold(0u64, |sum, n| sum.checked_add(n.size))
        .is_none_or(|n| n > root.size)
        || root
            .children
            .iter()
            .try_fold(0u64, |sum, n| sum.checked_add(n.files))
            .is_none_or(|n| n > root.files)
    {
        return Err("Saved map totals are inconsistent.".into());
    }
    let root_norm = util::norm(&disk.root_path);
    let mut big = disk.big;
    if big.iter().any(|f| {
        !f.path.is_absolute()
            || !util::is_within(&util::norm(&f.path), &root_norm)
            || f.path.as_os_str().len() > 32768
    }) {
        return Err("Saved file identity is outside its map.".into());
    }
    let mut paths = HashSet::new();
    if big.iter().any(|f| !paths.insert(util::norm(&f.path)))
        || big
            .iter()
            .try_fold(0u64, |sum, f| sum.checked_add(f.size))
            .is_none_or(|n| n > root.size)
        || disk.exts.iter().any(|e| e.ext.len() > 32)
        || disk
            .exts
            .iter()
            .try_fold(0u64, |sum, e| sum.checked_add(e.bytes))
            .is_none_or(|n| n > root.size)
        || disk
            .cats
            .iter()
            .try_fold(0u64, |sum, (_, bytes, _)| sum.checked_add(*bytes))
            .is_none_or(|n| n > root.size)
    {
        return Err("Saved map file totals are inconsistent.".into());
    }
    for f in &mut big {
        f.lower = f.path.to_string_lossy().to_lowercase();
        f.block = util::block_reason_fast(&f.path);
    }
    Ok(ScanResult {
        root_path: disk.root_path,
        root,
        big,
        exts: disk.exts,
        cats: disk.cats,
        elapsed: disk.elapsed,
        denied: disk.denied,
        dirs: disk.dirs,
        cancelled: disk.cancelled,
    })
}
struct LoadedMaps {
    maps: Vec<SavedMap>,
    index: Vec<MapIndex>,
}
fn load_indexed_map(item: &MapIndex) -> Result<SavedMap, String> {
    if !item.root.is_absolute()
        || item.root.as_os_str().len() > 32768
        || item.view.nav.len() > 256
        || item.view.filter.len() > 8192
        || item.view.removed.len() > 500_000
    {
        return Err("Saved map index is invalid.".into());
    }
    let stored = crate::scan_cache::load::<DiskMap>(&map_file(&item.root, item.stamp))?
        .ok_or("This saved map is no longer available.")?;
    if stored.saved != item.stamp {
        return Err("Saved map identity changed.".into());
    }
    let result = restore_map(stored.data)?;
    if util::norm(&result.root_path) != util::norm(&item.root) {
        return Err("Saved map root changed.".into());
    }
    Ok(SavedMap {
        result: Arc::new(result),
        stamp: item.stamp,
        view: item.view.clone(),
        read_only: true,
    })
}
fn load_maps() -> Result<Option<LoadedMaps>, String> {
    let Some(index) = crate::scan_cache::load::<Vec<MapIndex>>("maps-index.json")? else {
        return Ok(None);
    };
    if index.data.len() > DISK_MAPS {
        return Err("Saved map index is invalid.".into());
    }
    // Restore one map immediately. Other roots remain as lightweight index
    // entries and are loaded only when selected, without remapping their files.
    let first = index
        .data
        .iter()
        .find_map(|item| load_indexed_map(item).ok());
    Ok(Some(LoadedMaps {
        maps: first.into_iter().collect(),
        index: index.data,
    }))
}
fn save_maps(maps: Vec<SavedMap>, mut index: Vec<MapIndex>) -> Result<Vec<MapIndex>, String> {
    let _writer = MAP_INDEX_WRITER
        .lock()
        .map_err(|_| "Saved map publisher is unavailable")?;
    // A delayed metadata save must never replace a newer completed scan.
    if let Ok(Some(latest)) = crate::scan_cache::load::<Vec<MapIndex>>("maps-index.json") {
        if latest.data.len() <= DISK_MAPS {
            for item in latest.data.into_iter().rev() {
                if !index.iter().any(|old| {
                    util::norm(&old.root) == util::norm(&item.root) && old.stamp > item.stamp
                }) {
                    index.retain(|old| util::norm(&old.root) != util::norm(&item.root));
                    index.insert(0, item);
                }
            }
        }
    }
    for item in maps.iter().rev() {
        if index.iter().any(|old| {
            util::norm(&old.root) == util::norm(&item.result.root_path) && old.stamp > item.stamp
        }) {
            continue;
        }
        let name = map_file(&item.result.root_path, item.stamp);
        if !crate::scan_cache::exists(&name) {
            crate::scan_cache::save(&name, item.stamp, &map_ref(&item.result))?;
        }
        index.retain(|m| util::norm(&m.root) != util::norm(&item.result.root_path));
        index.insert(
            0,
            MapIndex {
                root: item.result.root_path.clone(),
                stamp: item.stamp,
                view: item.view.clone(),
            },
        );
    }
    let mut bytes = 0u64;
    let mut kept = Vec::new();
    let mut files = HashSet::new();
    for item in index {
        if kept.len() == DISK_MAPS {
            break;
        }
        let file = map_file(&item.root, item.stamp);
        let Some(size) = crate::scan_cache::file_size(&file)? else {
            continue;
        };
        if bytes.saturating_add(size) > MAP_BYTE_BUDGET {
            continue;
        }
        bytes += size;
        files.insert(file);
        kept.push(item);
    }
    crate::scan_cache::save("maps-index.json", crate::scan_cache::now(), &kept)?;
    crate::scan_cache::prune_maps(&files)?;
    Ok(kept)
}

pub struct AnalyzerState {
    pub scan: Task<ScanResult>,
    pub progress: Option<Arc<Progress>>,
    pub scan_root: Option<PathBuf>,
    pub started: Option<Instant>,
    pub result: Option<Arc<ScanResult>>,
    pub cache: AnalyzerCache,
    pub nav: Vec<usize>,
    pub tab: ATab,
    pub sort: ASort,
    pub filter: String,
    pub selected: HashSet<PathBuf>,
    pub removed: HashSet<PathBuf>,
    pub old_months: u32,
    pub old_min: u64,
    pub recycle: Task<util::RecycleReport>,
    pub journal_error: Option<String>,
    pub selection_check: Task<Result<util::SelectionReview, String>>,
}

impl Default for AnalyzerState {
    fn default() -> Self {
        Self {
            scan: Task::default(),
            progress: None,
            scan_root: None,
            started: None,
            result: None,
            cache: AnalyzerCache::default(),
            nav: Vec::new(),
            tab: ATab::Folders,
            sort: ASort::Size,
            filter: String::new(),
            selected: HashSet::new(),
            removed: HashSet::new(),
            old_months: 12,
            old_min: 100 * 1024 * 1024,
            recycle: Task::default(),
            journal_error: None,
            selection_check: Task::default(),
        }
    }
}

impl AnalyzerState {
    pub(crate) fn cache_running(&self) -> bool {
        self.cache.load.running() || self.cache.save.running()
    }

    fn activate_saved(&mut self, root: &Path) -> bool {
        let Some(index) = self
            .cache
            .recent
            .iter()
            .position(|m| util::norm(&m.result.root_path) == util::norm(root))
        else {
            return false;
        };
        let saved = self.cache.recent.remove(index);
        self.result = Some(saved.result.clone());
        self.scan_root = Some(saved.result.root_path.clone());
        self.tab = saved.view.tab;
        self.sort = saved.view.sort;
        self.filter = saved.view.filter.clone();
        self.old_months = saved.view.old_months;
        self.old_min = saved.view.old_min;
        self.removed = saved.view.removed.clone();
        self.nav.clear();
        let mut node = &saved.result.root;
        for &i in &saved.view.nav {
            let Some(child) = node.children.get(i) else {
                break;
            };
            if child.kind != NodeKind::Dir {
                break;
            }
            self.nav.push(i);
            node = child;
        }
        self.selected.clear();
        self.cache.stamp = Some(saved.stamp);
        self.cache.read_only = saved.read_only;
        self.cache.recent.insert(0, saved);
        self.cache.dirty = Some(Instant::now());
        true
    }
}

#[cfg(test)]
pub fn mark_preview_saved(state: &mut AnalyzerState) {
    state.cache.stamp = Some(crate::scan_cache::now());
    state.cache.read_only = true;
}

enum AAct {
    Go(Page),
    DismissJournalError,
    SkipRestore,
    Scan(PathBuf),
    Rescan(PathBuf),
    PickFolder,
    Cancel,
    Tab(ATab),
    Sort(ASort),
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
    fn remember_analyzer(&mut self) {
        let a = &mut self.analyzer;
        let (Some(result), Some(stamp)) = (a.result.as_ref(), a.cache.stamp) else {
            return;
        };
        let view = MapView::capture(a);
        if let Some(index) = a
            .cache
            .recent
            .iter()
            .position(|m| Arc::ptr_eq(&m.result, result))
        {
            if a.cache.recent[index].view == view {
                return;
            }
            a.cache.recent[index].view = view;
        } else {
            let (retained, old): (Vec<_>, Vec<_>) = std::mem::take(&mut a.cache.recent)
                .into_iter()
                .partition(|m| util::norm(&m.result.root_path) != util::norm(&result.root_path));
            a.cache.recent = retained;
            if !old.is_empty() {
                let _ = std::thread::Builder::new()
                    .name("pcsweep-release-old-map".into())
                    .spawn(move || drop(old));
            }
            a.cache.recent.insert(
                0,
                SavedMap {
                    result: result.clone(),
                    stamp,
                    view,
                    read_only: a.cache.read_only,
                },
            );
            if a.cache.recent.len() > RECENT_MAPS {
                let evicted = a.cache.recent.split_off(RECENT_MAPS);
                let _ = std::thread::Builder::new()
                    .name("pcsweep-release-map-cache".into())
                    .spawn(move || drop(evicted));
            }
        }
        a.cache.dirty = Some(Instant::now());
    }
    pub(crate) fn restore_analyzer_cache(&mut self, ctx: &egui::Context) {
        self.analyzer.cache.load_started = Some(Instant::now());
        self.analyzer.cache.load = Task::spawn(ctx, load_maps);
    }
    pub(crate) fn poll_analyzer_cache(&mut self, ctx: &egui::Context) {
        if self.analyzer.cache.load.running()
            && self
                .analyzer
                .cache
                .load_started
                .is_some_and(|t| t.elapsed() > Duration::from_secs(30))
        {
            self.analyzer.cache.load = Task::default();
            self.analyzer.cache.error = Some("Restoring the saved map took too long. You can start a fresh scan; the saved files have not been removed.".into());
            self.analyzer.cache.requested = None;
        }
        if let Some(result) = self.analyzer.cache.load.poll() {
            self.analyzer.cache.load_started = None;
            let requested = self.analyzer.cache.requested.take();
            match result.and_then(|r| r) {
                Ok(Some(loaded)) => {
                    if !loaded.index.is_empty() {
                        self.analyzer.cache.disk_index = loaded.index;
                    }
                    for map in loaded.maps {
                        self.analyzer.cache.recent.retain(|m| {
                            util::norm(&m.result.root_path) != util::norm(&map.result.root_path)
                        });
                        self.analyzer.cache.recent.insert(0, map);
                    }
                    if !self.analyzer.scan.running()
                        && (requested.is_some() || self.analyzer.result.is_none())
                    {
                        let target = requested.clone().or_else(|| {
                            self.analyzer
                                .cache
                                .recent
                                .first()
                                .map(|m| m.result.root_path.clone())
                        });
                        if let Some(path) = target {
                            if self.analyzer.activate_saved(&path) {
                                self.analyzer.cache.dirty = None;
                            } else if requested.is_some() {
                                self.force_analyze(path, ctx);
                            }
                        }
                    }
                    if self.analyzer.cache.recent.len() > RECENT_MAPS {
                        let old = self.analyzer.cache.recent.split_off(RECENT_MAPS);
                        let _ = std::thread::Builder::new()
                            .name("pcsweep-release-map-cache".into())
                            .spawn(move || drop(old));
                    }
                }
                Ok(None) => {
                    if let Some(path) = requested {
                        self.force_analyze(path, ctx);
                    }
                }
                Err(e) => {
                    self.analyzer.cache.error = Some(e);
                    if let Some(path) = requested {
                        self.force_analyze(path, ctx);
                    }
                }
            }
        }
        if let Some(result) = self.analyzer.cache.save.poll() {
            match result.and_then(|r| r) {
                Ok(index) => {
                    self.analyzer.cache.disk_index = index;
                    self.analyzer.cache.error = None;
                }
                Err(e) => self.analyzer.cache.error = Some(e),
            }
        }
        if !self.analyzer.cache.save.running()
            && self
                .analyzer
                .cache
                .dirty
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(350))
        {
            let maps = self.analyzer.cache.recent.clone();
            let index = self.analyzer.cache.disk_index.clone();
            self.analyzer.cache.dirty = None;
            self.analyzer.cache.save = Task::spawn(ctx, move || save_maps(maps, index));
        }
    }
    pub fn start_analyze(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.page = Page::Analyzer;
        if self.analyzer.cache.load.running() {
            self.toast(
                ToastKind::Info,
                "A saved map is loading. It will be ready shortly.",
            );
            return;
        }
        if !self.analyzer.scan.running() && !self.analyzer.recycle.running() {
            self.remember_analyzer();
            if self.analyzer.activate_saved(&path) {
                return;
            }
            if let Some(item) = self
                .analyzer
                .cache
                .disk_index
                .iter()
                .find(|m| util::norm(&m.root) == util::norm(&path))
                .cloned()
            {
                self.analyzer.scan_root = Some(path.clone());
                self.analyzer.cache.requested = Some(path);
                self.analyzer.cache.load_started = Some(Instant::now());
                self.analyzer.cache.load = Task::spawn(ctx, move || {
                    load_indexed_map(&item).map(|map| {
                        Some(LoadedMaps {
                            maps: vec![map],
                            index: Vec::new(),
                        })
                    })
                });
                return;
            }
        }
        self.force_analyze(path, ctx);
    }

    fn force_analyze(&mut self, path: PathBuf, ctx: &egui::Context) {
        self.page = Page::Analyzer;
        if self.analyzer.scan.running() || self.analyzer.recycle.running() {
            self.toast(
                ToastKind::Info,
                "Wait for the current scan or Recycle Bin operation to finish.",
            );
            return;
        }
        self.remember_analyzer();
        self.analyzer.cache.read_only = false;
        self.analyzer.cache.stamp = None;
        let progress = Arc::new(Progress::default());
        let p2 = progress.clone();
        let root = path.clone();
        let root_norm = util::norm(&path);
        let hdd = self.sys.drives.iter().any(|d| {
            d.kind == "HDD" && util::is_within(&root_norm, &util::norm(Path::new(&d.mount)))
        });
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let threads = scan_threads(hdd, cores);
        // Publish traversal results before serialization. Persistence has its own
        // task, so a slow disk or antivirus cannot hold a completed map hostage.
        self.analyzer.scan = Task::spawn(ctx, move || scan_bounded(root, p2, threads));
        self.analyzer.progress = Some(progress);
        self.analyzer.scan_root = Some(path.clone());
        self.analyzer.started = Some(Instant::now());
        // Freeing a multi-million-file tree takes a noticeable moment, so the
        // previous result is released on a background thread instead of this frame.
        if let Some(old) = self.analyzer.result.take() {
            let _ = std::thread::Builder::new()
                .name("pcsweep-release-scan".into())
                .spawn(move || drop(old));
        }
        self.analyzer.nav.clear();
        self.analyzer.selected.clear();
        self.analyzer.removed.clear();
        self.analyzer.filter.clear();
    }

    pub fn poll_analyzer(&mut self) {
        if self.modal.is_none() && !self.mutations_running() {
            if let Some(result) = self.analyzer.selection_check.poll() {
                match result.and_then(|r| r) {
                    Ok(review) => self.finish_analyzer_review(review),
                    Err(e) => self.toast(ToastKind::Error, e),
                }
            }
        }
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
                    self.analyzer.cache.stamp = (!res.cancelled).then(crate::scan_cache::now);
                    self.analyzer.cache.error = None;
                    self.analyzer.cache.read_only = false;
                    self.analyzer.result = Some(Arc::new(res));
                    self.remember_analyzer();
                }
                Err(e) => self.toast(ToastKind::Error, format!("Scan failed: {e}")),
            }
            self.analyzer.progress = None;
        }
        if let Some(r) = self.analyzer.recycle.poll() {
            match r {
                Ok(rep) => {
                    self.analyzer.journal_error = rep.journal_error.clone();
                    if let Some(error) = &rep.journal_error {
                        util::log(format!("Analyzer cleanup history: {error}"));
                        self.toast(ToastKind::Error, error.clone());
                    }
                    let n = rep.done.len();
                    for p in rep.done {
                        self.analyzer.selected.remove(&p);
                        self.analyzer.removed.insert(p);
                    }
                    self.analyzer
                        .selected
                        .retain(|p| !path_removed(p, &self.analyzer.removed));
                    if n > 0 {
                        self.toast(
                            if rep.journal_error.is_some() {
                                ToastKind::Info
                            } else {
                                ToastKind::Success
                            },
                            format!(
                                "Moved {} item(s), {}, to the Recycle Bin",
                                n,
                                fmt_bytes(rep.bytes)
                            ),
                        );
                    }
                    if let Some((p, why)) = rep.failed.first() {
                        self.toast(
                            ToastKind::Error,
                            format!(
                                "{} item(s) not moved. {}: {}",
                                rep.failed.len(),
                                p.display(),
                                why
                            ),
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
                AAct::Go(page) => self.page = page,
                AAct::SkipRestore => {
                    self.analyzer.cache.load = Task::default();
                    self.analyzer.cache.load_started = None;
                    self.analyzer.cache.requested = None;
                }
                AAct::DismissJournalError => self.analyzer.journal_error = None,
                AAct::Scan(p) => self.start_analyze(p, &ctx),
                AAct::Rescan(p) => self.force_analyze(p, &ctx),
                AAct::PickFolder => {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title("Choose a folder to analyze")
                        .pick_folder()
                    {
                        self.start_analyze(p, &ctx);
                    }
                }
                AAct::Cancel => {
                    if let Some(p) = &self.analyzer.progress {
                        p.cancel.store(true, Ordering::Relaxed);
                    }
                }
                AAct::Tab(t) => self.analyzer.tab = t,
                AAct::Sort(sort) => self.analyzer.sort = sort,
                AAct::Up => {
                    self.analyzer.nav.pop();
                }
                AAct::Truncate(n) => self.analyzer.nav.truncate(n),
                AAct::Enter(i) => self.analyzer.nav.push(i),
                AAct::Show(p) => util::show_in_explorer(&p),
                AAct::Open(p) => {
                    if p.is_dir() {
                        util::open_path(&p);
                    } else {
                        self.toast(ToastKind::Info, "This folder is unavailable or changed. Refresh the map to see current files.");
                    }
                }
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
                AAct::AskRecycle(items) => self.review_analyzer_selection(items, &ctx),
                AAct::OldMonths(m) => self.analyzer.old_months = m,
                AAct::OldMin(b) => self.analyzer.old_min = b,
                AAct::Filter(s) => self.analyzer.filter = s,
            }
        }
        self.remember_analyzer();
    }

    fn review_analyzer_selection(&mut self, items: Vec<(PathBuf, u64)>, ctx: &egui::Context) {
        if !self.analyzer.selection_check.running()
            && !self.analyzer.recycle.running()
            && !items.is_empty()
        {
            self.analyzer.selection_check = Task::spawn(ctx, move || util::check_selected(items));
        }
    }

    fn finish_analyzer_review(&mut self, review: util::SelectionReview) {
        if review.items.is_empty() {
            let message = review.skipped.first().map_or_else(
                || "No eligible files were selected.".into(),
                |(path, reason)| format!("{} could not be moved. {reason}", path.display()),
            );
            self.toast(ToastKind::Info, message);
            return;
        }
        let total = review
            .items
            .iter()
            .fold(0u64, |size, item| size.saturating_add(item.size));
        const SHOWN_ITEMS: usize = 64;
        const SHOWN_SKIPPED: usize = 16;
        let mut details: Vec<_> = review
            .items
            .iter()
            .take(SHOWN_ITEMS)
            .map(|p| format!("{} ({})", p.path.display(), fmt_bytes(p.size)))
            .collect();
        if review.items.len() > SHOWN_ITEMS {
            details.push(format!(
                "…and {} more checked items",
                review.items.len() - SHOWN_ITEMS
            ));
        }
        details.extend(
            review
                .skipped
                .iter()
                .take(SHOWN_SKIPPED)
                .map(|(p, reason)| format!("Kept {}. {reason}", p.display())),
        );
        if review.skipped.len() > SHOWN_SKIPPED {
            details.push(format!(
                "…and {} more items to keep",
                review.skipped.len() - SHOWN_SKIPPED
            ));
        }
        let mut body = format!("Move {} checked items to the Recycle Bin. PCSweep will verify each item again before moving it.", review.items.len());
        if !review.skipped.is_empty() {
            body.push_str(&format!(
                " {} other items will be kept.",
                review.skipped.len()
            ));
        }
        let note = if review.items.iter().any(|p| p.folder) {
            Some(
                "Whole folders include their contents. You can restore them from the Recycle Bin."
                    .into(),
            )
        } else if review.items.iter().any(|p| util::in_appdata(&p.path)) {
            Some("AppData can hold settings and saves. You can restore moved files from the Recycle Bin.".into())
        } else {
            None
        };
        self.modal = Some(Modal {
            title: format!("Move {} to the Recycle Bin?", fmt_bytes(total)),
            body,
            details,
            note,
            confirm: "Move to Recycle Bin".into(),
            danger: true,
            action: Action::RecycleChecked(review.items),
        });
    }

    fn render_analyzer(&self, ui: &mut Ui, acts: &mut Vec<AAct>) {
        let a = &self.analyzer;
        widgets::page_header(
            ui,
            "Space analyzer",
            "Map your disk space, find large files, and review files that have not changed in a long time.",
        );
        if ui
            .add(widgets::small("Disk health and maintenance"))
            .clicked()
        {
            acts.push(AAct::Go(Page::Storage));
        }
        if let Some(error) = &a.journal_error {
            ui.colored_label(color_warning(), format!("Cleanup history needs review: {error}. Cleanup stopped; check uncertain outcomes in Cleanup history."));
            ui.horizontal_wrapped(|ui| {
                if ui.add(widgets::small("Activity log")).clicked() {
                    acts.push(AAct::Go(Page::Activity));
                }
                if ui.add(widgets::small("Dismiss message")).clicked() {
                    acts.push(AAct::DismissJournalError);
                }
            });
        }
        let scanning = a.scan.running() || a.cache.load.running();

        // Source picker
        ui.horizontal_wrapped(|ui| {
            for d in &self.sys.drives {
                let label = format!(
                    "{}  {} free of {}",
                    d.letter(),
                    fmt_bytes(d.free),
                    fmt_bytes(d.total)
                );
                let selected = a
                    .scan_root
                    .as_ref()
                    .map(|r| util::norm(r) == util::norm(Path::new(&d.mount)))
                    .unwrap_or(false);
                if ui
                    .add_enabled(
                        !scanning,
                        widgets::chip(
                            widgets::icon_text(ic::DRIVE, &label, 14.0, color_ink()),
                            selected,
                        ),
                    )
                    .clicked()
                {
                    acts.push(AAct::Scan(PathBuf::from(&d.mount)));
                }
            }
            if ui
                .add_enabled(
                    !scanning,
                    widgets::chip(
                        widgets::icon_text(ic::FOLDER, "Your user folder", 14.0, color_ink()),
                        false,
                    ),
                )
                .clicked()
            {
                acts.push(AAct::Scan(util::folders().profile.clone()));
            }
            if ui
                .add_enabled(
                    !scanning,
                    widgets::chip(
                        widgets::icon_text(ic::SEARCH, "Choose a folder", 14.0, color_ink()),
                        false,
                    ),
                )
                .clicked()
            {
                acts.push(AAct::PickFolder);
            }
        });
        ui.add_space(8.0);

        if a.cache.load.running() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(widgets::dim("Restoring saved disk maps…"));
                if ui.add(widgets::small("Skip restore")).clicked() {
                    acts.push(AAct::SkipRestore);
                }
            });
            return;
        }
        if a.scan.running() {
            if let Some(p) = &a.progress {
                widgets::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let root = a
                            .scan_root
                            .as_ref()
                            .map(|r| r.display().to_string())
                            .unwrap_or_default();
                        ui.label(widgets::strong(format!("Mapping {root}"), 17.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if p.cancel.load(Ordering::Relaxed) {
                                ui.add_enabled(false, widgets::ghost("Stopping…"));
                            } else if ui.add(widgets::ghost("Stop")).clicked() {
                                acts.push(AAct::Cancel);
                            }
                        });
                    });
                    let secs = a.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                    ui.label(
                        RichText::new(fmt_bytes(p.bytes.load(Ordering::Relaxed)))
                            .font(widgets::light(40.0))
                            .color(color_ink()),
                    );
                    ui.label(widgets::dim(format!(
                        "{} files in {} folders so far, {}s",
                        fmt_num(p.files.load(Ordering::Relaxed)),
                        fmt_num(p.dirs.load(Ordering::Relaxed)),
                        secs
                    )));
                    if p.cancel.load(Ordering::Relaxed) {
                        ui.label(widgets::faint(
                            "Stopping: finishing the folders already being read. Results so far will be kept and marked partial.",
                        ));
                    } else {
                        // Never wait on a scan worker: skip this frame's path if it's busy.
                        let cur = p.current.try_lock().map(|s| s.clone()).unwrap_or_default();
                        ui.add(Label::new(widgets::faint(cur)).truncate());
                    }
                });
            }
            return;
        }

        let Some(res) = &a.result else {
            widgets::card(ui, |ui| {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(widgets::icon(ic::PIE, 40.0, color_accent()));
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

        if let Some(stamp) = a.cache.stamp {
            ui.label(widgets::dim(format!(
                "Scan from {}. {}",
                crate::scan_cache::saved_label(stamp),
                if a.cache.read_only {
                    "Saved map · Selected files are checked before moving."
                } else {
                    "Refresh when files change; switching drives keeps their maps."
                }
            )));
        }
        if let Some(error) = &a.cache.error {
            ui.label(widgets::faint(format!("Saved map unavailable: {error}")));
        }
        // Summary line
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!a.recycle.running(), widgets::small("Rescan")).on_hover_text("Refresh this map after files have changed").clicked() {
                acts.push(AAct::Rescan(res.root_path.clone()));
            }
            ui.label(RichText::new(fmt_bytes(res.root.size)).font(widgets::light(30.0)).color(color_ink()));
            ui.label(widgets::dim(format!(
                "in {} files and {} folders, mapped in {:.1}s.",
                fmt_num(res.root.files),
                fmt_num(res.dirs),
                res.elapsed.as_secs_f32()
            )));
            if res.cancelled {
                widgets::badge(ui, "Stopped early, partial results", color_warning());
            }
            if res.denied > 0 {
                widgets::badge(ui, &format!("{} folders not readable", fmt_num(res.denied)), color_secondary()).on_hover_text(
                    "Windows blocks some system folders. Restarting as administrator lets PCSweep read more of them.",
                );
            }
            if let Some(d) = self.sys.drives.iter().find(|d| util::norm(Path::new(&d.mount)) == util::norm(&res.root_path)) {
                let hidden = d.used().saturating_sub(res.root.size);
                if hidden > 1024 * 1024 * 1024 {
                    widgets::badge(ui, &format!("{} used by Windows and hidden data", fmt_bytes(hidden)), color_secondary()).on_hover_text(
                        "Space the scan can't see: restore points, the system reserve and protected folders. Tools can trim some of it.",
                    );
                }
            }
        });
        if !a.removed.is_empty() {
            ui.label(widgets::faint(
                "Map totals reflect the scan. Rescan to refresh sizes after moving files.",
            ));
        }
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            for (t, label) in [
                (ATab::Folders, "Folders"),
                (ATab::Largest, "Largest files"),
                (ATab::Old, "Old files"),
                (ATab::Types, "File types"),
            ] {
                if ui.add(widgets::chip(label, a.tab == t)).clicked() {
                    acts.push(AAct::Tab(t));
                }
            }
        });
        ui.add_space(6.0);

        if matches!(a.tab, ATab::Largest | ATab::Old) {
            ui.horizontal_wrapped(|ui| {
                let mut filter = a.filter.clone();
                if widgets::search_box(ui, &mut filter, "Search files in this scan", 240.0)
                    .changed()
                {
                    acts.push(AAct::Filter(filter));
                }
                for (sort, label) in [
                    (ASort::Size, "Size"),
                    (ASort::Name, "Name"),
                    (ASort::Modified, "Modified"),
                ] {
                    if ui.add(widgets::chip(label, a.sort == sort)).clicked() {
                        acts.push(AAct::Sort(sort));
                    }
                }
            });
        }
        match a.tab {
            ATab::Folders => self.render_folders(ui, res, acts),
            ATab::Largest => {
                let filter = a.filter.to_lowercase();
                let mut idx: Vec<usize> = (0..res.big.len())
                    .filter(|&i| !path_removed(&res.big[i].path, &a.removed))
                    .filter(|&i| filter.is_empty() || res.big[i].lower.contains(&filter))
                    .collect();
                sort_file_indices(res, &mut idx, a.sort);
                idx.truncate(1000);
                ui.label(widgets::faint(format!(
                    "Files of 10 MB and up · up to 1,000 matches · {} shown",
                    fmt_num(idx.len() as u64)
                )));
                self.render_file_list(ui, res, &idx, acts);
            }
            ATab::Old => {
                let min_days = a.old_months as f64 * 30.4;
                let needle = a.filter.trim().to_lowercase();
                let mut idx: Vec<usize> = (0..res.big.len())
                    .filter(|&i| {
                        let f = &res.big[i];
                        f.size >= a.old_min
                            && (needle.is_empty() || f.lower.contains(&needle))
                            && !path_removed(&f.path, &a.removed)
                            && f.modified
                                .map(|m| util::age_days(m) >= min_days)
                                .unwrap_or(false)
                    })
                    .collect();
                sort_file_indices(res, &mut idx, a.sort);
                idx.truncate(1000);
                ui.horizontal_wrapped(|ui| {
                    ui.label(widgets::dim("Not changed in"));
                    for (m, label) in [
                        (6, "6 months"),
                        (12, "1 year"),
                        (24, "2 years"),
                        (36, "3 years"),
                    ] {
                        if ui.add(widgets::chip(label, a.old_months == m)).clicked() {
                            acts.push(AAct::OldMonths(m));
                        }
                    }
                    ui.add_space(12.0);
                    ui.label(widgets::dim("At least"));
                    for (b, label) in [
                        (10u64 << 20, "10 MB"),
                        (100 << 20, "100 MB"),
                        (1 << 30, "1 GB"),
                    ] {
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
                .add_enabled(
                    !a.nav.is_empty(),
                    widgets::small(widgets::icon_text(ic::BACK, "Up", 13.0, color_ink())),
                )
                .clicked()
            {
                acts.push(AAct::Up);
            }
            let root_label = res.root_path.display().to_string();
            if ui
                .add(
                    Label::new(RichText::new(root_label).color(if a.nav.is_empty() {
                        color_ink()
                    } else {
                        color_accent()
                    }))
                    .sense(Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                acts.push(AAct::Truncate(0));
            }
            let mut n = &res.root;
            for (depth, &i) in a.nav.iter().enumerate() {
                let Some(c) = n.children.get(i) else { break };
                n = c;
                ui.label(widgets::icon(ic::CHEVRON, 10.0, color_secondary()));
                let last = depth + 1 == a.nav.len();
                if ui
                    .add(
                        Label::new(RichText::new(&c.name).color(if last { color_ink() } else { color_accent() }))
                            .sense(Sense::click()),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    acts.push(AAct::Truncate(depth + 1));
                }
            }
            if ui.add_enabled(!a.recycle.running(), widgets::small("Refresh this folder")).on_hover_text("Scan only this folder to get current results. Your saved full-drive map stays available.").clicked() { acts.push(AAct::Rescan(cur_path.clone())); }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(widgets::small(widgets::icon_text(
                        ic::OPEN,
                        "Open in Explorer",
                        13.0,
                        color_ink(),
                    )))
                    .clicked()
                {
                    acts.push(AAct::Open(cur_path.clone()));
                }
            });
        });

        let map_h = (ui.available_height() * 0.32).clamp(120.0, 300.0);
        if let Some(i) = treemap(ui, node, map_h) {
            acts.push(AAct::Enter(i));
        }
        ui.add_space(6.0);

        let needle = a.filter.trim().to_lowercase();
        let mut visible: Vec<usize> = (0..node.children.len())
            .filter(|&i| {
                needle.is_empty() || node.children[i].name.to_lowercase().contains(&needle)
            })
            .collect();
        visible.sort_by(|&i, &j| {
            let (left, right) = (&node.children[i], &node.children[j]);
            match a.sort {
                ASort::Size => right.size.cmp(&left.size),
                ASort::Name => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
                ASort::Modified => right.modified.cmp(&left.modified),
                ASort::Files => right.files.cmp(&left.files),
            }
            .then_with(|| left.name.cmp(&right.name))
        });
        ui.horizontal_wrapped(|ui| {
            let mut filter = a.filter.clone();
            if widgets::search_box(ui, &mut filter, "Search this folder", 220.0).changed() {
                acts.push(AAct::Filter(filter));
            }
            ui.label(widgets::faint(format!(
                "{} of {} entries in this folder",
                visible.len(),
                node.children.len()
            )));
            for (sort, label) in [
                (ASort::Size, "Size"),
                (ASort::Name, "Name"),
                (ASort::Modified, "Modified"),
                (ASort::Files, "File count"),
            ] {
                if ui.add(widgets::chip(label, a.sort == sort)).clicked() {
                    acts.push(AAct::Sort(sort));
                }
            }
        });
        let actionable = |index: usize| {
            let child = &node.children[index];
            let p = cur_path.join(&child.name);
            (matches!(child.kind, NodeKind::Dir | NodeKind::File)
                && !path_removed(&p, &a.removed)
                && util::block_reason_fast(&p).is_none())
            .then_some((p, child.size))
        };
        let selected: Vec<_> = visible
            .iter()
            .filter_map(|&i| actionable(i))
            .filter(|(p, _)| a.selected.contains(p))
            .collect();
        let selected_bytes: u64 = selected.iter().map(|(_, bytes)| *bytes).sum();
        ui.horizontal_wrapped(|ui| {
            if ui.add(widgets::small("Select shown")).clicked() {
                acts.push(AAct::SelectMany(
                    visible
                        .iter()
                        .filter_map(|&i| actionable(i))
                        .map(|(p, _)| p)
                        .collect(),
                ));
            }
            if ui
                .add_enabled(!a.selected.is_empty(), widgets::small("Clear selection"))
                .clicked()
            {
                acts.push(AAct::ClearSel);
            }
            ui.label(widgets::dim(format!(
                "{} selected here · {}",
                selected.len(),
                fmt_bytes(selected_bytes)
            )));
            if ui
                .add_enabled(
                    !selected.is_empty() && !a.recycle.running() && !a.selection_check.running(),
                    widgets::danger(if a.selection_check.running() {
                        "Checking selected files…"
                    } else {
                        "Move selected to Recycle Bin…"
                    }),
                )
                .clicked()
            {
                acts.push(AAct::AskRecycle(selected.clone()));
            }
        });
        let parent = node.size.max(1) as f32;
        let compact = ui.available_width() < 900.0;
        ScrollArea::vertical()
            .id_salt("folder_rows")
            .auto_shrink([false, false])
            .show_rows(ui, 38.0, visible.len(), |ui, range| {
                for &idx in &visible[range] {
                    let child = &node.children[idx];
                    let child_path = cur_path.join(&child.name);
                    let removed = path_removed(&child_path, &a.removed);
                    widgets::row(ui, 38.0, |ui| {
                        let block = if matches!(child.kind, NodeKind::Dir | NodeKind::File) { util::block_reason_fast(&child_path) } else { Some("This is an aggregate or inaccessible entry. Open its parent to review real files.") };
                        let mut selected = a.selected.contains(&child_path);
                        let checkbox = ui.add_enabled(block.is_none() && !removed, egui::Checkbox::new(&mut selected, ""));
                        if let Some(reason) = block { checkbox.on_disabled_hover_text(reason); }
                        else if checkbox.changed() { acts.push(AAct::Select(child_path.clone(), selected)); }
                        let (icon_c, col) = match child.kind {
                            NodeKind::Dir => (ic::FOLDER, PALETTE[idx % PALETTE.len()]),
                            NodeKind::File => (ic::FILE, classify(&ext_of(&child.name)).color()),
                            NodeKind::SmallFiles => (ic::FILE, color_secondary()),
                            NodeKind::Denied => (ic::LOCK, color_secondary()),
                        };
                        ui.label(widgets::icon(if block.is_some() { ic::LOCK } else { icon_c }, 16.0, col));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            let actionable =
                                matches!(child.kind, NodeKind::Dir | NodeKind::File) && !removed;
                            if actionable {
                                let btn = ui.add_enabled(
                                    block.is_none() && !a.selection_check.running(),
                                    widgets::small(widgets::icon(ic::CLEAN, 13.0, color_muted())),
                                );
                                let btn = match block {
                                    Some(why) => btn.on_disabled_hover_text(why),
                                    None => btn.on_hover_text("Move to Recycle Bin"),
                                };
                                if btn.clicked() {
                                    acts.push(AAct::AskRecycle(vec![(
                                        child_path.clone(),
                                        child.size,
                                    )]));
                                }
                                if ui
                                    .add(widgets::small(widgets::icon(ic::OPEN, 13.0, color_muted())))
                                    .on_hover_text("Show in Explorer")
                                    .clicked()
                                {
                                    acts.push(AAct::Show(child_path.clone()));
                                }
                            }
                            if !compact { ui.add_sized(
                                [96.0, 20.0],
                                Label::new(widgets::faint(fmt_age(child.modified))),
                            ); }
                            let files = if child.kind == NodeKind::Denied {
                                "no access".to_string()
                            } else {
                                format!("{} files", fmt_num(child.files))
                            };
                            if !compact { ui.add_sized([96.0, 20.0], Label::new(widgets::faint(files))); }
                            if !compact { ui.add_sized(
                                [50.0, 20.0],
                                Label::new(widgets::faint(format!(
                                    "{:.0}%",
                                    child.size as f32 / parent * 100.0
                                ))),
                            ); }
                            ui.add_sized(
                                [84.0, 20.0],
                                Label::new(widgets::strong(fmt_bytes(child.size), 14.0)),
                            );
                            if !compact { widgets::bar(ui, child.size as f32 / parent, col, vec2(110.0, 6.0)); }
                            ui.add_space(8.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let enterable =
                                    child.kind == NodeKind::Dir && !child.children.is_empty();
                                let mut text = RichText::new(&child.name).color(if removed {
                                    color_secondary()
                                } else {
                                    color_ink()
                                });
                                if removed {
                                    text = text.strikethrough();
                                }
                                let resp =
                                    ui.add(Label::new(text).truncate().sense(Sense::click()));
                                let resp = if enterable {
                                    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
                                } else {
                                    resp
                                };
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
            .filter(|f| a.selected.contains(&f.path) && !path_removed(&f.path, &a.removed))
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
                ui.label(widgets::dim(format!(
                    "{} selected, {}",
                    sel_items.len(),
                    fmt_bytes(sel_total)
                )));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !sel_items.is_empty()
                            && !a.recycle.running()
                            && !a.selection_check.running(),
                        widgets::danger(widgets::icon_text(
                            ic::CLEAN,
                            "Move selected to Recycle Bin",
                            14.0,
                            Color32::WHITE,
                        )),
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
            .id_salt("big_files")
            .auto_shrink([false, false])
            .show_rows(ui, 44.0, idx.len(), |ui, range| {
                for &i in &idx[range] {
                    let f = &res.big[i];
                    widgets::row(ui, 44.0, |ui| {
                        let mut sel = a.selected.contains(&f.path);
                        let cb =
                            ui.add_enabled(f.block.is_none(), egui::Checkbox::new(&mut sel, ""));
                        if let Some(why) = f.block {
                            cb.on_disabled_hover_text(why);
                        } else if cb.changed() {
                            acts.push(AAct::Select(f.path.clone(), sel));
                        }
                        let name = f
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let cat = classify(&ext_of(&name));
                        ui.label(widgets::icon(
                            if f.block.is_some() {
                                ic::LOCK
                            } else {
                                ic::FILE
                            },
                            16.0,
                            cat.color(),
                        ));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            if ui
                                .add(widgets::small(widgets::icon(ic::OPEN, 13.0, color_muted())))
                                .on_hover_text("Show in Explorer")
                                .clicked()
                            {
                                acts.push(AAct::Show(f.path.clone()));
                            }
                            ui.add_sized(
                                [110.0, 20.0],
                                Label::new(widgets::faint(fmt_age(f.modified))),
                            );
                            ui.add_sized(
                                [88.0, 20.0],
                                Label::new(widgets::strong(fmt_bytes(f.size), 14.0)),
                            );
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.add(
                                        Label::new(RichText::new(&name).color(color_ink()))
                                            .truncate(),
                                    );
                                    let folder = f
                                        .path
                                        .parent()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_default();
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
            .id_salt("types_scroll")
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
                            let r = Rect::from_min_max(
                                pos2(x, rect.top()),
                                pos2((x + seg).min(rect.right()), rect.bottom()),
                            );
                            ui.painter()
                                .rect_filled(r, Rounding::same(2.0), cat.color());
                            x += seg;
                        }
                        ui.add_space(10.0);
                        for (cat, bytes, count) in &res.cats {
                            ui.horizontal(|ui| {
                                widgets::dot(ui, cat.color());
                                ui.label(cat.label());
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(widgets::faint(format!("{} files", fmt_num(*count))));
                                    ui.add_sized(
                                        [80.0, 20.0],
                                        Label::new(widgets::strong(fmt_bytes(*bytes), 14.0)),
                                    );
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
                                ui.add_sized(
                                    [70.0, 20.0],
                                    Label::new(
                                        RichText::new(format!(".{}", e.ext)).color(color_ink()),
                                    ),
                                );
                                let bw = (ui.available_width() - 190.0).max(20.0);
                                widgets::bar(
                                    ui,
                                    e.bytes as f32 / top,
                                    e.cat.color(),
                                    vec2(bw, 6.0),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(widgets::faint(fmt_num(e.count)));
                                    ui.add_sized(
                                        [80.0, 20.0],
                                        Label::new(widgets::strong(fmt_bytes(e.bytes), 14.0)),
                                    );
                                });
                            });
                        }
                    });
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_fixture(root: &str) -> ScanResult {
        ScanResult {
            root_path: root.into(),
            root: Node {
                name: root.into(),
                size: 20,
                files: 1,
                kind: NodeKind::Dir,
                modified: None,
                children: vec![Node {
                    name: "nested".into(),
                    size: 20,
                    files: 1,
                    kind: NodeKind::Dir,
                    modified: None,
                    children: vec![Node {
                        name: "example.bin".into(),
                        size: 20,
                        files: 1,
                        kind: NodeKind::File,
                        modified: None,
                        children: Vec::new(),
                    }],
                }],
            },
            big: Vec::new(),
            exts: Vec::new(),
            cats: Vec::new(),
            elapsed: Duration::from_secs(1),
            denied: 0,
            dirs: 2,
            cancelled: false,
        }
    }
    #[test]
    fn saved_map_roundtrip_restores_tree_without_recursive_json() {
        let scan = map_fixture(r"C:\");
        let json = serde_json::to_vec(&map_ref(&scan)).unwrap();
        let restored = restore_map(serde_json::from_slice(&json).unwrap()).unwrap();
        assert_eq!(restored.root_path, scan.root_path);
        assert_eq!(restored.root.children[0].children[0].name, "example.bin");
        assert_eq!(restored.root.size, 20);
        let mut broken: DiskMap = serde_json::from_slice(&json).unwrap();
        broken.nodes[1].parent = Some(1);
        assert!(
            restore_map(broken).is_err(),
            "self/cyclic parents must never be followed"
        );
        let mut broken: DiskMap = serde_json::from_slice(&json).unwrap();
        broken.nodes[1].name = "..".into();
        assert!(
            restore_map(broken).is_err(),
            "cached child names cannot escape the scan root"
        );
    }
    #[test]
    fn switching_c_d_c_restores_saved_map_and_view_without_a_scan() {
        let mut a = AnalyzerState::default();
        for (root, filter, tab) in [
            (r"C:\", "example", ATab::Largest),
            (r"D:\", "backup", ATab::Old),
        ] {
            let mut view = MapView::capture(&a);
            view.filter = filter.into();
            view.tab = tab;
            view.nav = vec![0];
            a.cache.recent.push(SavedMap {
                result: Arc::new(map_fixture(root)),
                stamp: 1,
                view,
                read_only: true,
            });
        }
        let c = a.cache.recent[0].result.clone();
        assert!(a.activate_saved(Path::new(r"C:\")));
        assert!(a.activate_saved(Path::new(r"D:\")));
        assert_eq!(a.filter, "backup");
        assert!(a.activate_saved(Path::new(r"C:\")));
        assert_eq!(a.filter, "example");
        assert!(a.tab == ATab::Largest);
        assert_eq!(a.nav, vec![0]);
        assert!(Arc::ptr_eq(a.result.as_ref().unwrap(), &c));
        assert!(!a.scan.running());
        assert!(a.cache.read_only);
        assert!(!a.activate_saved(Path::new(r"E:\")));
    }
    #[test]
    fn fourth_drive_loads_its_saved_map_without_starting_a_scan() {
        let ctx = egui::Context::default();
        let mut app = App::test_fixture();
        app.analyzer = AnalyzerState::default();
        let stamp = crate::scan_cache::now();
        let maps: Vec<_> = [
            r"C:\cache-cycle",
            r"D:\cache-cycle",
            r"E:\cache-cycle",
            r"S:\cache-cycle",
        ]
        .into_iter()
        .map(|root| SavedMap {
            result: Arc::new(map_fixture(root)),
            stamp,
            view: MapView::capture(&app.analyzer),
            read_only: true,
        })
        .collect();
        let index = save_maps(maps.clone(), Vec::new()).unwrap();
        for root in [
            r"C:\cache-cycle",
            r"D:\cache-cycle",
            r"E:\cache-cycle",
            r"S:\cache-cycle",
        ] {
            assert!(index
                .iter()
                .any(|item| util::norm(&item.root) == util::norm(Path::new(root))));
        }
        app.analyzer.cache.disk_index = index;
        app.analyzer.cache.recent = maps.into_iter().take(RECENT_MAPS).collect();
        for root in [
            r"S:\cache-cycle",
            r"C:\cache-cycle",
            r"D:\cache-cycle",
            r"E:\cache-cycle",
            r"S:\cache-cycle",
            r"C:\cache-cycle",
        ] {
            app.start_analyze(PathBuf::from(root), &ctx);
            let deadline = Instant::now() + Duration::from_secs(10);
            while app.analyzer.cache.load.running() {
                app.poll_analyzer_cache(&ctx);
                assert!(Instant::now() < deadline, "cached map read timed out");
                std::thread::yield_now();
            }
            assert!(
                !app.analyzer.scan.running(),
                "a known drive must load its snapshot, not enumerate disk files again"
            );
            assert_eq!(
                app.analyzer.result.as_ref().unwrap().root_path,
                PathBuf::from(root)
            );
            assert!(app.analyzer.cache.recent.len() <= RECENT_MAPS);
        }
    }
    #[test]
    fn removed_folder_hides_descendants_but_not_neighbors() {
        let removed = HashSet::from([PathBuf::from(r"C:\data\photos")]);
        assert!(path_removed(
            Path::new(r"C:\data\photos\trip.jpg"),
            &removed
        ));
        assert!(!path_removed(
            Path::new(r"C:\data\photos-old\trip.jpg"),
            &removed
        ));
    }

    #[test]
    fn typescript_is_classified_as_code() {
        assert!(matches!(classify("ts"), FileCat::Code));
        assert!(matches!(classify("m2ts"), FileCat::Video));
        assert_eq!(ext_of("script.TS"), "ts");
    }

    #[test]
    fn map_layout_preserves_area_and_keeps_rectangles_inside_bounds() {
        let bounds = Rect::from_min_size(pos2(5.0, 10.0), vec2(800.0, 400.0));
        let cells = squarify(&[(0, 60.0), (1, 30.0), (2, 10.0)], bounds);
        assert_eq!(cells.len(), 3);
        let total: f32 = cells.iter().map(|(_, r)| r.area()).sum();
        assert!((total - bounds.area()).abs() < 1.0);
        for (_, cell) in cells {
            assert!(bounds.contains_rect(cell));
        }
    }

    #[test]
    fn scans_leave_a_core_free_and_spare_spinning_disks() {
        assert_eq!(scan_threads(true, 16), 2);
        assert_eq!(scan_threads(false, 16), 8);
        assert_eq!(scan_threads(false, 4), 3);
        assert_eq!(scan_threads(false, 1), 1);
        assert_eq!(scan_threads(true, 1), 1);
        assert_eq!(scan_threads(false, 2), 1);
    }

    #[test]
    fn bounded_scan_honours_cancellation() {
        let progress = Arc::new(Progress::default());
        progress.cancel.store(true, Ordering::Relaxed);
        let result = scan_bounded(std::env::temp_dir(), progress, 2);
        assert!(result.cancelled);
        assert_eq!(result.dirs, 0);
    }

    #[test]
    fn sequential_fallback_keeps_the_same_scan_totals() {
        let root =
            std::env::temp_dir().join(format!("pcsweep-scan-sequential-{}", std::process::id()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("one.txt"), b"123").unwrap();
        fs::write(root.join("nested").join("two.txt"), b"4567").unwrap();
        let result = scan_internal(root.clone(), Arc::new(Progress::default()), false);
        assert_eq!(result.root.files, 2);
        assert_eq!(result.root.size, 7);
        assert_eq!(result.dirs, 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_scan_does_not_visit_folders() {
        let progress = Arc::new(Progress::default());
        progress.cancel.store(true, Ordering::Relaxed);
        let result = scan(std::env::temp_dir(), progress);
        assert!(result.cancelled);
        assert_eq!(result.dirs, 0);
        assert_eq!(result.root.files, 0);
    }
}

#[cfg(test)]
mod responsiveness_tests {
    use super::*;
    #[test]
    fn scan_results_do_not_wait_for_snapshot_persistence_or_the_selected_tab() {
        let ctx = egui::Context::default();
        let mut app = App::test_fixture();
        let dir =
            std::env::temp_dir().join(format!("pcsweep-responsive-scan-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for i in 0..128 {
            fs::write(dir.join(format!("{i}.tmp")), b"example").unwrap();
        }
        let (release, wait) = std::sync::mpsc::channel();
        app.analyzer.cache.save = Task::spawn(&ctx, move || {
            wait.recv().unwrap();
            Ok(Vec::new())
        });
        app.force_analyze(dir.clone(), &ctx);
        app.page = Page::Memory;
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.analyzer.scan.running() {
            app.poll_analyzer();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(app.page == Page::Memory);
        assert_eq!(app.analyzer.result.as_ref().unwrap().root.files, 128);
        assert!(
            app.analyzer.cache.save.running(),
            "a completed map must be usable before saving finishes"
        );
        release.send(()).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn stalled_restore_times_out_without_waiting_for_its_worker() {
        let ctx = egui::Context::default();
        let mut app = App::test_fixture();
        let (release, wait) = std::sync::mpsc::channel();
        app.analyzer.cache.load = Task::spawn(&ctx, move || {
            wait.recv().unwrap();
            Ok(None)
        });
        app.analyzer.cache.load_started = Some(Instant::now() - Duration::from_secs(31));
        app.poll_analyzer_cache(&ctx);
        assert!(!app.analyzer.cache.load.running());
        assert!(app.analyzer.cache.error.is_some());
        release.send(()).unwrap();
    }
    #[test]
    fn cached_file_selection_can_be_validated_without_remapping_the_drive() {
        let ctx = egui::Context::default();
        let mut app = App::test_fixture();
        let dir =
            std::env::temp_dir().join(format!("pcsweep-saved-selection-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("review-me.tmp");
        fs::write(&path, b"new contents").unwrap();
        mark_preview_saved(&mut app.analyzer);
        let items = vec![(path.clone(), 1)];
        app.analyzer.selection_check = Task::spawn(&ctx, move || util::check_selected(items));
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.analyzer.selection_check.running() {
            app.poll_analyzer();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let modal = app.modal.unwrap();
        match modal.action {
            Action::RecycleChecked(items) => {
                assert_eq!(items[0].size, 12);
                assert_eq!(items[0].path, path);
            }
            _ => panic!("current-file confirmation required"),
        }
        assert!(!app.analyzer.scan.running());
        fs::remove_dir_all(dir).unwrap();
    }
}
