//! A structured, local record of every file PCSweep cleans up.
//!
//! Each cleanup is a session written as JSON Lines. What PCSweep intends to remove
//! is saved and flushed before anything is touched, and outcomes are added as work
//! completes. If the journal can't be written, nothing is deleted. An intention
//! without an outcome (for example after a crash or power loss) is shown as
//! uncertain rather than assumed to have succeeded.

use crate::app::{App, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Each journal file is capped; the previous file is kept as one older generation.
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const FILE_NAME: &str = "cleanup-journal.jsonl";
const OLD_FILE_NAME: &str = "cleanup-journal.1.jsonl";
/// The most entries loaded into the history view, newest first.
const MAX_ENTRIES: usize = 250_000;
const RECOVERY_NOTE: &str = "Items moved to the Recycle Bin can be restored from it (right-click, Restore) until it's emptied. Permanently deleted cache files can't be restored; apps rebuild them. A restore point protects system settings, not personal files.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Act {
    Delete,
    Recycle,
    RemoveFolder,
    EmptyRecycleBin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Deleted,
    Recycled,
    Removed,
    Skipped,
    Failed,
}

fn outcome_label(outcome: Option<Outcome>, action: Act) -> &'static str {
    match (outcome, action) {
        (Some(Outcome::Deleted), Act::EmptyRecycleBin) => "Recycle Bin emptied",
        (Some(Outcome::Deleted), _) => "Deleted permanently",
        (Some(Outcome::Recycled), _) => "Moved to Recycle Bin",
        (Some(Outcome::Removed), _) => "Removed empty folder",
        (Some(Outcome::Skipped), _) => "Not deleted",
        (Some(Outcome::Failed), _) => "Failed",
        (None, _) => "Uncertain",
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Session {
        session: String,
        time: String,
        source: String,
    },
    Intent {
        session: String,
        id: u64,
        path: String,
        size: u64,
        category: String,
        action: Act,
    },
    Done {
        session: String,
        id: u64,
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Batch {
        session: String,
        action: Act,
        items: u64,
        bytes: u64,
        outcome: Outcome,
    },
}

#[cfg(not(test))]
pub fn journal_dir() -> PathBuf {
    util::log_path().with_file_name("journal")
}
#[cfg(test)]
pub fn journal_dir() -> PathBuf {
    // Native mutation tests must never append disposable operations to the
    // developer's real cleanup history.
    std::env::temp_dir().join(format!("pcsweep-test-journal-{}", std::process::id()))
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// One cleanup run. Records are appended to the journal as work progresses.
pub struct Session {
    id: String,
    file: PathBuf,
    writer: Option<fs::File>,
    // An exclusive writer lease serializes cleanup across processes and keeps
    // rotation from separating an active operation from its recovery records.
    _lease: fs::File,
    _directories: Vec<fs::File>,
    source: String,
    time: String,
    next: u64,
}

fn guarded_directories(dir: &Path) -> Result<Vec<fs::File>, String> {
    if !dir.is_absolute() {
        return Err("The cleanup journal requires an absolute folder.".into());
    }
    let mut held = Vec::new();
    let mut ancestors: Vec<_> = dir.ancestors().collect();
    ancestors.reverse();
    for path in ancestors {
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("The journal folder could not be checked: {e}")),
        }
        // Open the directory itself, never its reparse target, and withhold
        // FILE_SHARE_DELETE so ancestors cannot be swapped while writing.
        let file = OpenOptions::new()
            .access_mode(0x80)
            .share_mode(3)
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .open(path)
            .map_err(|e| format!("The journal folder could not be protected: {e}"))?;
        let md = file.metadata().map_err(|e| e.to_string())?;
        if !md.is_dir() || util::is_reparse(&md) {
            return Err("Cleanup history cannot be written through links or junctions.".into());
        }
        held.push(file);
    }
    Ok(held)
}
fn single_link(file: &fs::File) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as isize, &mut info) } == 0
        || info.nNumberOfLinks != 1
    {
        return Err("The cleanup journal must not use hard links.".into());
    }
    Ok(())
}
fn open_journal(file: &Path) -> Result<fs::File, String> {
    let writer = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .open(file)
        .map_err(|e| format!("The cleanup journal could not be opened: {e}"))?;
    let md = writer.metadata().map_err(|e| e.to_string())?;
    if !md.is_file() || util::is_reparse(&md) {
        return Err("The cleanup journal must be a regular file, not a link.".into());
    }
    single_link(&writer)?;
    Ok(writer)
}
impl Session {
    pub fn begin(source: &str) -> Result<Self, String> {
        Self::begin_in(&journal_dir(), source)
    }
    pub fn begin_in(dir: &Path, source: &str) -> Result<Self, String> {
        let mut directories = guarded_directories(dir)?;
        fs::create_dir_all(dir)
            .map_err(|e| format!("The cleanup journal folder could not be created: {e}"))?;
        directories.extend(guarded_directories(dir)?);
        let lease = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .share_mode(0)
            .custom_flags(0x0020_0000)
            .open(dir.join("writer.lock"))
            .map_err(|e| format!("Another cleanup is running or history is unavailable: {e}"))?;
        if util::is_reparse(&lease.metadata().map_err(|e| e.to_string())?) {
            return Err("The journal lock cannot be a link.".into());
        }
        single_link(&lease)?;
        let file = dir.join(FILE_NAME);
        let writer = open_journal(&file)?;
        let now = chrono::Local::now();
        let counter = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        let id = format!(
            "{}-{}-{counter}",
            now.format("%Y%m%d-%H%M%S%.9f"),
            std::process::id()
        );
        let mut session = Self {
            id,
            file,
            writer: Some(writer),
            _lease: lease,
            _directories: directories,
            source: source.into(),
            time: now.to_rfc3339(),
            next: 1,
        };
        session.append(&[])?;
        Ok(session)
    }
    fn header(&self) -> Record {
        Record::Session {
            session: self.id.clone(),
            time: self.time.clone(),
            source: self.source.clone(),
        }
    }
    fn append(&mut self, records: &[Record]) -> Result<(), String> {
        let mut text = String::new();
        for record in records {
            text.push_str(&serde_json::to_string(record).map_err(|e| e.to_string())?);
            text.push('\n');
        }
        if text.len() as u64 > MAX_FILE_BYTES / 2 {
            return Err(
                "This history batch is too large; cleanup stopped before further removals.".into(),
            );
        }
        let len = self
            .writer
            .as_ref()
            .ok_or("The history writer is unavailable; cleanup stopped.")?
            .metadata()
            .map_err(|e| e.to_string())?
            .len();
        if len + text.len() as u64 + 1024 > MAX_FILE_BYTES {
            self.writer.take();
            let old = self.file.with_file_name(OLD_FILE_NAME);
            match fs::remove_file(&old) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("History rotation failed; cleanup stopped: {e}")),
            }
            fs::rename(&self.file, &old)
                .map_err(|e| format!("History rotation failed; cleanup stopped: {e}"))?;
            self.writer = Some(open_journal(&self.file)?);
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or("The history writer is unavailable; cleanup stopped.")?;
        if writer.metadata().map_err(|e| e.to_string())?.len() == 0 || records.is_empty() {
            let mut header = serde_json::to_string(&self.header()).map_err(|e| e.to_string())?;
            header.push('\n');
            header.push_str(&text);
            text = header;
        }
        self.writer
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .and_then(|_| self.writer.as_mut().unwrap().sync_data())
            .map_err(|e| format!("The cleanup journal could not be persisted: {e}"))
    }
    pub fn intend(
        &mut self,
        category: &str,
        action: Act,
        items: &[(PathBuf, u64)],
    ) -> Result<Vec<u64>, String> {
        let first = self.next;
        let records: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, (path, size))| Record::Intent {
                session: self.id.clone(),
                id: first + i as u64,
                path: path.display().to_string(),
                size: *size,
                category: category.into(),
                action,
            })
            .collect();
        self.append(&records)?;
        self.next += items.len() as u64;
        Ok((first..self.next).collect())
    }
    /// An outcome failure is surfaced to the caller: do not remove more items.
    /// Existing intentions remain uncertain, never assumed successful.
    pub fn finish(&mut self, outcomes: &[(u64, Outcome, Option<String>)]) -> Result<(), String> {
        let records: Vec<_> = outcomes
            .iter()
            .map(|(id, outcome, reason)| Record::Done {
                session: self.id.clone(),
                id: *id,
                outcome: *outcome,
                reason: reason.clone(),
            })
            .collect();
        self.append(&records)
    }
}

