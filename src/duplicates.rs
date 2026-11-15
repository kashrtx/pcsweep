use crate::app::{App, Origin, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes, fmt_date, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, ScrollArea, Ui};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};
use xxhash_rust::xxh3::{xxh3_128, Xxh3};

#[derive(Default)]
pub struct DupProgress {
    pub stage: Mutex<String>,
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

#[derive(Clone)]
pub struct DupFile {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub block: Option<&'static str>,
}

#[derive(Clone)]
pub struct DupGroup {
    pub size: u64,
    pub files: Vec<DupFile>,
}

impl DupGroup {
    pub fn wasted(&self) -> u64 {
        self.reclaimable(&HashSet::new())
    }

    fn reclaimable(&self, removed: &HashSet<PathBuf>) -> u64 {
        let mut live = 0usize;
        let mut removable = 0usize;
        for file in &self.files {
            if !removed.contains(&file.path) {
                live += 1;
                removable += usize::from(file.block.is_none());
            }
        }
        self.size
            .saturating_mul(removable.min(live.saturating_sub(1)) as u64)
    }
}

fn set_stage(p: &DupProgress, s: &str, total: u64) {
    if let Ok(mut st) = p.stage.lock() {
        *st = s.to_string();
    }
    p.done.store(0, Ordering::Relaxed);
    p.total.store(total, Ordering::Relaxed);
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> Option<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => return None,
        }
    }
    Some(n)
}

// Deny writers while inspecting a file. Selected duplicates permit rename/deletion
// so the same open handle can remain alive during the Recycle Bin operation.
fn open_stable(p: &Path, allow_delete: bool) -> Option<File> {
    let md = fs::symlink_metadata(p).ok()?;
    if !md.is_file() || util::is_reparse(&md) || md.file_attributes() & util::CLOUD_MASK != 0 {
        return None;
    }
    fs::OpenOptions::new()
        .read(true)
        .share_mode(if allow_delete { 0x1 | 0x4 } else { 0x1 })
        .open(p)
        .ok()
}

// File IDs distinguish a renamed/replaced path from the handle whose bytes
// were checked. Hard links are excluded because removing one does not reclaim
// the file's data and another alias could otherwise be mistaken for a backup.
fn file_identity(file: &File) -> Option<((u32, u64), u32)> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, info.as_mut_ptr()) };
    if ok == 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some((
        (
            info.dwVolumeSerialNumber,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ),
        info.nNumberOfLinks,
    ))
}

fn path_matches_handle(path: &Path, file: &File) -> bool {
    let Some(current) = open_stable(path, true) else {
        return false;
    };
    match (file_identity(file), file_identity(&current)) {
        (Some((expected, 1)), Some((actual, 1))) => expected == actual,
        _ => false,
    }
}

fn matches_snapshot(file: &File, size: u64, modified: Option<SystemTime>) -> bool {
    file.metadata()
        .is_ok_and(|m| m.len() == size && modified.is_some() && m.modified().ok() == modified)
        && file_identity(file).is_some_and(|(_, links)| links == 1)
}

fn equal_contents(a: &mut File, b: &mut File, cancel: &AtomicBool) -> bool {
    let mut left = vec![0; 256 * 1024];
    let mut right = vec![0; 256 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let (Some(ln), Some(rn)) = (read_up_to(a, &mut left), read_up_to(b, &mut right)) else {
            return false;
        };
        if ln != rn || left[..ln] != right[..rn] {
            return false;
        }
        if ln == 0 {
            return true;
        }
    }
}

fn hash_partial(p: &Path) -> Option<u128> {
    let mut f = open_stable(p, false)?;
    let mut buf = vec![0u8; 64 * 1024];
    let n = read_up_to(&mut f, &mut buf)?;
    Some(xxh3_128(&buf[..n]))
}

