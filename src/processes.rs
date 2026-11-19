use crate::app::{Action, App, Modal, ToastKind};
use crate::app_icons::{IconCache, IconSource};
use crate::recovery::{self, RecoveryAction, Reopen};
use crate::running::{self, Access, Identity, Process, Row, RowId, Snapshot, Window};
use crate::system::{self, MemoryPressure, PowerPolicy};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CACHE_HINT: &str = "Windows reuses eligible standby cache as apps need RAM.";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Sort {
    #[default]
    Ram,
    Cpu,
    Private,
    Growth,
    Name,
    Pid,
}
impl Sort {
    fn label(self) -> &'static str {
        match self {
            Self::Ram => "RAM",
            Self::Cpu => "CPU",
            Self::Private => "Private commit",
            Self::Growth => "Growth",
            Self::Name => "Name",
            Self::Pid => "PID",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Scope {
    #[default]
    All,
    Windows,
    Background,
    Pid,
}
impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All apps",
            Self::Windows => "Open windows",
            Self::Background => "Background",
            Self::Pid => "By PID",
        }
    }
    fn individual(self) -> bool {
        matches!(self, Self::Pid | Self::Background)
    }
    fn includes(self, row: &Row) -> bool {
        match self {
            Self::All | Self::Pid => true,
            Self::Windows => row.window_count != 0,
            Self::Background => row.window_count == 0,
        }
    }
}

#[derive(Default)]
struct ProcessView {
    filter: String,
    scope: Scope,
    sort: Sort,
    rows: Option<VisibleRows>,
}

struct VisibleRows {
    sample: Option<Instant>,
    scope: Scope,
    sort: Sort,
    filter: String,
    indices: Vec<usize>,
}

impl ProcessView {
    fn rows<'a>(&'a mut self, snapshot: &Snapshot, table: &[Row], scope: Scope) -> &'a [usize] {
        if !self.rows.as_ref().is_some_and(|v| {
            v.sample == snapshot.sampled_at
                && v.scope == scope
                && v.sort == self.sort
                && v.filter == self.filter
        }) {
            let query = self.filter.trim().to_lowercase();
            let pid_query = query.parse::<u32>().ok();
            let mut indices: Vec<_> = table
                .iter()
                .enumerate()
                .filter(|(_, row)| scope.includes(row))
                .filter(|(_, row)| {
                    if let Some(pid) = pid_query {
                        row.members
                            .iter()
                            .any(|&i| snapshot.processes[i].identity.pid == pid)
                    } else {
                        query.is_empty() || row.search.contains(&query)
                    }
                })
                .map(|(i, _)| i)
                .collect();
            let pid = |row: &Row| snapshot.processes[row.members[0]].identity.pid;
            // The worker already orders both tables by resident memory.
            if self.sort != Sort::Ram {
                indices.sort_by(|&a, &b| {
                    let a = &table[a];
                    let b = &table[b];
                    let order = match self.sort {
                        Sort::Ram => std::cmp::Ordering::Equal,
                        Sort::Cpu => (b.cpu_known > 0)
                            .cmp(&(a.cpu_known > 0))
                            .then_with(|| b.cpu.total_cmp(&a.cpu)),
                        Sort::Private => b.private.cmp(&a.private),
                        Sort::Growth => b.growth.cmp(&a.growth),
                        Sort::Name => a
                            .name
                            .to_ascii_lowercase()
                            .cmp(&b.name.to_ascii_lowercase()),
                        Sort::Pid => pid(a).cmp(&pid(b)),
                    };
                    order.then_with(|| a.id.cmp(&b.id))
                });
            }
            self.rows = Some(VisibleRows {
                sample: snapshot.sampled_at,
                scope,
                sort: self.sort,
                filter: self.filter.clone(),
                indices,
            });
        }
        &self
            .rows
            .as_ref()
            .expect("visible rows were prepared")
            .indices
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Close,
    End,
    Efficiency,
    RestoreEfficiency,
    Trim,
}
impl Operation {
    fn label(self) -> &'static str {
        match self {
            Self::Close => "Close normally",
            Self::End => "End task",
            Self::Efficiency => "Efficiency mode",
            Self::RestoreEfficiency => "Restore efficiency settings",
            Self::Trim => "Trim resident RAM",
        }
    }
    fn closes(self) -> bool {
        matches!(self, Self::Close | Self::End)
    }
}

struct Target {
    id: Identity,
    name: String,
    windows: Vec<Window>,
    original: Option<PowerPolicy>,
    reopenable: bool,
}

pub struct MemoryAction {
    operation: Operation,
    targets: Vec<Target>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Changed,
    Gone,
    Waiting,
    Failed,
}

struct ResultRow {
    id: Identity,
    name: String,
    outcome: Outcome,
    detail: String,
}

pub struct MemoryReport {
    operation: Operation,
    rows: Vec<ResultRow>,
    before: u64,
    after: u64,
    policies: Vec<(Identity, Option<PowerPolicy>)>,
    reopen: Vec<(Identity, Reopen)>,
}
impl MemoryReport {
    fn summary(&self) -> String {
        let count = |state| self.rows.iter().filter(|row| row.outcome == state).count();
        let mut parts = Vec::new();
        let changed = count(Outcome::Changed);
        if changed > 0 {
            let result = match self.operation {
                Operation::End | Operation::Close => "exited",
                Operation::Efficiency => "set to efficiency mode",
                Operation::RestoreEfficiency => "restored",
                Operation::Trim => "trimmed",
            };
            parts.push(format!("{changed} {result}"));
        }
        for (outcome, label) in [
            (Outcome::Gone, "already exited"),
            (Outcome::Waiting, "still running"),
            (Outcome::Failed, "could not change"),
        ] {
            let n = count(outcome);
            if n > 0 {
                parts.push(format!("{n} {label}"));
            }
        }
        let mut summary = format!("{}: {}.", self.operation.label(), parts.join(", "));
        if self.operation == Operation::Trim && changed > 0 {
            let freed = self.before.saturating_sub(self.after);
            summary.push_str(&format!(
                " Resident RAM fell by {} at measurement.",
                fmt_bytes(freed)
            ));
        }
        summary
    }
}

pub struct MemState {
    view: ProcessView,
    background_view: ProcessView,
    selected: Option<RowId>,
    inspect_open: bool,
    efficiency: HashMap<Identity, PowerPolicy>,
    pressure_since: Option<(MemoryPressure, Instant)>,
    last_sample: Option<Instant>,
    pub job: Task<MemoryReport>,
    checked: HashSet<Identity>,
    details: bool,
    stop_waiting: Arc<AtomicBool>,
    running: Option<Operation>,
    report: Option<MemoryReport>,
    results_open: bool,
    icons: IconCache,
    recovering: Task<Result<String, String>>,
    recent: VecDeque<Reopen>,
    pending_reopen: HashMap<Identity, Reopen>,
    recent_open: bool,
}