// ---------------------------------------------------------------- reading

#[derive(Clone, Serialize)]
pub struct Entry {
    pub time: String,
    pub source: String,
    pub session: String,
    pub category: String,
    pub action: Act,
    pub outcome: Option<Outcome>,
    pub reason: Option<String>,
    pub size: u64,
    pub path: String,
    #[serde(skip)]
    lower: String,
}

#[derive(Default)]
pub struct History {
    /// Newest first.
    pub entries: Vec<Entry>,
    /// When the oldest recorded session started.
    pub since: Option<String>,
    /// Lines that couldn't be read, such as a write cut off by a power loss.
    pub unreadable: u64,
    pub truncated: bool,
}

pub fn load() -> History {
    load_from(&journal_dir())
}

pub fn load_from(dir: &Path) -> History {
    let mut history = History::default();
    let _directories = match guarded_directories(dir) {
        Ok(held) => held,
        Err(_) => {
            history.unreadable = 1;
            return history;
        }
    };
    let mut sessions: HashMap<String, (String, String)> = HashMap::new();
    let mut session_order = VecDeque::new();
    let mut index: HashMap<(String, u64), usize> = HashMap::new();
    let mut entries: VecDeque<(Option<(String, u64)>, Entry)> = VecDeque::new();
    let mut offset = 0usize;
    for name in [OLD_FILE_NAME, FILE_NAME] {
        let path = dir.join(name);
        let mut file = match OpenOptions::new()
            .read(true)
            .share_mode(7)
            .custom_flags(0x0020_0000)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                history.unreadable += 1;
                continue;
            }
        };
        let md = match file.metadata() {
            Ok(md) => md,
            Err(_) => {
                history.unreadable += 1;
                continue;
            }
        };
        if !md.is_file() || util::is_reparse(&md) || single_link(&file).is_err() {
            history.unreadable += 1;
            continue;
        }
        let length = md.len();
        if length > MAX_FILE_BYTES {
            history.truncated = true;
            if file.seek(SeekFrom::Start(length - MAX_FILE_BYTES)).is_err() {
                history.unreadable += 1;
                continue;
            }
        }
        let reader = BufReader::new(file.take(MAX_FILE_BYTES));
        for (line_number, line) in reader.split(b'\n').enumerate() {
            if length > MAX_FILE_BYTES && line_number == 0 {
                continue;
            }
            let line = match line {
                Ok(line) => line,
                Err(_) => {
                    history.unreadable += 1;
                    break;
                }
            };
            if line.is_empty() {
                continue;
            }
            if line.len() > 1024 * 1024 {
                history.unreadable += 1;
                continue;
            }
            let mut added = None;
            match serde_json::from_slice::<Record>(&line) {
                Ok(Record::Session {
                    session,
                    time,
                    source,
                }) => {
                    history.since.get_or_insert_with(|| time.clone());
                    if !sessions.contains_key(&session) {
                        session_order.push_back(session.clone());
                    }
                    sessions.insert(session, (time, source));
                    while sessions.len() > MAX_ENTRIES {
                        if let Some(old) = session_order.pop_front() {
                            sessions.remove(&old);
                        }
                    }
                }
                Ok(Record::Intent {
                    session,
                    id,
                    path,
                    size,
                    category,
                    action,
                }) => {
                    let key = (session.clone(), id);
                    if index.contains_key(&key) {
                        history.unreadable += 1;
                        continue;
                    }
                    let (time, source) = sessions.get(&session).cloned().unwrap_or_default();
                    index.insert(key.clone(), offset + entries.len());
                    added = Some((
                        Some(key),
                        Entry {
                            lower: format!("{path} {category} {source}").to_lowercase(),
                            time,
                            source,
                            session,
                            category,
                            action,
                            outcome: None,
                            reason: None,
                            size,
                            path,
                        },
                    ));
                }
                Ok(Record::Done {
                    session,
                    id,
                    outcome,
                    reason,
                }) => {
                    if let Some(position) = index.get(&(session, id)).copied() {
                        if let Some((_, entry)) = position
                            .checked_sub(offset)
                            .and_then(|i| entries.get_mut(i))
                        {
                            entry.outcome = Some(outcome);
                            entry.reason = reason;
                        }
                    }
                }
                Ok(Record::Batch {
                    session,
                    action,
                    items,
                    bytes,
                    outcome,
                }) => {
                    let (time, source) = sessions.get(&session).cloned().unwrap_or_default();
                    added = Some((
                        None,
                        Entry {
                            lower: format!("recycle bin {source}").to_lowercase(),
                            time,
                            source,
                            session,
                            category: "Recycle Bin".into(),
                            action,
                            outcome: Some(outcome),
                            reason: None,
                            size: bytes,
                            path: format!("{} items (batch)", fmt_num(items)),
                        },
                    ));
                }
                Err(_) => history.unreadable += 1,
            }
            if let Some(entry) = added {
                entries.push_back(entry);
            }
            if entries.len() > MAX_ENTRIES {
                if let Some((Some(key), _)) = entries.pop_front() {
                    index.remove(&key);
                }
                offset += 1;
                history.truncated = true;
            }
        }
    }
    history.entries = entries.into_iter().rev().map(|(_, entry)| entry).collect();
    // Retention is bounded: report the beginning of retained evidence, rather
    // than suggesting an overwritten generation is still searchable.
    history.since = history
        .entries
        .last()
        .map(|e| e.time.clone())
        .filter(|s| !s.is_empty());
    history
}