fn hash_full(p: &Path, cancel: &AtomicBool) -> Option<u128> {
    let mut f = open_stable(p, false)?;
    if file_identity(&f)?.1 != 1 {
        return None;
    }
    let mut h = Xxh3::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(h.digest128())
}

pub fn find(roots: Vec<PathBuf>, min_size: u64, p: Arc<DupProgress>) -> Vec<DupGroup> {
    // 1. List files.
    set_stage(&p, "Listing files", 0);
    let mut all: Vec<(PathBuf, u64, Option<SystemTime>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut visited_dirs = HashSet::new();
    let mut stack: Vec<PathBuf> = roots;
    while let Some(dir) = stack.pop() {
        if p.cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let Ok(md) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if !md.is_dir() || util::is_reparse(&md) || !visited_dirs.insert(util::norm(&dir)) {
            continue;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if p.cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let Ok(md) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            let attrs = md.file_attributes();
            if attrs & util::ATTR_REPARSE != 0 || md.file_type().is_symlink() {
                continue;
            }
            if md.is_dir() {
                stack.push(e.path());
                continue;
            }
            if !md.is_file() || attrs & util::CLOUD_MASK != 0 {
                continue;
            }
            let len = md.len();
            if len >= min_size {
                let path = e.path();
                if seen.insert(util::norm(&path)) {
                    all.push((path, len, md.modified().ok()));
                    p.done.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    // 2. Same size is required for a duplicate.
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, f) in all.iter().enumerate() {
        by_size.entry(f.1).or_default().push(i);
    }
    let cand: Vec<usize> = by_size
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();

    // 3. Compare the first 64 KB.
    set_stage(&p, "Comparing file starts", cand.len() as u64);
    let partial: Vec<(u64, u128, usize)> = cand
        .par_iter()
        .filter_map(|&i| {
            if p.cancel.load(Ordering::Relaxed) {
                return None;
            }
            let r = hash_partial(&all[i].0).map(|h| (all[i].1, h, i));
            p.done.fetch_add(1, Ordering::Relaxed);
            r
        })
        .collect();
    let mut by_partial: HashMap<(u64, u128), Vec<usize>> = HashMap::new();
    for (size, h, i) in partial {
        by_partial.entry((size, h)).or_default().push(i);
    }
    let cand2: Vec<usize> = by_partial
        .into_values()
        .filter(|v| v.len() > 1)
        .flatten()
        .collect();

    // 4. Verify full contents.
    set_stage(&p, "Verifying full contents", cand2.len() as u64);
    let full: Vec<(u64, u128, usize)> = cand2
        .par_iter()
        .filter_map(|&i| {
            let r = hash_full(&all[i].0, &p.cancel).map(|h| (all[i].1, h, i));
            p.done.fetch_add(1, Ordering::Relaxed);
            r
        })
        .collect();
    if p.cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    let mut by_full: HashMap<(u64, u128), Vec<usize>> = HashMap::new();
    for (size, h, i) in full {
        by_full.entry((size, h)).or_default().push(i);
    }

    // Hashes narrow down candidates; only an actual byte comparison can prove
    // equality. Also reject files changed since directory enumeration.
    let buckets: Vec<Vec<usize>> = by_full.into_values().filter(|v| v.len() > 1).collect();
    let comparisons = buckets.iter().map(Vec::len).sum::<usize>();
    set_stage(
        &p,
        "Checking matching files byte for byte",
        comparisons as u64,
    );
    let mut groups: Vec<DupGroup> = buckets
        .into_par_iter()
        .flat_map_iter(|indices| {
            let mut verified: Vec<Vec<usize>> = Vec::new();
            for i in indices {
                if p.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let (path, size, modified) = &all[i];
                let mut matched = false;
                for group in &mut verified {
                    let other = &all[group[0]];
                    let (Some(mut a), Some(mut b)) =
                        (open_stable(&other.0, false), open_stable(path, false))
                    else {
                        continue;
                    };
                    if matches_snapshot(&a, other.1, other.2)
                        && matches_snapshot(&b, *size, *modified)
                        && equal_contents(&mut a, &mut b, &p.cancel)
                    {
                        group.push(i);
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    verified.push(vec![i]);
                }
                p.done.fetch_add(1, Ordering::Relaxed);
            }
            verified
                .into_iter()
                .filter(|g| g.len() > 1)
                .map(|v| {
                    let size = all[v[0]].1;
                    let mut files: Vec<DupFile> = v
                        .into_iter()
                        .map(|i| DupFile {
                            block: util::block_reason_fast(&all[i].0),
                            path: all[i].0.clone(),
                            modified: all[i].2,
                        })
                        .collect();
                    files.sort_by(|a, b| {
                        a.modified
                            .cmp(&b.modified)
                            .then_with(|| a.path.cmp(&b.path))
                    });
                    DupGroup { size, files }
                })
                .collect::<Vec<_>>()
        })
        .collect();
    if p.cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()));
    groups.truncate(3000);
    groups
}

/// Recheck duplicates at confirmation time, holding the kept copy open so
/// another app cannot change/delete it while selected copies are recycled.
pub(crate) fn recycle_verified(
    items: Vec<(PathBuf, u64)>,
    groups: Vec<DupGroup>,
) -> util::RecycleReport {
    use std::io::{Seek, SeekFrom};
    let selected: HashSet<_> = items.iter().map(|(p, _)| p.clone()).collect();
    let mut pending: HashMap<_, _> = items.into_iter().collect();
    let mut report = util::RecycleReport {
        done: Vec::new(),
        failed: Vec::new(),
        bytes: 0,
    };
    let cancel = AtomicBool::new(false);
    for group in groups {
        let targets: Vec<_> = group
            .files
            .iter()
            .filter(|f| pending.contains_key(&f.path))
            .collect();
        if targets.is_empty() {
            continue;
        }
        let keeper = group
            .files
            .iter()
            .filter(|f| !selected.contains(&f.path))
            .find_map(|f| {
                let handle = open_stable(&f.path, false)?;
                matches_snapshot(&handle, group.size, f.modified).then_some(handle)
            });
        let Some(mut keeper) = keeper else {
            for f in targets {
                pending.remove(&f.path);
                report.failed.push((
                    f.path.clone(),
                    "The copy to keep changed or is unavailable. Scan again.".into(),
                ));
            }
            continue;
        };
        for target in targets {
            pending.remove(&target.path);
            let Some(mut file) = open_stable(&target.path, true) else {
                report.failed.push((
                    target.path.clone(),
                    "File changed or is in use. Scan again.".into(),
                ));
                continue;
            };
            if !matches_snapshot(&file, group.size, target.modified)
                || keeper.seek(SeekFrom::Start(0)).is_err()
                || !equal_contents(&mut keeper, &mut file, &cancel)
                || !path_matches_handle(&target.path, &file)
            {
                report.failed.push((
                    target.path.clone(),
                    "File contents changed. Scan again before removing it.".into(),
                ));
                continue;
            }
            let result = util::recycle(vec![(target.path.clone(), group.size)]);
            report.bytes += result.bytes;
            report.done.extend(result.done);
            report.failed.extend(result.failed);
        }
    }
    report.failed.extend(pending.into_keys().map(|p| {
        (
            p,
            "File is no longer in these duplicate results. Scan again.".into(),
        )
    }));
    report
}

// ====================================================================== state

pub struct DupState {
    pub roots: Vec<PathBuf>,
    pub min_size: u64,
    pub scan: Task<Vec<DupGroup>>,
    pub progress: Option<Arc<DupProgress>>,
    pub started: Option<Instant>,
    pub groups: Option<Vec<DupGroup>>,
    pub selected: HashSet<PathBuf>,
    pub removed: HashSet<PathBuf>,
    pub recycle: Task<util::RecycleReport>,
}

impl Default for DupState {
    fn default() -> Self {
        Self {
            roots: vec![util::folders().profile.clone()],
            min_size: 1024 * 1024,
            scan: Task::default(),
            progress: None,
            started: None,
            groups: None,
            selected: HashSet::new(),
            removed: HashSet::new(),
            recycle: Task::default(),
        }
    }
}

enum DAct {
    AddFolder,
    RemoveRoot(usize),
    MinSize(u64),
    Start,
    Cancel,
    Toggle(usize, PathBuf, bool),
    AutoKeep(bool),
    ClearSel,
    Show(PathBuf),
    AskRecycle,
}

impl App {
    pub fn poll_duplicates(&mut self) {
        if let Some(r) = self.dups.scan.poll() {
            match r {
                Ok(groups) => {
                    let wasted: u64 = groups.iter().map(|g| g.wasted()).sum();
                    util::log(format!(
                        "Duplicate scan found {} groups, {} reclaimable",
                        groups.len(),
                        fmt_bytes(wasted)
                    ));
                    if self
                        .dups
                        .progress
                        .as_ref()
                        .is_some_and(|p| p.cancel.load(Ordering::Relaxed))
                    {
                        self.toast(
                            ToastKind::Info,
                            "Duplicate scan stopped. Start a new scan when ready.",
                        );
                    } else {
                        self.dups.groups = Some(groups);
                    }
                }
                Err(e) => self.toast(ToastKind::Error, format!("Duplicate scan failed: {e}")),
            }
            self.dups.progress = None;
        }
        if let Some(r) = self.dups.recycle.poll() {
            match r {
                Ok(rep) => {
                    let n = rep.done.len();
                    for p in rep.done {
                        self.dups.selected.remove(&p);
                        self.dups.removed.insert(p);
                    }
                    if n > 0 {
                        self.toast(
                            ToastKind::Success,
                            format!(
                                "Moved {} duplicate(s), {}, to the Recycle Bin",
                                n,
                                fmt_bytes(rep.bytes)
                            ),
                        );
                    }
                    if !rep.failed.is_empty() {
                        self.toast(
                            ToastKind::Error,
                            format!("{} file(s) could not be moved", rep.failed.len()),
                        );
                    }
                    self.sys.refresh_drives();
                }
                Err(e) => self.toast(ToastKind::Error, format!("Recycle failed: {e}")),
            }
        }
    }

    fn dup_group_live(&self, g: &DupGroup) -> usize {
        g.files
            .iter()
            .filter(|f| !self.dups.removed.contains(&f.path))
            .count()
    }

    pub fn page_duplicates(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        self.render_duplicates(ui, &mut acts);
        for a in acts {
            match a {
                DAct::AddFolder => {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_title("Add a folder to search")
                        .pick_folder()
                    {
                        if !self.dups.roots.contains(&p) {
                            self.dups.roots.push(p);
                        }
                    }
                }
                DAct::RemoveRoot(i) => {
                    if i < self.dups.roots.len() {
                        self.dups.roots.remove(i);
                    }
                }
                DAct::MinSize(b) => self.dups.min_size = b,
                DAct::Start => {
                    if self.dups.roots.is_empty()
                        || self.dups.scan.running()
                        || self.dups.recycle.running()
                    {
                        continue;
                    }
                    let prog = Arc::new(DupProgress::default());
                    let p2 = prog.clone();
                    let roots = self.dups.roots.clone();
                    let min = self.dups.min_size;
                    self.dups.scan = Task::spawn(&ctx, move || find(roots, min, p2));
                    self.dups.progress = Some(prog);
                    self.dups.started = Some(Instant::now());
                    self.dups.groups = None;
                    self.dups.selected.clear();
                    self.dups.removed.clear();
                }
                DAct::Cancel => {
                    if let Some(p) = &self.dups.progress {
                        p.cancel.store(true, Ordering::Relaxed);
                    }
                }
                DAct::Toggle(gi, path, on) => {
                    if !on {
                        self.dups.selected.remove(&path);
                    } else {
                        // Always keep at least one copy.
                        let remaining = self
                            .dups
                            .groups
                            .as_ref()
                            .and_then(|gs| gs.get(gi))
                            .map(|g| {
                                g.files
                                    .iter()
                                    .filter(|f| {
                                        !self.dups.removed.contains(&f.path)
                                            && !self.dups.selected.contains(&f.path)
                                            && f.path != path
                                    })
                                    .count()
                            })
                            .unwrap_or(0);
                        if remaining == 0 {
                            self.toast(ToastKind::Info, "At least one copy is always kept.");
                        } else {
                            self.dups.selected.insert(path);
                        }
                    }
                }
                DAct::AutoKeep(newest) => {
                    if let Some(groups) = &self.dups.groups {
                        let mut sel = HashSet::new();
                        for g in groups {
                            let live: Vec<&DupFile> = g
                                .files
                                .iter()
                                .filter(|f| !self.dups.removed.contains(&f.path))
                                .collect();
                            if live.len() < 2 {
                                continue;
                            }
                            // files are sorted oldest first
                            let keep = if newest {
                                live[live.len() - 1]
                            } else {
                                live[0]
                            };
                            for f in live {
                                if f.block.is_none() && f.path != keep.path {
                                    sel.insert(f.path.clone());
                                }
                            }
                        }
                        self.dups.selected = sel;
                    }
                }
                DAct::ClearSel => self.dups.selected.clear(),
                DAct::Show(p) => util::show_in_explorer(&p),
                DAct::AskRecycle => {
                    let mut items = Vec::new();
                    if let Some(groups) = &self.dups.groups {
                        for g in groups {
                            for f in &g.files {
                                if self.dups.selected.contains(&f.path) {
                                    items.push((f.path.clone(), g.size));
                                }
                            }
                        }
                    }
                    self.ask_recycle(items, Origin::Duplicates);
                }
            }
        }
    }

    fn render_duplicates(&self, ui: &mut Ui, acts: &mut Vec<DAct>) {
        let d = &self.dups;
        widgets::page_header(
            ui,
            "Duplicate files",
            "Finds files with exactly the same contents, byte for byte, so you only keep one copy.",
        );
        let scanning = d.scan.running();

        widgets::card(ui, |ui| {
            ui.label(widgets::strong("Where to look", 15.0));
            ui.horizontal_wrapped(|ui| {
                for (i, r) in d.roots.iter().enumerate() {
                    widgets::tight_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(widgets::icon(ic::FOLDER, 14.0, ACCENT));
                            ui.label(r.display().to_string());
                            if !scanning
                                && ui
                                    .add(
                                        egui::Button::new(widgets::icon(ic::CLOSE, 10.0, TEXT_DIM))
                                            .frame(false),
                                    )
                                    .clicked()
                            {
                                acts.push(DAct::RemoveRoot(i));
                            }
                        });
                    });
                }
                if ui
                    .add_enabled(
                        !scanning,
                        widgets::ghost(widgets::icon_text(ic::FOLDER, "Add folder", 14.0, TEXT)),
                    )
                    .clicked()
                {
                    acts.push(DAct::AddFolder);
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::dim("Ignore files smaller than"));
                for (b, label) in [
                    (100u64 << 10, "100 KB"),
                    (1 << 20, "1 MB"),
                    (10 << 20, "10 MB"),
                    (100 << 20, "100 MB"),
                ] {
                    if ui
                        .add_enabled(!scanning, widgets::chip(label, d.min_size == b))
                        .clicked()
                    {
                        acts.push(DAct::MinSize(b));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if scanning {
                        if ui.add(widgets::ghost("Stop")).clicked() {
                            acts.push(DAct::Cancel);
                        }
                    } else if ui
                        .add_enabled(
                            !d.roots.is_empty() && !d.recycle.running(),
                            widgets::primary(widgets::icon_text(
                                ic::SEARCH,
                                "Find duplicates",
                                14.0,
                                Color32::WHITE,
                            )),
                        )
                        .clicked()
                    {
                        acts.push(DAct::Start);
                    }
                });
            });
        });
        ui.add_space(6.0);

        if scanning {
            if let Some(p) = &d.progress {
                widgets::card(ui, |ui| {
                    let stage = p.stage.lock().map(|s| s.clone()).unwrap_or_default();
                    let done = p.done.load(Ordering::Relaxed);
                    let total = p.total.load(Ordering::Relaxed);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(widgets::strong(stage, 16.0));
                    });
                    if total > 0 {
                        let bw = ui.available_width().min(520.0);
                        widgets::bar(ui, done as f32 / total as f32, ACCENT, egui::vec2(bw, 8.0));
                        ui.label(widgets::dim(format!(
                            "{} of {} files",
                            fmt_num(done),
                            fmt_num(total)
                        )));
                    } else {
                        ui.label(widgets::dim(format!(
                            "{} files found so far",
                            fmt_num(done)
                        )));
                    }
                });
            }
            return;
        }

        let Some(groups) = &d.groups else {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(widgets::icon(ic::COPY, 40.0, ACCENT));
                ui.add_space(6.0);
                ui.label(widgets::strong("Search your user folder, or add other folders and drives", 17.0));
                ui.label(widgets::dim(
                    "Duplicates are only ever moved to the Recycle Bin, and one copy of every file is always kept.",
                ));
            });
            return;
        };

        let live_groups: Vec<usize> = (0..groups.len())
            .filter(|&i| self.dup_group_live(&groups[i]) > 1)
            .collect();
        let wasted: u64 = live_groups
            .iter()
            .map(|&i| groups[i].reclaimable(&d.removed))
            .sum();
        let sel_bytes: u64 = groups
            .iter()
            .flat_map(|g| g.files.iter().map(move |f| (f, g.size)))
            .filter(|(f, _)| d.selected.contains(&f.path))
            .map(|(_, s)| s)
            .sum();

        if live_groups.is_empty() {
            widgets::banner(ui, GOOD, ic::CHECK, |ui| {
                ui.label("No duplicates found here.");
            });
            return;
        }

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(fmt_bytes(wasted))
                    .font(widgets::light(30.0))
                    .color(TEXT),
            );
            ui.label(widgets::dim(format!(
                "could be freed across {} sets of duplicates",
                fmt_num(live_groups.len() as u64)
            )));
        });
        ui.horizontal(|ui| {
            if ui.add(widgets::small("Keep the newest copy")).clicked() {
                acts.push(DAct::AutoKeep(true));
            }
            if ui.add(widgets::small("Keep the oldest copy")).clicked() {
                acts.push(DAct::AutoKeep(false));
            }
            if !d.selected.is_empty() && ui.add(widgets::small("Clear selection")).clicked() {
                acts.push(DAct::ClearSel);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !d.selected.is_empty() && !d.recycle.running(),
                        widgets::danger(widgets::icon_text(
                            ic::CLEAN,
                            &format!("Move {} selected to Recycle Bin", fmt_bytes(sel_bytes)),
                            14.0,
                            Color32::WHITE,
                        )),
                    )
                    .clicked()
                {
                    acts.push(DAct::AskRecycle);
                }
                if d.recycle.running() {
                    ui.spinner();
                }
            });
        });
        ui.add_space(4.0);

        ScrollArea::vertical()
            .id_salt("dups_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for &gi in live_groups.iter().take(400) {
                    let g = &groups[gi];
                    widgets::tight_frame().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(widgets::strong(
                                format!("{} copies", self.dup_group_live(g)),
                                14.5,
                            ));
                            ui.label(widgets::dim(format!(
                                "{} each, {} reclaimable",
                                fmt_bytes(g.size),
                                fmt_bytes(g.reclaimable(&d.removed))
                            )));
                        });
                        for f in &g.files {
                            if d.removed.contains(&f.path) {
                                continue;
                            }
                            widgets::row(ui, 28.0, |ui| {
                                let mut sel = d.selected.contains(&f.path);
                                let cb = ui.add_enabled(
                                    f.block.is_none(),
                                    egui::Checkbox::new(&mut sel, ""),
                                );
                                if let Some(why) = f.block {
                                    cb.on_disabled_hover_text(why);
                                } else if cb.changed() {
                                    acts.push(DAct::Toggle(gi, f.path.clone(), sel));
                                }
                                if f.block.is_some() {
                                    ui.label(widgets::icon(ic::LOCK, 13.0, TEXT_FAINT));
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.add_space(6.0);
                                    if ui.add(widgets::small("Show")).clicked() {
                                        acts.push(DAct::Show(f.path.clone()));
                                    }
                                    ui.add_sized(
                                        [110.0, 20.0],
                                        Label::new(widgets::faint(fmt_date(f.modified))),
                                    );
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        let col = if sel { TEXT } else { TEXT_DIM };
                                        ui.add(
                                            Label::new(
                                                RichText::new(f.path.display().to_string())
                                                    .color(col),
                                            )
                                            .truncate(),
                                        );
                                    });
                                });
                            });
                        }
                    });
                    ui.add_space(2.0);
                }
                if live_groups.len() > 400 {
                    ui.label(widgets::faint(
                        "Showing the 400 sets that waste the most space.",
                    ));
                }
                ui.add_space(20.0);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "pcsweep-dups-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn byte_comparison_checks_past_the_partial_hash_window() {
        let dir = TestDir::new();
        let left = dir.0.join("left");
        let right = dir.0.join("right");
        let data = vec![7; 300_000];
        fs::write(&left, &data).unwrap();
        fs::write(&right, &data).unwrap();
        assert!(equal_contents(
            &mut File::open(&left).unwrap(),
            &mut File::open(&right).unwrap(),
            &AtomicBool::new(false)
        ));
        let mut changed = data;
        changed[299_999] = 8;
        fs::write(&right, changed).unwrap();
        assert_eq!(hash_partial(&left), hash_partial(&right));
        assert!(!equal_contents(
            &mut File::open(&left).unwrap(),
            &mut File::open(&right).unwrap(),
            &AtomicBool::new(false)
        ));
    }

    #[test]
    fn overlapping_roots_do_not_create_fake_copies() {
        let dir = TestDir::new();
        let sub = dir.0.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("one"), b"only one copy").unwrap();
        assert!(find(
            vec![dir.0.clone(), sub],
            1,
            Arc::new(DupProgress::default())
        )
        .is_empty());
        fs::write(dir.0.join("two"), b"only one copy").unwrap();
        let groups = find(vec![dir.0.clone()], 1, Arc::new(DupProgress::default()));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
    }

    #[test]
    fn hard_links_are_not_reported_as_reclaimable_duplicates() {
        let dir = TestDir::new();
        let path = dir.0.join("original");
        fs::write(&path, b"same data").unwrap();
        fs::hard_link(&path, dir.0.join("linked")).unwrap();
        fs::write(dir.0.join("separate"), b"same data").unwrap();
        assert!(find(vec![dir.0.clone()], 1, Arc::new(DupProgress::default())).is_empty());
    }

    #[test]
    fn replaced_path_does_not_match_the_verified_handle() {
        let dir = TestDir::new();
        let path = dir.0.join("target");
        fs::write(&path, b"same").unwrap();
        let handle = open_stable(&path, true).unwrap();
        fs::rename(&path, dir.0.join("old")).unwrap();
        fs::write(&path, b"same").unwrap();
        assert!(!path_matches_handle(&path, &handle));
    }

    #[test]
    fn kept_file_cannot_be_modified_or_deleted_during_verification() {
        let dir = TestDir::new();
        let path = dir.0.join("keeper");
        fs::write(&path, b"keep me").unwrap();
        let handle = open_stable(&path, false).unwrap();
        assert!(fs::write(&path, b"changed").is_err());
        assert!(fs::remove_file(&path).is_err());
        drop(handle);
        assert!(fs::write(&path, b"changed").is_ok());
    }

    #[test]
    fn missing_keeper_refuses_recycling_the_last_existing_copy() {
        let dir = TestDir::new();
        let path = dir.0.join("last-copy");
        fs::write(&path, b"precious").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().ok();
        let group = DupGroup {
            size: 8,
            files: vec![
                DupFile {
                    path: dir.0.join("missing"),
                    modified,
                    block: None,
                },
                DupFile {
                    path: path.clone(),
                    modified,
                    block: None,
                },
            ],
        };
        let report = recycle_verified(vec![(path.clone(), 8)], vec![group]);
        assert!(report.done.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(fs::read(path).unwrap(), b"precious");
    }

    #[test]
    fn selecting_every_copy_is_rejected_at_execution_time() {
        let dir = TestDir::new();
        let files: Vec<_> = ["a", "b"]
            .iter()
            .map(|name| {
                let path = dir.0.join(name);
                fs::write(&path, b"same").unwrap();
                DupFile {
                    modified: fs::metadata(&path).unwrap().modified().ok(),
                    path,
                    block: None,
                }
            })
            .collect();
        let items = files.iter().map(|f| (f.path.clone(), 4)).collect();
        let report = recycle_verified(items, vec![DupGroup { size: 4, files }]);
        assert!(report.done.is_empty());
        assert_eq!(report.failed.len(), 2);
        assert!(dir.0.join("a").exists() && dir.0.join("b").exists());
    }

    #[test]
    fn changed_keeper_or_target_is_never_recycled() {
        for changed_index in 0..2 {
            let dir = TestDir::new();
            let files: Vec<_> = ["keeper", "target"]
                .iter()
                .map(|name| {
                    let path = dir.0.join(name);
                    fs::write(&path, b"same").unwrap();
                    DupFile {
                        modified: fs::metadata(&path).unwrap().modified().ok(),
                        path,
                        block: None,
                    }
                })
                .collect();
            fs::write(&files[changed_index].path, b"diff").unwrap();
            let target = files[1].path.clone();
            let report = recycle_verified(vec![(target, 4)], vec![DupGroup { size: 4, files }]);
            assert!(report.done.is_empty());
            assert_eq!(report.failed.len(), 1);
            assert!(dir.0.join("keeper").exists() && dir.0.join("target").exists());
        }
    }

    #[test]
    fn verified_recycle_keeps_one_copy() {
        let dir = TestDir::new();
        let files: Vec<_> = ["keeper", "target"]
            .iter()
            .map(|name| {
                let path = dir.0.join(name);
                fs::write(&path, b"same").unwrap();
                DupFile {
                    modified: fs::metadata(&path).unwrap().modified().ok(),
                    path,
                    block: None,
                }
            })
            .collect();
        let keeper = files[0].path.clone();
        let target = files[1].path.clone();
        let report = recycle_verified(vec![(target.clone(), 4)], vec![DupGroup { size: 4, files }]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.done, vec![target.clone()]);
        assert!(!target.exists());
        assert_eq!(fs::read(keeper).unwrap(), b"same");
    }

    #[test]
    fn reclaimable_total_excludes_protected_and_removed_copies() {
        let group = DupGroup {
            size: 100,
            files: (0..4)
                .map(|n| DupFile {
                    path: PathBuf::from(n.to_string()),
                    modified: None,
                    block: (n < 2).then_some("protected"),
                })
                .collect(),
        };
        assert_eq!(group.wasted(), 200);
        assert_eq!(group.reclaimable(&HashSet::from([PathBuf::from("2")])), 100);
        assert_eq!(
            group.reclaimable(&HashSet::from([PathBuf::from("2"), PathBuf::from("3")])),
            0
        );
    }
}