impl Default for MemState {
    fn default() -> Self {
        Self {
            view: ProcessView::default(),
            background_view: ProcessView {
                scope: Scope::Background,
                ..ProcessView::default()
            },
            selected: None,
            inspect_open: false,
            efficiency: HashMap::new(),
            pressure_since: None,
            last_sample: None,
            job: Task::default(),
            checked: HashSet::new(),
            details: false,
            stop_waiting: Arc::default(),
            running: None,
            report: None,
            results_open: false,
            icons: IconCache::default(),
            recovering: Task::default(),
            recent: VecDeque::new(),
            pending_reopen: HashMap::new(),
            recent_open: false,
        }
    }
}

impl MemState {
    fn remember_closed(&mut self, app: Reopen) {
        self.recent
            .retain(|old| !crate::winfile::same_path(&old.path, &app.path));
        self.recent.push_front(app);
        self.recent.truncate(12);
    }

    pub fn busy(&self) -> bool {
        self.job.running() || self.recovering.running()
    }
}

enum MAct {
    Inspect(RowId),
    Ask(Operation, Selection),
    Check(Vec<Identity>, bool),
    Clear,
    StopWaiting,
    Refresh,
    PageFile,
    Details(bool),
    Show(Identity, Window),
    Location(PathBuf),
}

enum Selection {
    Checked,
    Row(RowId),
    Pid(Identity),
}

fn run_operation(action: MemoryAction, stop: &AtomicBool) -> MemoryReport {
    let operation = action.operation;
    let mut report = MemoryReport {
        operation,
        rows: Vec::with_capacity(action.targets.len()),
        before: 0,
        after: 0,
        policies: Vec::new(),
        reopen: Vec::new(),
    };
    let mut pending = Vec::new();
    for target in action.targets {
        let mut row = ResultRow {
            id: target.id,
            name: target.name,
            outcome: Outcome::Changed,
            detail: String::new(),
        };
        if operation.closes() {
            let reopen = if target.reopenable && !stop.load(Ordering::Relaxed) {
                recovery::remember(target.id, &row.name)
            } else {
                None
            };
            if row.name.eq_ignore_ascii_case("explorer.exe") && operation == Operation::End {
                recovery::before_end(target.id);
            }
            let request = if stop.load(Ordering::Relaxed) {
                Err("Request stopped before this process was reached.".into())
            } else if operation == Operation::Close {
                running::request_close(target.id, &target.windows)
            } else {
                running::request_end(target.id)
            };
            if request.is_ok() {
                if let Some(app) = reopen {
                    report.reopen.push((target.id, app));
                }
            }
            match request {
                Ok(Some(handle)) => {
                    pending.push((report.rows.len(), handle));
                    row.outcome = Outcome::Waiting;
                    row.detail = if operation == Operation::Close {
                        "Still running. Check the app for a save prompt or use End task.".into()
                    } else {
                        "Windows accepted the request and is still ending this process.".into()
                    };
                }
                Ok(None) => {
                    row.outcome = Outcome::Gone;
                    row.detail = "This process had already exited.".into();
                }
                Err(error) => {
                    row.outcome = Outcome::Failed;
                    row.detail = error;
                }
            }
        } else {
            let result = match operation {
                Operation::Efficiency => system::enable_app_efficiency(target.id).map(Some),
                Operation::RestoreEfficiency => target
                    .original
                    .ok_or_else(|| "No efficiency settings were saved for this process.".to_owned())
                    .and_then(|original| system::restore_app_efficiency(target.id, original))
                    .map(|_| None),
                Operation::Trim => system::trim_app_memory(target.id).map(|(before, after)| {
                    report.before = report.before.saturating_add(before);
                    report.after = report.after.saturating_add(after);
                    None
                }),
                Operation::Close | Operation::End => unreachable!("handled above"),
            };
            match result {
                Ok(policy) => report.policies.push((target.id, policy)),
                Err(error) => {
                    row.outcome = Outcome::Failed;
                    row.detail = error;
                }
            }
        }
        report.rows.push(row);
    }

    let wait = if operation == Operation::Close { 8 } else { 2 };
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        pending.retain(|(index, handle)| {
            if handle.exited() {
                let row = &mut report.rows[*index];
                row.outcome = Outcome::Changed;
                row.detail.clear();
                false
            } else {
                true
            }
        });
        if pending.is_empty() || stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    report
}

fn growth_text(value: i64, observed: u64) -> String {
    if observed < 3 {
        return "—".into();
    }
    let prefix = if value > 0 {
        "+"
    } else if value < 0 {
        "−"
    } else {
        ""
    };
    format!("{prefix}{}", fmt_bytes(value.unsigned_abs()))
}

fn metric(known: usize, total: usize, value: String) -> String {
    if known == 0 {
        "—".into()
    } else if known < total {
        format!("~{value}")
    } else {
        value
    }
}

fn members<'a>(
    snapshot: &'a Snapshot,
    row: &'a Row,
) -> impl Iterator<Item = &'a Process> + Clone + 'a {
    row.members.iter().map(|&i| &snapshot.processes[i])
}

fn find_row<'a>(snapshot: &'a Snapshot, id: &RowId) -> Option<&'a Row> {
    match id {
        RowId::Process(_) => snapshot.individuals.iter().find(|row| row.id == *id),
        RowId::Image(_, _) => snapshot.groups.iter().find(|row| row.id == *id),
    }
}

impl App {
    pub fn execute_memory(&mut self, action: MemoryAction, ctx: &egui::Context) {
        if self.mem.busy() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.mem.stop_waiting = stop.clone();
        self.mem.running = Some(action.operation);
        self.mem.job = Task::spawn(ctx, move || {
            let report = run_operation(action, &stop);
            util::log(&report.summary());
            for row in report.rows.iter().filter(|r| !r.detail.is_empty()).take(20) {
                util::log(format!(
                    "  {} (PID {}): {}",
                    row.name, row.id.pid, row.detail
                ));
            }
            report
        });
    }

