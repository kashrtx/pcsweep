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
use std::os::windows::fs::MetadataExt;
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

pub struct DupFile {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub block: Option<&'static str>,
}

pub struct DupGroup {
    pub size: u64,
    pub files: Vec<DupFile>,
}

impl DupGroup {
    pub fn wasted(&self) -> u64 {
        self.size * (self.files.len().saturating_sub(1) as u64)
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

fn hash_partial(p: &Path) -> Option<u128> {
    let mut f = File::open(p).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let n = read_up_to(&mut f, &mut buf)?;
    Some(xxh3_128(&buf[..n]))
}

fn hash_full(p: &Path, cancel: &AtomicBool) -> Option<u128> {
    let mut f = File::open(p).ok()?;
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
    let mut stack: Vec<PathBuf> = roots;
    while let Some(dir) = stack.pop() {
        if p.cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            let attrs = md.file_attributes();
            if attrs & util::ATTR_REPARSE != 0 || md.file_type().is_symlink() {
                continue;
            }
            if md.is_dir() {
                stack.push(e.path());
                continue;
            }
            if attrs & util::CLOUD_MASK != 0 {
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
    let cand: Vec<usize> = by_size.into_values().filter(|v| v.len() > 1).flatten().collect();

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
    let cand2: Vec<usize> = by_partial.into_values().filter(|v| v.len() > 1).flatten().collect();

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

    let mut groups: Vec<DupGroup> = by_full
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|((size, _), v)| {
            let mut files: Vec<DupFile> = v
                .into_iter()
                .map(|i| DupFile {
                    block: util::block_reason_fast(&all[i].0),
                    path: all[i].0.clone(),
                    modified: all[i].2,
                })
                .collect();
            files.sort_by(|a, b| a.modified.cmp(&b.modified));
            DupGroup { size, files }
        })
        .collect();
    groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()));
    groups.truncate(3000);
    groups
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
                    util::log(format!("Duplicate scan found {} groups, {} reclaimable", groups.len(), fmt_bytes(wasted)));
                    self.dups.groups = Some(groups);
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
                        self.toast(ToastKind::Success, format!("Moved {} duplicate(s), {}, to the Recycle Bin", n, fmt_bytes(rep.bytes)));
                    }
                    if !rep.failed.is_empty() {
                        self.toast(ToastKind::Error, format!("{} file(s) could not be moved", rep.failed.len()));
                    }
                    self.sys.refresh_drives();
                }
                Err(e) => self.toast(ToastKind::Error, format!("Recycle failed: {e}")),
            }
        }
    }

    fn dup_group_live(&self, g: &DupGroup) -> usize {
        g.files.iter().filter(|f| !self.dups.removed.contains(&f.path)).count()
    }

    pub fn page_duplicates(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        self.render_duplicates(ui, &mut acts);
        for a in acts {
            match a {
                DAct::AddFolder => {
                    if let Some(p) = rfd::FileDialog::new().set_title("Add a folder to search").pick_folder() {
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
                    if self.dups.roots.is_empty() || self.dups.scan.running() {
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
                            let live: Vec<&DupFile> = g.files.iter().filter(|f| !self.dups.removed.contains(&f.path)).collect();
                            if live.len() < 2 {
                                continue;
                            }
                            // files are sorted oldest first
                            let keep = if newest { live[live.len() - 1] } else { live[0] };
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
                            if !scanning && ui.add(egui::Button::new(widgets::icon(ic::CLOSE, 10.0, TEXT_DIM)).frame(false)).clicked() {
                                acts.push(DAct::RemoveRoot(i));
                            }
                        });
                    });
                }
                if ui.add_enabled(!scanning, widgets::ghost(widgets::icon_text(ic::FOLDER, "Add folder", 14.0, TEXT))).clicked() {
                    acts.push(DAct::AddFolder);
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::dim("Ignore files smaller than"));
                for (b, label) in [(100u64 << 10, "100 KB"), (1 << 20, "1 MB"), (10 << 20, "10 MB"), (100 << 20, "100 MB")] {
                    if ui.add_enabled(!scanning, widgets::chip(label, d.min_size == b)).clicked() {
                        acts.push(DAct::MinSize(b));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if scanning {
                        if ui.add(widgets::ghost("Stop")).clicked() {
                            acts.push(DAct::Cancel);
                        }
                    } else if ui
                        .add_enabled(!d.roots.is_empty(), widgets::primary(widgets::icon_text(ic::SEARCH, "Find duplicates", 14.0, Color32::WHITE)))
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
                        ui.label(widgets::dim(format!("{} of {} files", fmt_num(done), fmt_num(total))));
                    } else {
                        ui.label(widgets::dim(format!("{} files found so far", fmt_num(done))));
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

        let live_groups: Vec<usize> = (0..groups.len()).filter(|&i| self.dup_group_live(&groups[i]) > 1).collect();
        let wasted: u64 = live_groups.iter().map(|&i| groups[i].wasted()).sum();
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
            ui.label(RichText::new(fmt_bytes(wasted)).font(widgets::light(30.0)).color(TEXT));
            ui.label(widgets::dim(format!("could be freed across {} sets of duplicates", fmt_num(live_groups.len() as u64))));
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
            .id_source("dups_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for &gi in live_groups.iter().take(400) {
                    let g = &groups[gi];
                    widgets::tight_frame().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(widgets::strong(format!("{} copies", self.dup_group_live(g)), 14.5));
                            ui.label(widgets::dim(format!("{} each, {} reclaimable", fmt_bytes(g.size), fmt_bytes(g.wasted()))));
                        });
                        for f in &g.files {
                            if d.removed.contains(&f.path) {
                                continue;
                            }
                            widgets::row(ui, 28.0, |ui| {
                                let mut sel = d.selected.contains(&f.path);
                                let cb = ui.add_enabled(f.block.is_none(), egui::Checkbox::new(&mut sel, ""));
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
                                    ui.add_sized([110.0, 20.0], Label::new(widgets::faint(fmt_date(f.modified))));
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        let col = if sel { TEXT } else { TEXT_DIM };
                                        ui.add(Label::new(RichText::new(f.path.display().to_string()).color(col)).truncate());
                                    });
                                });
                            });
                        }
                    });
                    ui.add_space(2.0);
                }
                if live_groups.len() > 400 {
                    ui.label(widgets::faint("Showing the 400 sets that waste the most space."));
                }
                ui.add_space(20.0);
            });
    }
}