/// A CSV cell, quoted when needed. Cells that a spreadsheet would treat as a
/// formula are prefixed with an apostrophe so opening the export is safe.
fn csv_cell(value: &str) -> String {
    let value = if value
        .trim_start_matches(|c: char| c.is_whitespace() || c.is_control())
        .starts_with(['=', '+', '-', '@'])
        || value.starts_with(['\t', '\r', '\n'])
    {
        format!("'{value}")
    } else {
        value.to_string()
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

fn export_csv(entries: &[Entry]) -> String {
    let mut out = String::from("time,source,category,result,reason,size_bytes,path\n");
    for e in entries {
        let cells = [
            csv_cell(&e.time),
            csv_cell(&e.source),
            csv_cell(&e.category),
            csv_cell(outcome_label(e.outcome, e.action)),
            csv_cell(e.reason.as_deref().unwrap_or("")),
            e.size.to_string(),
            csv_cell(&e.path),
        ];
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

fn write_export(path: PathBuf, entries: Vec<Entry>, json: bool) -> Result<PathBuf, String> {
    let text = if json {
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?
    } else {
        export_csv(&entries)
    };
    let parent = path.parent().ok_or("The export needs an absolute folder")?;
    let _held = guarded_directories(parent)
        .map_err(|e| format!("Choose a local export folder without links or junctions: {e}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .share_mode(1)
        .custom_flags(0x0020_0000)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let md = file.metadata().map_err(|e| e.to_string())?;
    if !md.is_file() || util::is_reparse(&md) {
        return Err("Exports cannot overwrite links or non-files.".into());
    }
    single_link(&file)?;
    file.set_len(0)
        .and_then(|_| file.write_all(text.as_bytes()))
        .and_then(|_| file.sync_data())
        .map_err(|e| format!("The export could not be saved: {e}"))?;
    Ok(path)
}

// ---------------------------------------------------------------- view

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Shown {
    #[default]
    All,
    Removed,
    Kept,
    Uncertain,
}

impl Shown {
    fn matches(self, outcome: Option<Outcome>) -> bool {
        match self {
            Shown::All => true,
            Shown::Removed => matches!(
                outcome,
                Some(Outcome::Deleted | Outcome::Recycled | Outcome::Removed)
            ),
            Shown::Kept => matches!(outcome, Some(Outcome::Skipped | Outcome::Failed)),
            Shown::Uncertain => outcome.is_none(),
        }
    }
}

#[derive(Default)]
pub struct HistoryState {
    /// The Activity page shows the cleanup history instead of the text log.
    pub open: bool,
    load: Task<History>,
    export: Task<Result<PathBuf, String>>,
    history: Option<Arc<History>>,
    generation: u64,
    query: String,
    shown: Shown,
    latest_only: bool,
    view: Vec<usize>,
    key: Option<(String, Shown, bool, u64)>,
}

impl HistoryState {
    pub fn refresh(&mut self, ctx: &egui::Context) {
        if !self.load.running() {
            self.load = Task::spawn(ctx, load);
        }
    }

    /// Rebuilds the filtered list only when the search or the data changed.
    fn prepare(&mut self) {
        let Some(history) = self.history.as_ref() else {
            return;
        };
        let key = (
            self.query.trim().to_lowercase(),
            self.shown,
            self.latest_only,
            self.generation,
        );
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let latest = history.entries.first().map(|e| e.session.as_str());
        let needle = key.0.as_str();
        self.view = history
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !self.latest_only || Some(e.session.as_str()) == latest)
            .filter(|(_, e)| self.shown.matches(e.outcome))
            .filter(|(_, e)| needle.is_empty() || e.lower.contains(needle))
            .map(|(i, _)| i)
            .collect();
        self.key = Some(key);
    }
}

enum HAct {
    Back,
    Refresh,
    Query(String),
    Shown(Shown),
    LatestOnly(bool),
    Export(bool),
    OpenFolder,
    Show(PathBuf),
    Copy(String),
}

impl App {
    pub fn poll_history(&mut self) {
        if let Some(result) = self.history.load.poll() {
            match result {
                Ok(history) => {
                    let old = self.history.history.replace(Arc::new(history));
                    // Large retained histories contain many paths; releasing
                    // them must not stall the frame that accepts a refresh.
                    if let Some(old) = old {
                        let _ = std::thread::Builder::new()
                            .name("history-dispose".into())
                            .spawn(move || drop(old));
                    }
                    self.history.generation += 1;
                }
                Err(error) => {
                    // Show an empty history instead of retrying on every frame.
                    self.history.history = Some(Arc::new(History::default()));
                    self.history.generation += 1;
                    self.toast(
                        ToastKind::Error,
                        format!("History couldn't be read: {error}"),
                    );
                }
            }
        }
        if let Some(result) = self.history.export.poll() {
            match result.unwrap_or_else(Err) {
                Ok(path) => self.toast(ToastKind::Success, format!("Saved {}", path.display())),
                Err(error) => self.toast(ToastKind::Error, error),
            }
        }
    }

    pub fn page_history(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self.history.history.is_none() {
            self.history.refresh(&ctx);
        }
        self.history.prepare();
        let mut acts = Vec::new();
        self.render_history(ui, &mut acts);
        for act in acts {
            match act {
                HAct::Back => self.history.open = false,
                HAct::Refresh => self.history.refresh(&ctx),
                HAct::Query(value) => self.history.query = value,
                HAct::Shown(value) => self.history.shown = value,
                HAct::LatestOnly(value) => self.history.latest_only = value,
                HAct::OpenFolder => {
                    let dir = journal_dir();
                    let _ = fs::create_dir_all(&dir);
                    util::open_path(&dir);
                }
                HAct::Show(path) => util::show_in_explorer(&path),
                HAct::Copy(text) => ctx.output_mut(|o| o.copied_text = text),
                HAct::Export(json) => {
                    if self.history.export.running() {
                        continue;
                    }
                    let (name, label, extension) = if json {
                        ("pcsweep-cleanup-history.json", "JSON", "json")
                    } else {
                        ("pcsweep-cleanup-history.csv", "CSV", "csv")
                    };
                    let Some(path) = rfd::FileDialog::new()
                        .set_title("Export cleanup history")
                        .set_file_name(name)
                        .add_filter(label, &[extension])
                        .save_file()
                    else {
                        continue;
                    };
                    let history = self.history.history.clone();
                    let view = self.history.view.clone();
                    self.history.export = Task::spawn(&ctx, move || {
                        let rows: Vec<_> = history
                            .as_ref()
                            .map(|h| view.iter().map(|&i| h.entries[i].clone()).collect())
                            .unwrap_or_default();
                        write_export(path, rows, json)
                    });
                }
            }
        }
    }

    fn render_history(&self, ui: &mut Ui, acts: &mut Vec<HAct>) {
        let h = &self.history;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(widgets::ghost(widgets::icon_text(
                    ic::BACK,
                    "Activity log",
                    14.0,
                    color_ink(),
                )))
                .clicked()
            {
                acts.push(HAct::Back);
            }
            ui.label(widgets::strong("Cleanup history", 22.0));
            if h.load.running() {
                ui.spinner();
            }
        });
        let Some(history) = h.history.as_ref() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(widgets::dim("Reading the cleanup journal…")));
            return;
        };
        widgets::banner(ui, color_accent(), ic::INFO, |ui| {
            ui.vertical(|ui| {
                let since = history.since.as_deref().map(|s|format!(" from {s}")).unwrap_or_default();
                ui.label(format!("Retained PCSweep cleanup history{since}. Older records can expire; earlier releases only recorded partial text activity."));
                ui.label(widgets::faint(RECOVERY_NOTE));
            });
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let mut query = h.query.clone();
            if widgets::search_box(ui, &mut query, "Search name, folder or category", 260.0)
                .changed()
            {
                acts.push(HAct::Query(query));
            }
            for (shown, label) in [
                (Shown::All, "All"),
                (Shown::Removed, "Removed"),
                (Shown::Kept, "Not removed"),
                (Shown::Uncertain, "Uncertain"),
            ] {
                if ui.add(widgets::chip(label, h.shown == shown)).clicked() {
                    acts.push(HAct::Shown(shown));
                }
            }
            let mut latest = h.latest_only;
            if ui.checkbox(&mut latest, "Latest cleanup only").changed() {
                acts.push(HAct::LatestOnly(latest));
            }
        });
        ui.horizontal_wrapped(|ui| {
            let busy = h.export.running();
            if ui
                .add_enabled(!busy && !h.view.is_empty(), widgets::small("Export CSV"))
                .clicked()
            {
                acts.push(HAct::Export(false));
            }
            if ui
                .add_enabled(!busy && !h.view.is_empty(), widgets::small("Export JSON"))
                .clicked()
            {
                acts.push(HAct::Export(true));
            }
            if ui.add(widgets::small("Open journal folder")).clicked() {
                acts.push(HAct::OpenFolder);
            }
            let refresh = widgets::icon_text(ic::REFRESH, "Refresh", 13.0, color_ink());
            if ui
                .add_enabled(!h.load.running(), widgets::small(refresh))
                .clicked()
            {
                acts.push(HAct::Refresh);
            }
            let mut note = format!(
                "{} of {} entries",
                fmt_num(h.view.len() as u64),
                fmt_num(history.entries.len() as u64)
            );
            if history.unreadable > 0 {
                note.push_str(&format!(
                    ", {} unreadable records or files",
                    fmt_num(history.unreadable)
                ));
            }
            if history.truncated {
                note.push_str(", showing the most recent entries");
            }
            ui.label(widgets::faint(note));
        });
        ui.add_space(2.0);
        if h.view.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                let text = if history.entries.is_empty() && history.unreadable > 0 {
                    "History could not be fully read. Check the journal folder and try Refresh."
                } else if history.entries.is_empty() {
                    "Nothing has been cleaned since history recording started."
                } else {
                    "No entries match."
                };
                ui.label(widgets::dim(text));
            });
            return;
        }
        ScrollArea::vertical()
            .id_salt("history_list")
            .auto_shrink([false, false])
            .show_rows(ui, 44.0, h.view.len(), |ui, range| {
                for &i in &h.view[range] {
                    render_entry(ui, &history.entries[i], acts);
                }
            });
    }
}