    pub fn poll_memory(&mut self, ctx: &egui::Context) {
        self.mem.icons.poll(ctx);
        if let Some(result) = self.mem.recovering.poll() {
            match result.and_then(|result| result) {
                Ok(message) => {
                    util::log(&message);
                    self.toast(ToastKind::Info, message);
                    self.sys.refresh_processes_soon();
                }
                Err(error) => self.toast(ToastKind::Error, error),
            }
        }
        let pressure = system::memory_pressure(
            self.sys.mem_total,
            self.sys.mem_avail,
            self.sys.commit_total,
            self.sys.commit_limit,
        );
        if self.mem.pressure_since.as_ref().map(|(value, _)| *value) != Some(pressure) {
            self.mem.pressure_since = Some((pressure, Instant::now()));
        }
        let snapshot = &self.sys.processes;
        if self.mem.last_sample != snapshot.sampled_at {
            self.mem.last_sample = snapshot.sampled_at;
            if !self.mem.checked.is_empty()
                || !self.mem.efficiency.is_empty()
                || !self.mem.pending_reopen.is_empty()
            {
                let live: HashSet<_> = snapshot.processes.iter().map(|p| p.identity).collect();
                self.mem.checked.retain(|id| live.contains(id));
                self.mem.efficiency.retain(|id, _| live.contains(id));
                let exited: Vec<_> = self
                    .mem
                    .pending_reopen
                    .extract_if(|id, _| !live.contains(id))
                    .map(|(_, app)| app)
                    .collect();
                for app in exited {
                    self.mem.remember_closed(app);
                }
            }
        }
        let Some(result) = self.mem.job.poll() else {
            return;
        };
        self.mem.running = None;
        match result {
            Err(error) => self.toast(ToastKind::Error, error),
            Ok(mut report) => {
                for (id, app) in std::mem::take(&mut report.reopen) {
                    if report
                        .rows
                        .iter()
                        .any(|row| row.id == id && row.outcome == Outcome::Waiting)
                    {
                        if self.mem.pending_reopen.len() < 64 {
                            self.mem.pending_reopen.insert(id, app);
                        }
                    } else {
                        self.mem.remember_closed(app);
                    }
                }
                for &(id, policy) in &report.policies {
                    if let Some(original) = policy {
                        self.mem.efficiency.entry(id).or_insert(original);
                    } else if report.operation == Operation::RestoreEfficiency {
                        self.mem.efficiency.remove(&id);
                    }
                }
                if report.operation.closes() {
                    for row in &report.rows {
                        if matches!(row.outcome, Outcome::Changed | Outcome::Gone) {
                            self.mem.checked.remove(&row.id);
                        }
                    }
                }
                let complete = report
                    .rows
                    .iter()
                    .all(|row| matches!(row.outcome, Outcome::Changed | Outcome::Gone));
                self.toast(
                    if complete {
                        ToastKind::Success
                    } else {
                        ToastKind::Info
                    },
                    report.summary(),
                );
                self.mem.report = Some(report);
                self.mem.results_open = !complete;
                self.sys.refresh_processes_soon();
            }
        }
    }

    pub fn execute_recovery(&mut self, action: RecoveryAction, ctx: &egui::Context) {
        if !self.mutations_running() {
            self.mem.recovering = Task::spawn(ctx, move || recovery::recover(action));
        }
    }

    pub(crate) fn inspect_process(&mut self, id: Identity) {
        self.page = crate::app::Page::Memory;
        self.mem.selected = Some(RowId::Process(id));
        self.mem.inspect_open = true;
    }

    pub(crate) fn review_explorer_restart(&mut self) {
        self.mem.recent_open = false;
        self.modal = Some(Modal {
            title: "Restart Windows Explorer?".into(),
            body: "Restart the Windows desktop and taskbar. Open File Explorer windows may close."
                .into(),
            details: Vec::new(),
            note: Some("Wait for file copies and moves to finish before restarting.".into()),
            confirm: "Restart Explorer".into(),
            danger: false,
            action: Action::Recovery(RecoveryAction::Explorer { restart: true }),
        });
    }

    pub(crate) fn show_recent_apps(&mut self) {
        self.mem.recent_open = true;
    }

    pub(crate) fn recovery_buttons(&mut self, ui: &mut Ui) {
        let enabled = !self.mutations_running();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(enabled, widgets::small("Restore taskbar"))
                .clicked()
            {
                self.execute_recovery(RecoveryAction::Explorer { restart: false }, ui.ctx());
            }
            if ui
                .add_enabled(enabled, widgets::small("Restart Explorer…"))
                .clicked()
            {
                self.review_explorer_restart();
            }
            let recent = self.mem.recent.len();
            let label = if recent == 0 {
                "Recently closed".into()
            } else {
                format!("Recently closed ({recent})")
            };
            if ui.add(widgets::small(label)).clicked() {
                self.show_recent_apps();
            }
            if self.mem.recovering.running() {
                ui.spinner();
                ui.label(widgets::dim("Opening…"));
            }
        });
    }

    pub(crate) fn recent_apps_ui(&mut self, ctx: &egui::Context) {
        if !self.mem.recent_open || self.modal.is_some() {
            return;
        }
        let mut open = self.mem.recent_open;
        let mut reopen = None;
        let enabled = !self.mutations_running();
        egui::Window::new("Recently closed apps")
            .id(egui::Id::new("recently_closed_apps"))
            .default_width(520.0)
            .resizable(false)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                if self.mem.recent.is_empty() {
                    ui.label(widgets::dim("Desktop apps closed with PCSweep will appear here when they can be reopened."));
                    return;
                }
                ui.label(widgets::dim("Reopen a desktop app on this PC. Any session restore is handled by the app."));
                ScrollArea::vertical()
                    .id_salt("recent_closed_scroll")
                    .max_height((ctx.screen_rect().height() - 180.0).clamp(100.0, 320.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (index, app) in self.mem.recent.iter().enumerate() {
                            ui.push_id(index, |ui| {
                                widgets::tight_card(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if ui.add_enabled(enabled && reopen.is_none(), widgets::small("Reopen")).clicked() {
                                            reopen = Some(app.clone());
                                        }
                                        self.mem.icons.show(ui, &IconSource::Executable(app.path.clone()));
                                        ui.vertical(|ui| {
                                            ui.add(egui::Label::new(widgets::strong(&app.name, 14.0)).truncate());
                                            ui.add(egui::Label::new(widgets::faint(app.path.display().to_string())).truncate())
                                                .on_hover_text(app.path.display().to_string());
                                        });
                                    });
                                });
                            });
                        }
                    });
            });
        self.mem.recent_open = open;
        self.mem.icons.start_pending(ctx);
        if let Some(app) = reopen {
            self.execute_recovery(RecoveryAction::Reopen(app), ctx);
        }
    }

    fn review_processes(&mut self, operation: Operation, selection: Selection) {
        let snapshot = &self.sys.processes;
        let (name, candidates): (String, Vec<&Process>) = match selection {
            Selection::Checked => (
                "selected processes".into(),
                snapshot
                    .processes
                    .iter()
                    .filter(|process| self.mem.checked.contains(&process.identity))
                    .collect(),
            ),
            Selection::Row(id) => {
                let Some(row) = find_row(snapshot, &id) else {
                    return;
                };
                (row.name.clone(), members(snapshot, row).collect())
            }
            Selection::Pid(id) => {
                let Some(process) = snapshot.processes.iter().find(|p| p.identity == id) else {
                    return;
                };
                (process.name.clone(), vec![process])
            }
        };
        let mut affected: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|p| {
                p.access == Access::Available
                    && (operation != Operation::Close || !p.windows.is_empty())
                    && (operation != Operation::RestoreEfficiency
                        || self.mem.efficiency.contains_key(&p.identity))
            })
            .collect();
        if affected.is_empty() {
            self.toast(
                ToastKind::Info,
                "No selected processes can perform this action. Open process details to see why.",
            );
            return;
        }
        affected.sort_unstable_by_key(|p| (&p.name, p.identity));
        let windows_component = affected.iter().any(|p| p.windows_component);
        let count = affected.len();
        let excluded = candidates.len() - count;
        let mut details: Vec<_> = affected
            .iter()
            .map(|p| {
                format!(
                    "{} • PID {}{}",
                    p.name,
                    p.identity.pid,
                    p.image
                        .as_ref()
                        .map(|path| format!("\n{}", path.display()))
                        .unwrap_or_default(),
                )
            })
            .collect();
        if excluded > 0 {
            details.push(format!(
                "{excluded} selected processes are ineligible for this action."
            ));
        }
        let (title, body, confirm, note) = match operation {
            Operation::Close => (
                format!("Close {name} normally?"),
                if count == 1 {
                    "Ask this app to close its windows. It can show a save prompt.".into()
                } else {
                    format!("Ask {count} processes to close their windows. Apps can show a save prompt.")
                },
                "Ask to close",
                Some("Use End task if a background process stays running or an app stops responding."),
            ),
            Operation::End => (
                format!("End {name}?"),
                if count == 1 {
                    "End this process now. Unsaved work can be lost.".into()
                } else {
                    format!("End the {count} listed processes now. Unsaved work can be lost.")
                },
                "End task",
                windows_component.then_some("This selection includes a Windows component. Related apps, the desktop, or a service may be interrupted. Windows can restart some processes automatically."),
            ),
            Operation::Efficiency => (
                format!("Use efficiency mode for {name}?"),
                "Apply Windows’ power-efficient CPU policy to the listed processes.".into(),
                "Enable efficiency",
                Some("Apps can run more slowly. The previous settings are available to restore while PCSweep stays open."),
            ),
            Operation::RestoreEfficiency => (
                format!("Restore efficiency settings for {name}?"),
                "Restore the execution-speed policy saved for the listed processes.".into(),
                "Restore settings",
                None,
            ),
            Operation::Trim => (
                format!("Trim resident RAM for {name}?"),
                "Ask Windows to reclaim eligible pages from the listed processes’ working sets.".into(),
                "Trim RAM",
                Some("Pages can reload as soon as apps need them. This can cause stutter and leaves private commit unchanged."),
            ),
        };
        let targets = affected
            .into_iter()
            .map(|process| Target {
                id: process.identity,
                name: process.name.clone(),
                windows: if operation == Operation::Close {
                    process.windows.clone()
                } else {
                    Vec::new()
                },
                original: self.mem.efficiency.get(&process.identity).copied(),
                reopenable: !process.windows.is_empty() && !process.windows_component,
            })
            .collect();
        self.modal = Some(Modal {
            title,
            body,
            details,
            note: note.map(str::to_owned),
            confirm: confirm.into(),
            danger: operation == Operation::End,
            action: Action::Memory(MemoryAction { operation, targets }),
        });
    }

    pub fn page_memory(&mut self, ui: &mut Ui) {
        let mut acts = Vec::new();
        widgets::page_header(
            ui,
            "Memory and running apps",
            "See what is using RAM and manage running apps.",
        );
        let pressure = system::memory_pressure(
            self.sys.mem_total,
            self.sys.mem_avail,
            self.sys.commit_total,
            self.sys.commit_limit,
        );
        let color = match pressure {
            MemoryPressure::Normal => color_success(),
            MemoryPressure::Elevated => color_warning(),
            MemoryPressure::Critical => color_error(),
        };
        self.render_memory_summary(ui, color, &mut acts);
        if pressure != MemoryPressure::Normal {
            let sustained = self
                .mem
                .pressure_since
                .is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(10));
            let message = match (pressure, sustained) {
                (MemoryPressure::Critical, true) => {
                    "Memory pressure has stayed high. Save your work and close a heavy app."
                }
                (MemoryPressure::Critical, false) => "Memory is very tight right now.",
                _ => "Memory is getting tight. Close an app you are no longer using.",
            };
            ui.colored_label(color, message);
        }
        self.recovery_buttons(ui);
        ui.add_space(8.0);
        self.render_process_table(ui, false, &mut acts);
        self.process_windows(ui.ctx(), &mut acts);
        self.apply_process_actions(ui.ctx(), acts);
    }

    pub fn background_processes(&mut self, ui: &mut Ui) {
        self.recovery_buttons(ui);
        let mut acts = Vec::new();
        self.render_process_table(ui, true, &mut acts);
        self.process_windows(ui.ctx(), &mut acts);
        self.apply_process_actions(ui.ctx(), acts);
    }

    fn apply_process_actions(&mut self, ctx: &egui::Context, actions: Vec<MAct>) {
        for act in actions {
            match act {
                MAct::Inspect(id) => {
                    self.mem.selected = Some(id);
                    self.mem.inspect_open = true;
                }
                MAct::Ask(operation, selection) => self.review_processes(operation, selection),
                MAct::Check(ids, on) => {
                    if on {
                        self.mem.checked.extend(ids);
                    } else {
                        self.mem.checked.retain(|id| !ids.contains(id));
                    }
                }
                MAct::Clear => self.mem.checked.clear(),
                MAct::StopWaiting => self.mem.stop_waiting.store(true, Ordering::Relaxed),
                MAct::Refresh => self.sys.refresh_processes_soon(),
                MAct::Details(value) => self.mem.details = value,
                MAct::Show(id, window) => {
                    if let Err(error) = running::show_window(id, &window) {
                        self.toast(ToastKind::Info, error);
                    }
                }
                MAct::Location(path) => util::show_in_explorer(&path),
                MAct::PageFile => {
                    if let Err(error) = util::open_system_tool("SystemPropertiesPerformance.exe") {
                        self.toast(ToastKind::Error, error);
                    }
                }
            }
        }
        self.mem.icons.start_pending(ctx);
    }

    fn render_process_table(&mut self, ui: &mut Ui, background: bool, acts: &mut Vec<MAct>) {
        let available = !self.mutations_running();
        let snapshot = &self.sys.processes;
        let view = if background {
            &mut self.mem.background_view
        } else {
            &mut self.mem.view
        };
        let busy = self.mem.running;
        ui.horizontal_wrapped(|ui| {
            for scope in [Scope::All, Scope::Windows, Scope::Background, Scope::Pid] {
                if ui
                    .add(widgets::chip(scope.label(), view.scope == scope))
                    .clicked()
                {
                    view.scope = scope;
                }
            }
            if ui
                .add(widgets::small(widgets::icon_text(
                    ic::REFRESH,
                    "Refresh",
                    13.0,
                    color_ink(),
                )))
                .clicked()
            {
                acts.push(MAct::Refresh);
            }
        });
        ui.horizontal_wrapped(|ui| {
            widgets::search_box(
                ui,
                &mut view.filter,
                "Search name, PID, path or window",
                247.0,
            );
            egui::ComboBox::from_id_salt("process_sort")
                .selected_text(format!("Sort by {}", view.sort.label()))
                .show_ui(ui, |ui| {
                    for sort in [
                        Sort::Ram,
                        Sort::Cpu,
                        Sort::Private,
                        Sort::Growth,
                        Sort::Name,
                        Sort::Pid,
                    ] {
                        ui.selectable_value(&mut view.sort, sort, sort.label());
                    }
                });
        });
        // Entering an exact PID should expose that process alone. Ending a
        // matching row can never include siblings with the same executable.
        let scope = if view.filter.trim().parse::<u32>().is_ok() {
            Scope::Pid
        } else {
            view.scope
        };
        let table = if scope.individual() {
            &snapshot.individuals
        } else {
            &snapshot.groups
        };
        let indices = view.rows(snapshot, table, scope);
        let checked = &self.mem.checked;
        let shown_count = indices.len();
        if let Some(error) = &self.sys.process_warning {
            ui.add(Label::new(RichText::new(error).color(color_warning())).wrap());
        }
        if let Some(at) = snapshot.sampled_at {
            let age = at.elapsed().as_secs();
            let view_name = if scope.individual() {
                "processes"
            } else {
                "app groups"
            };
            ui.label(widgets::faint(format!(
                "{} {view_name} shown · {} processes total · {} groups with open windows{}",
                fmt_num(shown_count as u64),
                fmt_num(snapshot.processes.len() as u64),
                snapshot.foreground,
                if age > 8 {
                    format!(" · updated {age}s ago")
                } else {
                    String::new()
                },
            )));
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(widgets::dim("Reading running processes…"));
            });
        }

        let selected = selection_summary(snapshot, table, indices, checked, scope);
        selection_bar(ui, &selected, busy, available, acts);
        if let Some(report) = &self.mem.report {
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::dim(report.summary()));
                if ui.add(widgets::small("Process results")).clicked() {
                    self.mem.results_open = true;
                }
            });
        }
        if indices.is_empty() {
            if snapshot.sampled_at.is_some() {
                widgets::tight_card(ui, |ui| {
                    ui.label(widgets::dim("No processes match this view."));
                    if ui.add(widgets::small("Show all apps")).clicked() {
                        view.scope = Scope::All;
                        view.filter.clear();
                    }
                });
            }
            return;
        }

        let columns = TableColumns::new(ui.available_width());
        widgets::row(ui, 24.0, |ui| {
            ui.add_space(55.0);
            ui.label(widgets::faint(if scope.individual() {
                "Process / PID"
            } else {
                "App or process group"
            }));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(columns.action);
                columns.header(ui);
            });
        });
        let icons = &mut self.mem.icons;
        ScrollArea::vertical()
            .id_salt(("live_process_rows", scope.label()))
            .auto_shrink([false, false])
            .show_rows(ui, 42.0, indices.len(), |ui, range| {
                for &index in &indices[range] {
                    let row = &table[index];
                    ui.push_id(&row.id, |ui| {
                        process_row(
                            ui, snapshot, row, columns, scope, available, checked, icons, acts,
                        );
                    });
                }
            });
    }

    fn process_windows(&mut self, ctx: &egui::Context, acts: &mut Vec<MAct>) {
        if self.modal.is_some() {
            return;
        }
        let available = !self.mutations_running();
        if self.mem.inspect_open {
            let snapshot = &self.sys.processes;
            let row = self
                .mem
                .selected
                .as_ref()
                .and_then(|id| find_row(snapshot, id));
            let mut open = self.mem.inspect_open;
            egui::Window::new("Process details")
                .id(egui::Id::new("process_inspector"))
                .open(&mut open)
                .default_width(660.0)
                .min_width(300.0)
                .vscroll(true)
                .collapsible(false)
                .show(ctx, |ui| {
                    let Some(row) = row else {
                        ui.label(widgets::dim(
                            "This process has exited. Choose another row for details.",
                        ));
                        return;
                    };
                    process_inspector(ui, snapshot, row, &mut self.mem, available, acts);
                });
            self.mem.inspect_open = open;
        }
        if self.mem.results_open {
            let Some(report) = &self.mem.report else {
                return;
            };
            egui::Window::new("Process results")
                .open(&mut self.mem.results_open)
                .default_width(560.0)
                .vscroll(true)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.add(Label::new(widgets::strong(report.summary(), 16.0)).wrap());
                    for row in &report.rows {
                        ui.separator();
                        ui.label(widgets::strong(
                            format!("{} · PID {}", row.name, row.id.pid),
                            14.0,
                        ));
                        let detail = match row.outcome {
                            Outcome::Changed if report.operation.closes() => "Exit confirmed",
                            Outcome::Changed => "Completed",
                            _ => &row.detail,
                        };
                        ui.add(Label::new(widgets::dim(detail)).wrap());
                    }
                });
        }
    }
    fn render_memory_summary(&self, ui: &mut Ui, color: egui::Color32, acts: &mut Vec<MAct>) {
        let s = &self.sys;
        widgets::card(ui, |ui| {
            if ui.available_width() < 800.0 {
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if self.mem.details {
                            "Fewer details"
                        } else {
                            "More details"
                        };
                        if ui.add(widgets::small(label)).clicked() {
                            acts.push(MAct::Details(!self.mem.details));
                        }
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let frac = s.mem_frac();
                            widgets::ring(
                                ui,
                                frac,
                                48.0,
                                5.0,
                                color,
                                &format!("{:.0}%", frac * 100.0),
                                "RAM",
                            );
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::Label::new(widgets::strong(
                                        format!("{} available", fmt_bytes(s.mem_avail)),
                                        15.0,
                                    ))
                                    .truncate(),
                                );
                                let commit = if s.memory_info_available {
                                    format!(
                                        "Commit {} / {}",
                                        fmt_bytes(s.commit_total),
                                        fmt_bytes(s.commit_limit)
                                    )
                                } else {
                                    "Commit unavailable".into()
                                };
                                ui.add(
                                    egui::Label::new(widgets::dim(format!(
                                        "{commit} · CPU {:.0}%",
                                        s.cpu
                                    )))
                                    .truncate(),
                                )
                                .on_hover_text(format!(
                                    "{} in use of {} RAM · {} logical cores",
                                    fmt_bytes(s.mem_used),
                                    fmt_bytes(s.mem_total),
                                    s.cores
                                ));
                            });
                        });
                    });
                });
            } else {
                ui.horizontal(|ui| {
                    let frac = s.mem_frac();
                    let percent = format!("{:.0}%", frac * 100.0);
                    widgets::ring(ui, frac, 64.0, 7.0, color, &percent, "RAM");
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        let available = format!("{} available", fmt_bytes(s.mem_avail));
                        ui.label(widgets::strong(available, 16.0));
                        ui.label(widgets::dim(format!(
                            "{} in use of {}",
                            fmt_bytes(s.mem_used),
                            fmt_bytes(s.mem_total)
                        )));
                    });
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        ui.label(widgets::strong("Committed memory", 14.0));
                        if s.memory_info_available {
                            ui.label(widgets::dim(format!(
                                "{} of {} limit",
                                fmt_bytes(s.commit_total),
                                fmt_bytes(s.commit_limit)
                            )));
                        } else {
                            ui.label(widgets::dim("Unavailable"));
                        }
                    });
                    ui.add_space(24.0);
                    ui.vertical(|ui| {
                        ui.label(widgets::strong(format!("CPU {:.0}%", s.cpu), 14.0));
                        ui.label(widgets::dim(format!("{} logical cores", s.cores)))
                            .on_hover_text(&s.cpu_name);
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if self.mem.details {
                            "Fewer details"
                        } else {
                            "More details"
                        };
                        if ui.add(widgets::small(label)).clicked() {
                            acts.push(MAct::Details(!self.mem.details));
                        }
                    });
                });
            }
            if self.mem.details {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if s.memory_info_available {
                        let cache = format!("System cache {}", fmt_bytes(s.system_cache));
                        ui.label(widgets::faint(cache)).on_hover_text(CACHE_HINT);
                        ui.label(widgets::faint(format!(
                            "Commit peak since boot {}",
                            fmt_bytes(s.commit_peak)
                        )));
                    }
                    ui.label(widgets::faint(format!(
                        "Page file {} of {} in use",
                        fmt_bytes(s.swap_used),
                        fmt_bytes(s.swap_total)
                    )));
                    if ui.add(widgets::small("Virtual memory settings")).clicked() {
                        acts.push(MAct::PageFile);
                    }
                });
                ui.label(widgets::faint(
                    "Private commit is memory an app has reserved for itself; it's different from resident RAM and page-file usage. Let Windows manage page-file size.",
                ));
            }
        });
    }
}