fn render_entry(ui: &mut Ui, entry: &Entry, acts: &mut Vec<HAct>) {
    let (color, icon_char) = match entry.outcome {
        Some(Outcome::Deleted | Outcome::Removed) => (color_muted(), ic::CLEAN),
        Some(Outcome::Recycled) => (color_success(), ic::REFRESH),
        Some(Outcome::Skipped) => (color_secondary(), ic::LOCK),
        Some(Outcome::Failed) => (color_error(), ic::WARN),
        None => (color_warning(), ic::WARN),
    };
    let label = outcome_label(entry.outcome, entry.action);
    widgets::row(ui, 44.0, |ui| {
        ui.label(widgets::icon(icon_char, 15.0, color));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(6.0);
            widgets::cell(ui, 140.0, true, widgets::faint(&entry.time));
            widgets::cell(ui, 84.0, true, widgets::strong(fmt_bytes(entry.size), 14.0));
            let result = RichText::new(label).color(color);
            let hint = entry.reason.clone().unwrap_or_else(|| label.to_string());
            widgets::cell(ui, 150.0, true, result).on_hover_text(hint);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let path = Path::new(&entry.path);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| entry.path.clone());
                let detail = format!("{}, {}", entry.category, entry.source);
                let response = ui
                    .vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.add(Label::new(RichText::new(name).color(color_ink())).truncate());
                        ui.add(Label::new(widgets::faint(detail)).truncate());
                    })
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_text(&entry.path);
                response.context_menu(|ui| {
                    if ui.button("Copy full path").clicked() {
                        acts.push(HAct::Copy(entry.path.clone()));
                        ui.close_menu();
                    }
                    if entry.action != Act::EmptyRecycleBin && ui.button("Show location").clicked()
                    {
                        acts.push(HAct::Show(PathBuf::from(&entry.path)));
                        ui.close_menu();
                    }
                });
            });
        });
    });
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> HistoryState {
    let entry = |path: &str, outcome: Option<Outcome>, reason: Option<&str>| Entry {
        time: "2026-11-15 19:30:12".into(),
        source: "Junk cleaner".into(),
        session: "preview".into(),
        category: "Temporary files".into(),
        action: Act::Delete,
        outcome,
        reason: reason.map(str::to_string),
        size: 4096,
        path: path.into(),
        lower: path.to_lowercase(),
    };
    HistoryState {
        open: true,
        history: Some(Arc::new(History {
            entries: vec![
                entry(r"C:\Temp\setup.log", Some(Outcome::Deleted), None),
                entry(r"C:\Temp\locked.tmp", Some(Outcome::Skipped), Some("Busy")),
                entry(r"C:\Temp\pending.tmp", None, None),
            ],
            since: Some("2026-11-15 19:30:12".into()),
            unreadable: 0,
            truncated: false,
        })),
        generation: 1,
        ..HistoryState::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pcsweep-journal-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn intentions_are_saved_first_and_outcomes_attach_to_them() {
        let dir = temp_dir("outcomes");
        let mut session = Session::begin_in(&dir, "Junk cleaner").unwrap();
        let items = vec![
            (PathBuf::from(r"C:\Temp\a.tmp"), 10),
            (PathBuf::from(r"C:\Temp\b.tmp"), 20),
            (PathBuf::from(r"C:\Temp\c.tmp"), 30),
        ];
        let ids = session
            .intend("Temporary files", Act::Delete, &items)
            .unwrap();
        assert_eq!(ids, vec![1, 2, 3]);
        // Before any outcome is written, every intention is uncertain.
        let before = load_from(&dir);
        assert_eq!(before.entries.len(), 3);
        assert!(before.entries.iter().all(|e| e.outcome.is_none()));
        session
            .finish(&[
                (1, Outcome::Deleted, None),
                (2, Outcome::Skipped, Some("In use".into())),
            ])
            .unwrap();
        let after = load_from(&dir);
        let find = |name: &str| after.entries.iter().find(|e| e.path.ends_with(name));
        assert_eq!(find("a.tmp").unwrap().outcome, Some(Outcome::Deleted));
        assert_eq!(find("b.tmp").unwrap().reason.as_deref(), Some("In use"));
        assert_eq!(
            find("c.tmp").unwrap().outcome,
            None,
            "no outcome means uncertain"
        );
        assert_eq!(find("a.tmp").unwrap().source, "Junk cleaner");
        assert!(after.since.is_some());
        drop(session);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cut_off_lines_are_counted_and_batches_are_not_split_into_items() {
        let dir = temp_dir("batch");
        let mut session = Session::begin_in(&dir, "Junk cleaner").unwrap();
        let ids = session
            .intend(
                "Recycle Bin",
                Act::EmptyRecycleBin,
                &[(PathBuf::from("12 items (batch)"), 4096)],
            )
            .unwrap();
        session.finish(&[(ids[0], Outcome::Deleted, None)]).unwrap();
        drop(session);
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join(FILE_NAME))
            .unwrap();
        file.write_all(b"{\"kind\":\"intent\",\"session\":\"x\"")
            .unwrap();
        let history = load_from(&dir);
        assert_eq!(history.unreadable, 1);
        assert_eq!(history.entries.len(), 1);
        let entry = &history.entries[0];
        assert_eq!(entry.size, 4096);
        assert_eq!(
            outcome_label(entry.outcome, entry.action),
            "Recycle Bin emptied"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn newest_sessions_come_first() {
        let dir = temp_dir("order");
        for name in ["first.tmp", "second.tmp"] {
            let mut session = Session::begin_in(&dir, "Junk cleaner").unwrap();
            let ids = session
                .intend("Temp", Act::Delete, &[(PathBuf::from(name), 1)])
                .unwrap();
            session.finish(&[(ids[0], Outcome::Deleted, None)]).unwrap();
        }
        let history = load_from(&dir);
        assert!(history.entries[0].path.ends_with("second.tmp"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn csv_export_quotes_and_neutralizes_formulas() {
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("=cmd"), "'=cmd");
        assert_eq!(csv_cell("-1"), "'-1");
        assert_eq!(csv_cell(" \t=cmd"), "' \t=cmd");
        assert_eq!(csv_cell("\tplain"), "'\tplain");
    }
    #[test]
    fn active_session_excludes_another_writer_and_can_be_read() {
        let dir = temp_dir("exclusive");
        let mut first = Session::begin_in(&dir, "First cleanup").unwrap();
        first
            .intend("Temp", Act::Delete, &[(PathBuf::from("first.tmp"), 4)])
            .unwrap();
        assert!(Session::begin_in(&dir, "Second cleanup").is_err());
        assert_eq!(load_from(&dir).entries.len(), 1);
        drop(first);
        let second = Session::begin_in(&dir, "Second cleanup").unwrap();
        drop(second);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn outcome_persistence_failure_preserves_uncertain_intention() {
        let dir = temp_dir("failed-outcome");
        let mut session = Session::begin_in(&dir, "Test").unwrap();
        let ids = session
            .intend("Temp", Act::Delete, &[(PathBuf::from("reviewed.tmp"), 4)])
            .unwrap();
        // A read-only replacement simulates the OS refusing a post-operation
        // journal write without touching any real cleanup target.
        session.writer.take();
        session.writer = Some(fs::File::open(&session.file).unwrap());
        assert!(session.finish(&[(ids[0], Outcome::Deleted, None)]).is_err());
        assert!(session
            .intend("Temp", Act::Delete, &[(PathBuf::from("next.tmp"), 4)])
            .is_err());
        let history = load_from(&dir);
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].outcome, None);
        drop(session);
        fs::remove_dir_all(dir).unwrap();
    }
    fn grow_fixture_journal(session: &mut Session) {
        // The production append-only handle deliberately lacks SetEndOfFile
        // permission. Grow this disposable fixture through a temporary handle.
        session.writer.take();
        let file = OpenOptions::new().write(true).open(&session.file).unwrap();
        file.set_len(MAX_FILE_BYTES).unwrap();
        drop(file);
        session.writer = Some(open_journal(&session.file).unwrap());
    }
    #[test]
    fn rotation_replaces_the_older_generation_without_growing_forever() {
        let dir = temp_dir("rotation");
        let mut session = Session::begin_in(&dir, "Test").unwrap();
        for name in ["first.tmp", "second.tmp"] {
            grow_fixture_journal(&mut session);
            let ids = session
                .intend("Temp", Act::Delete, &[(PathBuf::from(name), 4)])
                .unwrap();
            session.finish(&[(ids[0], Outcome::Deleted, None)]).unwrap();
            assert!(session.writer.as_ref().unwrap().metadata().unwrap().len() < MAX_FILE_BYTES);
            assert_eq!(
                fs::metadata(dir.join(OLD_FILE_NAME)).unwrap().len(),
                MAX_FILE_BYTES
            );
        }
        assert!(load_from(&dir)
            .entries
            .iter()
            .any(|e| e.path == "second.tmp" && e.outcome == Some(Outcome::Deleted)));
        drop(session);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn failed_rotation_blocks_new_intentions() {
        let dir = temp_dir("rotation-blocked");
        let mut session = Session::begin_in(&dir, "Test").unwrap();
        fs::create_dir(dir.join(OLD_FILE_NAME)).unwrap();
        grow_fixture_journal(&mut session);
        assert!(session
            .intend("Temp", Act::Delete, &[(PathBuf::from("blocked.tmp"), 4)])
            .is_err());
        assert!(session.writer.is_none());
        drop(session);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn journal_and_export_refuse_hardlinks_without_changing_the_target() {
        let dir = temp_dir("hardlinks");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("keep.txt");
        fs::write(&target, b"keep this content").unwrap();
        fs::hard_link(&target, dir.join(FILE_NAME)).unwrap();
        assert!(Session::begin_in(&dir, "Test").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep this content");
        assert!(write_export(dir.join(FILE_NAME), Vec::new(), false).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep this content");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn journal_refuses_a_linked_ancestor_when_link_creation_is_available() {
        let dir = temp_dir("linked-parent");
        fs::create_dir_all(dir.join("real")).unwrap();
        let linked = dir.join("linked");
        if let Err(error) = std::os::windows::fs::symlink_dir(dir.join("real"), &linked) {
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(1314)
            {
                fs::remove_dir_all(dir).unwrap();
                return;
            }
            panic!("Could not create disposable test link: {error}");
        }
        assert!(Session::begin_in(&linked.join("journal"), "Test").is_err());
        assert!(!dir.join("real").join("journal").exists());
        fs::remove_dir(&linked).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn filters_select_the_right_outcomes() {
        assert!(Shown::Removed.matches(Some(Outcome::Recycled)));
        assert!(!Shown::Removed.matches(Some(Outcome::Skipped)));
        assert!(Shown::Kept.matches(Some(Outcome::Failed)));
        assert!(Shown::Uncertain.matches(None));
        assert!(Shown::All.matches(None));
    }
}