struct SelectionSummary {
    groups: usize,
    processes: usize,
    hidden: usize,
    can_close: bool,
    selected_ram: u64,
    by_pid: bool,
}
fn selection_summary(
    snapshot: &Snapshot,
    table: &[Row],
    indices: &[usize],
    checked: &HashSet<Identity>,
    scope: Scope,
) -> SelectionSummary {
    let mut summary = SelectionSummary {
        groups: 0,
        processes: 0,
        hidden: 0,
        can_close: false,
        selected_ram: 0,
        by_pid: scope.individual(),
    };
    if checked.is_empty() {
        return summary;
    }
    summary.groups = snapshot
        .groups
        .iter()
        .filter(|row| members(snapshot, row).any(|p| checked.contains(&p.identity)))
        .count();
    let visible = indices
        .iter()
        .flat_map(|&index| members(snapshot, &table[index]))
        .filter(|p| checked.contains(&p.identity))
        .count();
    for p in snapshot
        .processes
        .iter()
        .filter(|p| p.access == Access::Available && checked.contains(&p.identity))
    {
        summary.processes += 1;
        summary.can_close |= !p.windows.is_empty();
        summary.selected_ram = summary.selected_ram.saturating_add(p.memory.unwrap_or(0));
    }
    summary.hidden = summary.processes.saturating_sub(visible);
    summary
}

fn selection_bar(
    ui: &mut Ui,
    summary: &SelectionSummary,
    busy: Option<Operation>,
    available: bool,
    acts: &mut Vec<MAct>,
) {
    if summary.processes == 0 && busy.is_none() {
        return;
    }
    widgets::tight_card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if let Some(operation) = busy {
                ui.spinner();
                ui.label(widgets::dim(format!("{} in progress…", operation.label())));
                if operation.closes() && ui.add(widgets::small("Stop waiting")).clicked() {
                    acts.push(MAct::StopWaiting);
                }
                return;
            }
            let (count, singular, plural) = if summary.by_pid {
                (summary.processes, "process", "processes")
            } else {
                (summary.groups, "app", "apps")
            };
            ui.label(widgets::strong(
                format!(
                    "{count} {} selected",
                    if count == 1 { singular } else { plural }
                ),
                14.5,
            ))
            .on_hover_text(format!(
                "{} processes · {} resident RAM",
                summary.processes,
                fmt_bytes(summary.selected_ram)
            ));
            if summary.hidden > 0 {
                ui.label(widgets::faint(format!(
                    "{} outside this view",
                    summary.hidden
                )));
            }
            if ui
                .add_enabled(
                    available && summary.can_close,
                    widgets::small("Close selected…"),
                )
                .on_disabled_hover_text("Select a process with an open window to close normally.")
                .clicked()
            {
                acts.push(MAct::Ask(Operation::Close, Selection::Checked));
            }
            if ui
                .add_enabled(available, widgets::small("End selected…"))
                .clicked()
            {
                acts.push(MAct::Ask(Operation::End, Selection::Checked));
            }
            if ui.add(widgets::small("Clear")).clicked() {
                acts.push(MAct::Clear);
            }
        });
    });
}

#[derive(Clone, Copy)]
struct TableColumns {
    action: f32,
    wide: bool,
}
impl TableColumns {
    fn new(width: f32) -> Self {
        Self {
            action: 100.0,
            wide: width >= 690.0,
        }
    }
    fn header(self, ui: &mut Ui) {
        widgets::cell(ui, 57.0, true, widgets::faint("CPU"));
        widgets::cell(ui, 83.0, true, widgets::faint("RAM"));
        if self.wide {
            widgets::cell(ui, 90.0, true, widgets::faint("Private"));
            widgets::cell(ui, 88.0, true, widgets::faint("Growth"));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_row(
    ui: &mut Ui,
    snapshot: &Snapshot,
    row: &Row,
    columns: TableColumns,
    scope: Scope,
    available: bool,
    checked: &HashSet<Identity>,
    icons: &mut IconCache,
    acts: &mut Vec<MAct>,
) {
    let first = &snapshot.processes[row.members[0]];
    let selectable = members(snapshot, row).filter(|p| p.access == Access::Available);
    let count = selectable
        .clone()
        .filter(|p| checked.contains(&p.identity))
        .count();
    let restriction = first
        .access
        .restriction()
        .unwrap_or("Windows did not verify a process in this row.");
    widgets::row(ui, 42.0, |ui| {
        let mut on = row.available > 0 && count == row.available;
        let checkbox =
            egui::Checkbox::without_text(&mut on).indeterminate(count > 0 && count < row.available);
        let selection = ui
            .add_enabled(row.available > 0, checkbox)
            .on_hover_text("Select for a bulk action");
        if row.available == 0 {
            selection.on_disabled_hover_text(restriction);
        } else if selection.changed() {
            acts.push(MAct::Check(selectable.map(|p| p.identity).collect(), on));
        }
        let source = first
            .image
            .as_ref()
            .map(|path| IconSource::Executable(path.clone()))
            .unwrap_or(IconSource::Generic);
        icons.show_sized(ui, &source, 22.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let end = ui.add_enabled(
                available && row.available > 0,
                widgets::small("End task…").min_size(egui::vec2(columns.action - 8.0, 0.0)),
            );
            if row.available == 0 { end.on_disabled_hover_text(restriction); }
            else if end.clicked() { acts.push(MAct::Ask(Operation::End, Selection::Row(row.id.clone()))); }
            let total = row.members.len();
            widgets::cell(ui, 57.0, true, widgets::faint(metric(row.cpu_known, total, format!("{:.1}%", row.cpu))));
            widgets::cell(ui, 83.0, true, widgets::strong(metric(row.memory_known, total, fmt_bytes(row.memory)), 14.0));
            if columns.wide {
                widgets::cell(ui, 90.0, true, widgets::dim(metric(row.private_known, total, fmt_bytes(row.private))))
                    .on_hover_text("Private commit is memory reserved by this process. ~ indicates a partial total.");
                widgets::cell(ui, 88.0, true, RichText::new(growth_text(row.growth, row.observed_secs))
                    .color(if row.growth > 0 { color_warning() } else { color_muted() }))
                    .on_hover_text("Change in private commit across the available samples, up to 10 minutes.");
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let label = if scope.individual() { format!("{} · {}", row.name, first.identity.pid) } else { row.title() };
                let button = ui.add(egui::Button::new(RichText::new(label).color(color_ink())).frame(false).truncate())
                    .on_hover_text(format!(
                        "{}\n{}",
                        first.image.as_ref().map_or_else(|| first.name.clone(), |path| path.display().to_string()),
                        if row.window_count > 0 { "Open process details and switch windows" } else { "Open process details" }
                    ));
                if button.clicked() { acts.push(MAct::Inspect(row.id.clone())); }
                button.context_menu(|ui| {
                    if ui.button("Process details").clicked() {
                        acts.push(MAct::Inspect(row.id.clone()));
                        ui.close_menu();
                    }
                    if ui.add_enabled(available && selectable_window(snapshot, row), egui::Button::new("Close normally…")).clicked() {
                        acts.push(MAct::Ask(Operation::Close, Selection::Row(row.id.clone())));
                        ui.close_menu();
                    }
                    if let Some(path) = &first.image {
                        if ui.add_enabled(running::local_image_path(path), egui::Button::new("Open file location")).clicked() {
                            acts.push(MAct::Location(path.clone()));
                            ui.close_menu();
                        }
                        if ui.button("Copy executable path").clicked() {
                            ui.ctx().copy_text(path.display().to_string());
                            ui.close_menu();
                        }
                    }
                });
            });
        });
    });
}

fn selectable_window(snapshot: &Snapshot, row: &Row) -> bool {
    members(snapshot, row).any(|p| p.access == Access::Available && !p.windows.is_empty())
}

fn process_inspector(
    ui: &mut Ui,
    snapshot: &Snapshot,
    row: &Row,
    mem: &mut MemState,
    available: bool,
    acts: &mut Vec<MAct>,
) {
    let first = &snapshot.processes[row.members[0]];
    ui.horizontal_wrapped(|ui| {
        let icon = first
            .image
            .as_ref()
            .map(|p| IconSource::Executable(p.clone()))
            .unwrap_or(IconSource::Generic);
        mem.icons.show(ui, &icon);
        ui.label(widgets::strong(row.title(), 20.0));
        if first.windows_component {
            ui.label(widgets::faint("Windows component"));
        }
    });
    if let Some(path) = &first.image {
        ui.add(Label::new(widgets::dim(path.display().to_string())).wrap());
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    running::local_image_path(path),
                    widgets::small("Open file location"),
                )
                .clicked()
            {
                acts.push(MAct::Location(path.clone()));
            }
            if ui.add(widgets::small("Copy path")).clicked() {
                ui.ctx().copy_text(path.display().to_string());
            }
        });
    }
    ui.horizontal_wrapped(|ui| {
        let can_close = selectable_window(snapshot, row);
        if ui
            .add_enabled(available && can_close, widgets::small("Close normally…"))
            .on_disabled_hover_text("A normal close needs an open window.")
            .clicked()
        {
            acts.push(MAct::Ask(Operation::Close, Selection::Row(row.id.clone())));
        }
        if ui
            .add_enabled(available && row.available > 0, widgets::small("End task…"))
            .clicked()
        {
            acts.push(MAct::Ask(Operation::End, Selection::Row(row.id.clone())));
        }
        let undo = members(snapshot, row).any(|p| mem.efficiency.contains_key(&p.identity));
        let mode = if undo {
            Operation::RestoreEfficiency
        } else {
            Operation::Efficiency
        };
        if ui
            .add_enabled(
                available && row.available > 0,
                widgets::small(if undo {
                    "Undo efficiency…"
                } else {
                    "Efficiency mode…"
                }),
            )
            .clicked()
        {
            acts.push(MAct::Ask(mode, Selection::Row(row.id.clone())));
        }
        if ui
            .add_enabled(available && row.available > 0, widgets::small("Trim RAM…"))
            .clicked()
        {
            acts.push(MAct::Ask(Operation::Trim, Selection::Row(row.id.clone())));
        }
    });
    if row.available == 0 {
        if let Some(reason) = first.access.restriction() {
            ui.add(Label::new(widgets::dim(reason)).wrap());
        }
    } else if row.window_count == 0 {
        ui.label(widgets::dim("This group has no open windows on this desktop. Use End task to stop a background process."));
    }
    ui.separator();
    for process in members(snapshot, row) {
        ui.push_id(process.identity, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::strong(
                    format!("PID {}", process.identity.pid),
                    14.0,
                ));
                if ui.add(widgets::small("Copy PID")).clicked() {
                    ui.ctx().copy_text(process.identity.pid.to_string());
                }
                if process.parent != 0 {
                    ui.label(widgets::faint(format!("Parent {}", process.parent)));
                }
                if let Some(session) = process.session {
                    ui.label(widgets::faint(format!("Session {session}")));
                }
                if let Some(bytes) = process.memory {
                    ui.label(widgets::dim(format!("{} RAM", fmt_bytes(bytes))));
                }
                if let Some(bytes) = process.private {
                    ui.label(widgets::faint(format!(
                        "{} private commit",
                        fmt_bytes(bytes)
                    )));
                }
                if row.members.len() > 1
                    && ui
                        .add_enabled(
                            available && process.access == Access::Available,
                            widgets::small("End PID…"),
                        )
                        .clicked()
                {
                    acts.push(MAct::Ask(Operation::End, Selection::Pid(process.identity)));
                }
            });
            if row.members.len() > 1 {
                if let Some(reason) = process.access.restriction() {
                    ui.add(Label::new(widgets::faint(reason)).wrap());
                }
            }
            for window in &process.windows {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(process.identity.created != 0, widgets::small("Switch to"))
                        .clicked()
                    {
                        acts.push(MAct::Show(process.identity, window.clone()));
                    }
                    ui.add(Label::new(widgets::dim(&window.title)).truncate())
                        .on_hover_text(&window.title);
                });
            }
        });
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> MemState {
    MemState {
        icons: IconCache::preview_fixture(),
        ..MemState::default()
    }
}

#[cfg(test)]
pub(crate) fn bulk_preview_fixture() -> MemState {
    let mut mem = preview_fixture();
    mem.checked.extend([7001, 7002, 7003].map(|pid| Identity {
        pid,
        created: u64::from(pid) + 1,
    }));
    mem
}

#[cfg(test)]
pub(crate) fn memory_preview_fixture() -> MemState {
    let mut mem = preview_fixture();
    mem.checked.insert(Identity {
        pid: 7001,
        created: 7002,
    });
    mem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_group_action_keeps_exact_reviewed_pids_and_allows_explorer() {
        let mut app = App::test_fixture();
        let snapshot = &app.sys.processes;
        let explorer = snapshot
            .groups
            .iter()
            .find(|row| row.name == "explorer.exe")
            .unwrap();
        let explorer_id = snapshot.processes[explorer.members[0]].identity;
        app.review_processes(Operation::End, Selection::Row(explorer.id.clone()));
        let modal = app.modal.take().unwrap();
        assert!(modal.note.unwrap().contains("Windows component"));
        let Action::Memory(action) = modal.action else {
            panic!("expected a process action")
        };
        assert_eq!(action.targets.len(), 1);
        assert_eq!(action.targets[0].id, explorer_id);
    }

    #[test]
    fn pid_search_targets_only_that_instance_and_background_includes_helpers() {
        let snapshot = running::preview_fixture(1024 * 1024);
        let mut view = ProcessView {
            scope: Scope::Background,
            ..ProcessView::default()
        };
        let indices = view.rows(&snapshot, &snapshot.individuals, Scope::Background);
        assert!(indices.iter().any(|&i| snapshot.individuals[i].id
            == RowId::Process(Identity {
                pid: 7002,
                created: 7003
            })));
        assert!(!indices
            .iter()
            .any(|&i| snapshot.individuals[i].window_count > 0));
        view.filter = "7002".into();
        let indices = view.rows(&snapshot, &snapshot.individuals, Scope::Pid);
        assert_eq!(indices.len(), 1);
        let row = &snapshot.individuals[indices[0]];
        assert_eq!(row.members.len(), 1);
        assert_eq!(snapshot.processes[row.members[0]].identity.pid, 7002);
    }

    #[test]
    fn close_review_only_includes_processes_that_can_receive_window_messages() {
        let mut app = App::test_fixture();
        app.mem
            .checked
            .extend([7001, 7002, 7011].map(|pid| Identity {
                pid,
                created: u64::from(pid) + 1,
            }));
        app.review_processes(Operation::Close, Selection::Checked);
        let modal = app.modal.take().unwrap();
        let Action::Memory(action) = modal.action else {
            panic!("expected a process action")
        };
        assert_eq!(action.targets.len(), 1);
        assert_eq!(action.targets[0].id.pid, 7001);
        app.review_processes(Operation::End, Selection::Checked);
        let Action::Memory(action) = app.modal.take().unwrap().action else {
            panic!("expected a process action")
        };
        assert_eq!(action.targets.len(), 3);
    }

    #[test]
    fn stopping_a_batch_keeps_reviewed_processes_untouched() {
        let action = MemoryAction {
            operation: Operation::End,
            targets: vec![Target {
                id: Identity {
                    pid: 1234,
                    created: 99,
                },
                name: "Test".into(),
                windows: Vec::new(),
                original: None,
                reopenable: false,
            }],
        };
        let report = run_operation(action, &AtomicBool::new(true));
        assert!(report.rows[0].outcome == Outcome::Failed);
        assert!(report.rows[0].detail.contains("stopped"));
    }
}
