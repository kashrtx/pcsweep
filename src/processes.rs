use crate::app::{Action, App, Modal, Page, ToastKind};
use crate::system::{self, MemoryPressure, PowerPolicy, ProcGroup};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Layout, RichText, ScrollArea, Ui};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum MemSort {
    #[default]
    Resident,
    Private,
    Growth,
    Cpu,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum MemoryKind {
    Close,
    Trim,
    Efficiency,
    RestoreEfficiency,
}
#[derive(Clone)]
struct MemoryTarget {
    pid: u32,
    start: u64,
    restore: Option<PowerPolicy>,
    app: String,
}
/// A confirmed process snapshot; PID reuse cannot redirect it to a new app.
pub struct MemoryAction {
    kind: MemoryKind,
    targets: Vec<MemoryTarget>,
    name: String,
}
pub struct MemoryReport {
    kind: MemoryKind,
    name: String,
    attempted: usize,
    changed: Vec<((u32, u64), Option<PowerPolicy>)>,
    before: u64,
    after: u64,
    failed: Vec<String>,
    /// Close only: apps whose every reviewed process has actually exited.
    closed_apps: Vec<String>,
    /// Close only: apps that were asked to close but are still running.
    open_apps: Vec<String>,
}
#[derive(Default)]
pub struct MemState {
    pub filter: String,
    sort: MemSort,
    selected: Option<String>,
    advanced: bool,
    efficiency: HashMap<(u32, u64), PowerPolicy>,
    pressure_since: Option<(MemoryPressure, Instant)>,
    last_identity_refresh: Option<Instant>,
    pub job: Task<MemoryReport>,
    /// Lower-case names of app groups ticked for a bulk close.
    checked: HashSet<String>,
    details: bool,
    stop_waiting: Arc<AtomicBool>,
    /// True while a close request is waiting to see which apps exit.
    closing: bool,
}
impl MemState {
    pub fn busy(&self) -> bool {
        self.job.running()
    }
}
enum MAct {
    Filter(String),
    Sort(MemSort),
    Select(String),
    Advanced(bool),
    Ask(MemoryKind, ProcGroup),
    AskKill(ProcGroup),
    Go(Page),
    PageFile,
    Details(bool),
    Check(String, bool),
    CheckMany(Vec<String>, bool),
    ClearChecked,
    AskCloseChecked,
    StopWaiting,
}

const CLOSE_NOTE: &str = "Apps can show a save prompt or refuse. Nothing is force-closed, and apps started after this review are not included.";
const CACHE_HINT: &str = "Cache overlaps available memory, so don't add the two. Windows reuses standby cache when apps need RAM.";
const PRIVATE_HINT: &str = "Private commit. Unavailable when Windows won't report it for this app, for example elevated apps while PCSweep isn't running as administrator.";
const PROTECTED_HINT: &str = "Windows, security and PCSweep processes are protected.";

/// Windows, security and PCSweep's own processes can't be selected for closing.
fn eligible(group: &ProcGroup) -> bool {
    !system::is_critical(&group.name) && !group.pids.contains(&std::process::id())
}

fn private_text(group: &ProcGroup) -> String {
    if group.private_known == 0 {
        "unavailable".into()
    } else if group.private_known < group.count {
        format!("~{}", fmt_bytes(group.private_bytes))
    } else {
        fmt_bytes(group.private_bytes)
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn list_names(names: &[String]) -> String {
    match names.len() {
        0..=3 => names.join(", "),
        n => format!("{}, {} and {} more", names[0], names[1], n - 2),
    }
}

struct AppClose {
    name: String,
    targets: Vec<MemoryTarget>,
    requested: bool,
    error: Option<String>,
}

/// Asks each reviewed app to close normally, then waits off the UI thread to see
/// which ones really exited. A posted close request is not reported as success,
/// nothing is force-terminated, and save prompts are left for the user.
trait CloseBackend {
    fn request(&mut self, target: &MemoryTarget) -> Result<usize, String>;
    fn exited(&mut self, target: &MemoryTarget) -> bool;
}
struct WindowsClose;
impl CloseBackend for WindowsClose {
    fn request(&mut self, target: &MemoryTarget) -> Result<usize, String> {
        system::request_app_close(target.pid, target.start)
    }
    fn exited(&mut self, target: &MemoryTarget) -> bool {
        system::process_exited(target.pid, target.start)
    }
}
fn close_apps(action: MemoryAction, stop: &AtomicBool) -> MemoryReport {
    close_apps_with(action, stop, &mut WindowsClose, Duration::from_secs(15))
}
fn close_apps_with(
    action: MemoryAction,
    stop: &AtomicBool,
    backend: &mut impl CloseBackend,
    wait: Duration,
) -> MemoryReport {
    let mut report = MemoryReport {
        kind: action.kind,
        name: action.name,
        attempted: action.targets.len(),
        changed: Vec::new(),
        before: 0,
        after: 0,
        failed: Vec::new(),
        closed_apps: Vec::new(),
        open_apps: Vec::new(),
    };
    let mut apps: Vec<AppClose> = Vec::new();
    for target in action.targets {
        let result = if stop.load(Ordering::Relaxed) {
            Err("Close request canceled before this process was asked to close.".into())
        } else {
            backend.request(&target)
        };
        let index = apps
            .iter()
            .position(|a| a.name == target.app)
            .unwrap_or_else(|| {
                apps.push(AppClose {
                    name: target.app.clone(),
                    targets: Vec::new(),
                    requested: false,
                    error: None,
                });
                apps.len() - 1
            });
        let app = &mut apps[index];
        match result {
            Ok(_) => app.requested = true,
            Err(error) => {
                if app.error.is_none() {
                    app.error = Some(error);
                }
            }
        }
        app.targets.push(target);
    }
    let deadline = Instant::now() + wait;
    loop {
        let waiting = apps
            .iter()
            .filter(|a| a.requested)
            .flat_map(|a| a.targets.iter())
            .any(|t| !backend.exited(t));
        if !waiting || stop.load(Ordering::Relaxed) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    for app in apps {
        let exited = app.targets.iter().filter(|t| backend.exited(t)).count();
        if exited == app.targets.len() {
            let identities = app.targets.iter().map(|t| ((t.pid, t.start), None));
            report.changed.extend(identities);
            report.closed_apps.push(app.name);
        } else if app.requested {
            report.open_apps.push(app.name);
        } else {
            let reason = app.error.unwrap_or_else(|| "It did not respond.".into());
            report.failed.push(format!("{}: {reason}", app.name));
        }
    }
    report
}

fn close_summary(report: &MemoryReport) -> String {
    let mut parts = Vec::new();
    if !report.closed_apps.is_empty() {
        parts.push(format!("Closed {}.", list_names(&report.closed_apps)));
    }
    if !report.open_apps.is_empty() {
        parts.push(format!(
            "{} still open after the request, possibly showing a save prompt.",
            list_names(&report.open_apps)
        ));
    }
    if !report.failed.is_empty() {
        parts.push(format!(
            "{} could not be asked to close.",
            plural(report.failed.len(), "app", "apps")
        ));
    }
    if parts.is_empty() {
        "No apps were changed.".into()
    } else {
        parts.join(" ")
    }
}
fn growth_text(value: i64, observed: u64) -> String {
    if observed < 3 {
        return "measuring".into();
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
impl App {
    pub fn execute_memory(&mut self, action: MemoryAction, ctx: &egui::Context) {
        if self.mem.busy() {
            self.toast(ToastKind::Info, "Another app operation is still running.");
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.mem.stop_waiting = stop.clone();
        self.mem.closing = action.kind == MemoryKind::Close;
        self.mem.job = Task::spawn(ctx, move || {
            if action.kind == MemoryKind::Close {
                return close_apps(action, &stop);
            }
            let mut report = MemoryReport {
                kind: action.kind,
                name: action.name,
                attempted: action.targets.len(),
                changed: Vec::new(),
                before: 0,
                after: 0,
                failed: Vec::new(),
                closed_apps: Vec::new(),
                open_apps: Vec::new(),
            };
            for target in action.targets {
                let result = match action.kind {
                    MemoryKind::Close => {
                        system::request_app_close(target.pid, target.start).map(|_| None)
                    }
                    MemoryKind::Trim => {
                        system::trim_app_memory(target.pid, target.start).map(|(before, after)| {
                            report.before = report.before.saturating_add(before);
                            report.after = report.after.saturating_add(after);
                            None
                        })
                    }
                    MemoryKind::Efficiency => {
                        system::enable_app_efficiency(target.pid, target.start).map(Some)
                    }
                    MemoryKind::RestoreEfficiency => target
                        .restore
                        .ok_or_else(|| "No saved efficiency policy for this process.".to_string())
                        .and_then(|policy| {
                            system::restore_app_efficiency(target.pid, target.start, policy)
                        })
                        .map(|_| None),
                };
                match result {
                    Ok(original) => report.changed.push(((target.pid, target.start), original)),
                    Err(error) => report.failed.push(format!("PID {}: {error}", target.pid)),
                }
            }
            report
        });
    }
    pub fn poll_memory(&mut self) {
        let pressure = system::memory_pressure(
            self.sys.mem_total,
            self.sys.mem_avail,
            self.sys.commit_total,
            self.sys.commit_limit,
        );
        if self
            .mem
            .pressure_since
            .as_ref()
            .map(|(previous, _)| *previous)
            != Some(pressure)
        {
            self.mem.pressure_since = Some((pressure, Instant::now()));
        }
        if self
            .mem
            .last_identity_refresh
            .is_none_or(|last| last.elapsed().as_secs() >= 3)
        {
            let live: std::collections::HashSet<_> = self
                .sys
                .procs
                .iter()
                .map(|p| (p.pid, p.start_time))
                .collect();
            self.mem
                .efficiency
                .retain(|identity, _| live.contains(identity));
            if !self.mem.checked.is_empty() && !self.sys.procs.is_empty() {
                let names: HashSet<String> = self
                    .sys
                    .procs
                    .iter()
                    .map(|p| p.name.to_lowercase())
                    .collect();
                self.mem.checked.retain(|name| names.contains(name));
            }
            self.mem.last_identity_refresh = Some(Instant::now());
        }
        let Some(result) = self.mem.job.poll() else {
            return;
        };
        self.mem.closing = false;
        match result {
            Err(error) => self.toast(ToastKind::Error, format!("App operation failed: {error}")),
            Ok(report) => {
                let done = report.changed.len();
                for (identity, original) in &report.changed {
                    if let Some(original) = original {
                        self.mem.efficiency.entry(*identity).or_insert(*original);
                    } else if report.kind == MemoryKind::RestoreEfficiency {
                        self.mem.efficiency.remove(identity);
                    }
                }
                let message = match report.kind {
                    MemoryKind::Close => close_summary(&report),
                    MemoryKind::Trim => format!("Trimmed {done} of {} processes: resident memory fell by {} at measurement. It can grow back immediately.", report.attempted, fmt_bytes(report.before.saturating_sub(report.after))),
                    MemoryKind::Efficiency => format!("Efficiency mode enabled for {done} of {} processes for {}. Undo is available here while PCSweep stays open.", report.attempted, report.name),
                    MemoryKind::RestoreEfficiency => format!("Restored the previous efficiency policy for {done} of {} processes for {}.", report.attempted, report.name),
                };
                util::log(&message);
                for error in report.failed.iter().take(20) {
                    util::log(format!("  {error}"));
                }
                let kind = if !report.open_apps.is_empty() {
                    ToastKind::Info
                } else if done == 0 {
                    ToastKind::Error
                } else if report.failed.is_empty() {
                    ToastKind::Success
                } else {
                    ToastKind::Info
                };
                self.toast(kind, message);
                if done == 0 && report.open_apps.is_empty() {
                    if let Some(reason) = report.failed.first() {
                        self.toast(ToastKind::Error, reason.clone());
                    }
                }
                self.sys.refresh_processes_soon();
            }
        }
    }
    fn memory_targets(&self, group: &ProcGroup, kind: MemoryKind) -> Vec<MemoryTarget> {
        self.sys
            .procs
            .iter()
            .filter(|p| group.pids.contains(&p.pid))
            .filter_map(|p| {
                let restore = self.mem.efficiency.get(&(p.pid, p.start_time)).copied();
                if kind == MemoryKind::RestoreEfficiency && restore.is_none() {
                    return None;
                }
                Some(MemoryTarget {
                    pid: p.pid,
                    start: p.start_time,
                    restore,
                    app: group.name.clone(),
                })
            })
            .collect()
    }
    pub fn page_memory(&mut self, ui: &mut Ui) {
        let mut acts = Vec::new();
        self.render_memory(ui, &mut acts);
        for act in acts {
            match act {
                MAct::Details(value) => self.mem.details = value,
                MAct::Check(name, on) => {
                    if on {
                        self.mem.checked.insert(name);
                    } else {
                        self.mem.checked.remove(&name);
                    }
                }
                MAct::CheckMany(names, on) => {
                    for name in names {
                        if on {
                            self.mem.checked.insert(name);
                        } else {
                            self.mem.checked.remove(&name);
                        }
                    }
                }
                MAct::ClearChecked => self.mem.checked.clear(),
                MAct::StopWaiting => self.mem.stop_waiting.store(true, Ordering::Relaxed),
                MAct::AskCloseChecked => {
                    let groups: Vec<ProcGroup> = self
                        .sys
                        .groups()
                        .into_iter()
                        .filter(|g| self.mem.checked.contains(&g.name.to_lowercase()))
                        .filter(eligible)
                        .collect();
                    if groups.is_empty() {
                        self.toast(
                            ToastKind::Info,
                            "None of the selected apps are running now.",
                        );
                        continue;
                    }
                    let mut targets = Vec::new();
                    let mut details = Vec::new();
                    let mut ram = 0;
                    for group in &groups {
                        let group_targets = self.memory_targets(group, MemoryKind::Close);
                        details.push(format!(
                            "{}: {}, {} RAM now",
                            group.name,
                            plural(group_targets.len(), "process", "processes"),
                            fmt_bytes(group.mem)
                        ));
                        ram += group.mem;
                        targets.extend(group_targets);
                    }
                    self.modal = Some(Modal {
                        title: format!("Close {} normally?", plural(groups.len(), "app", "apps")),
                        body: format!(
                            "Windows will ask each app's windows to close, then PCSweep checks which ones really exited. Together they use {} of RAM right now; how much memory is freed depends on what Windows does next.",
                            fmt_bytes(ram)
                        ),
                        details,
                        note: Some(CLOSE_NOTE.into()),
                        confirm: "Ask apps to close".into(),
                        danger: false,
                        action: Action::Memory(MemoryAction {
                            kind: MemoryKind::Close,
                            targets,
                            name: plural(groups.len(), "app", "apps"),
                        }),
                    });
                }
                MAct::Filter(value) => self.mem.filter = value,
                MAct::Sort(value) => self.mem.sort = value,
                MAct::Select(name) => self.mem.selected = Some(name),
                MAct::Advanced(value) => self.mem.advanced = value,
                MAct::Go(page) => self.page = page,
                MAct::PageFile => {
                    let executable = util::env("SystemRoot")
                        .unwrap_or_else(|| r"C:\Windows".into())
                        .join(r"System32\SystemPropertiesPerformance.exe");
                    if let Err(error) = std::process::Command::new(executable).spawn() {
                        self.toast(
                            ToastKind::Error,
                            format!("Could not open virtual memory settings: {error}"),
                        );
                    }
                }
                MAct::AskKill(group) => {
                    let pids = self
                        .memory_targets(&group, MemoryKind::Close)
                        .into_iter()
                        .map(|p| (p.pid, p.start))
                        .collect::<Vec<_>>();
                    self.modal = Some(Modal {
                        title: format!("Force end {}?", group.name),
                        body: format!("{} processes using {} will be terminated immediately. Unsaved work can be lost. Try Close app first.", pids.len(), fmt_bytes(group.mem)),
                        details: Vec::new(), note: Some("Processes that exited, changed identity, or belong to Windows are skipped.".into()),
                        confirm: "Force end".into(), danger: true,
                        action: Action::Kill { pids, name: group.name },
                    });
                }
                MAct::Ask(kind, group) => {
                    let targets = self.memory_targets(&group, kind);
                    let (title, body, note, confirm) = match kind {
                        MemoryKind::Close => (format!("Close {} normally?", group.name),
                            "Windows will ask the app's windows to close. The app can show a save prompt or refuse; background services may have no windows to close.",
                            "Review any save prompts in the app. This does not force termination.", "Ask app to close"),
                        MemoryKind::Trim => (format!("Trim resident RAM for {}?", group.name),
                            "Windows will move eligible pages out of this app's working set. This can lower its resident RAM temporarily, and the next access may need to reload pages.",
                            "This does not fix leaks or reduce committed memory. It can cause stutter or disk activity. There is no automatic or scheduled trimming.", "Trim selected app"),
                        MemoryKind::Efficiency => (format!("Use efficiency mode for {}?", group.name),
                            "Use Windows' power-efficient execution policy for these background processes. This may reduce CPU interference, heat and battery use, and may slow the selected app.",
                            "Avoid games, calls and time-sensitive work. Applies to current processes only. Restore their previous policy here while PCSweep remains open.", "Enable efficiency"),
                        MemoryKind::RestoreEfficiency => (format!("Undo efficiency mode for {}?", group.name),
                            "Restore the execution-speed policy these processes had before PCSweep enabled efficiency mode.",
                            "Only the same process lifetimes are changed. Restarted apps already use their own default policy.", "Restore previous policy"),
                    };
                    self.modal = Some(Modal {
                        title,
                        body: body.into(),
                        details: vec![format!("{} current process(es)", targets.len())],
                        note: Some(note.into()),
                        confirm: confirm.into(),
                        danger: false,
                        action: Action::Memory(MemoryAction {
                            kind,
                            targets,
                            name: group.name,
                        }),
                    });
                }
            }
        }
    }
    fn render_memory(&self, ui: &mut Ui, acts: &mut Vec<MAct>) {
        let s = &self.sys;
        let pressure =
            system::memory_pressure(s.mem_total, s.mem_avail, s.commit_total, s.commit_limit);
        let color = match pressure {
            MemoryPressure::Normal => GOOD,
            MemoryPressure::Elevated => WARN,
            MemoryPressure::Critical => BAD,
        };
        widgets::page_header(ui, "Memory and running apps", "See actual memory pressure, find growing apps, and keep background work under control.");
        self.render_memory_summary(ui, color, acts);
        if let Some(warning) = &s.process_warning {
            ui.colored_label(WARN, warning);
        }
        if pressure != MemoryPressure::Normal {
            ui.add_space(4.0);
            widgets::banner(
                ui,
                color,
                if pressure == MemoryPressure::Normal {
                    ic::CHECK
                } else {
                    ic::WARN
                },
                |ui| {
                    let seconds = self
                        .mem
                        .pressure_since
                        .as_ref()
                        .map_or(0, |(_, t)| t.elapsed().as_secs());
                    let message = match pressure {
                    MemoryPressure::Normal => "Memory has room to work. Cached RAM is useful and can be reused automatically.".to_string(),
                    MemoryPressure::Elevated => "Memory is getting tight. Close an app you are not using, and check its startup settings if it keeps returning.".to_string(),
                    MemoryPressure::Critical if seconds >= 10 => "Memory pressure has stayed high. Save your work and close a heavy app. Near the commit limit, apps can fail to allocate memory.".to_string(),
                    MemoryPressure::Critical => "Memory is very tight right now. Watch for sustained pressure rather than clearing cache on a timer.".to_string(),
                };
                    ui.vertical(|ui| {
                        ui.label(message);
                        ui.horizontal_wrapped(|ui| {
                            if ui.add(widgets::small("Manage startup apps")).clicked() {
                                acts.push(MAct::Go(Page::Startup));
                            }
                            ui.label(widgets::faint(
                                "Private commit is different from resident RAM and page-file usage.",
                            ));
                        });
                    });
                },
            );
        }
        ui.add_space(6.0);
        let mut groups = s.groups();
        match self.mem.sort {
            MemSort::Resident => {}
            MemSort::Private => groups.sort_by_key(|g| std::cmp::Reverse(g.private_bytes)),
            MemSort::Growth => groups.sort_by_key(|g| std::cmp::Reverse(g.growth)),
            MemSort::Cpu => groups.sort_by(|a, b| b.cpu.total_cmp(&a.cpu)),
        }
        if let Some(selected) = self
            .mem
            .selected
            .as_ref()
            .and_then(|name| groups.iter().find(|g| g.name.eq_ignore_ascii_case(name)))
        {
            self.render_selected_memory(ui, selected, acts);
            ui.add_space(6.0);
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(widgets::dim("Sort"));
            for (sort, label) in [
                (MemSort::Resident, "RAM"),
                (MemSort::Private, "Private commit"),
                (MemSort::Growth, "Growth"),
                (MemSort::Cpu, "CPU"),
            ] {
                if ui
                    .add(widgets::chip(label, self.mem.sort == sort))
                    .clicked()
                {
                    acts.push(MAct::Sort(sort));
                }
            }
            let mut filter = self.mem.filter.clone();
            if widgets::search_box(ui, &mut filter, "Search apps", 180.0).changed() {
                acts.push(MAct::Filter(filter));
            }
        });
        let filter = self.mem.filter.trim().to_lowercase();
        let shown: Vec<&ProcGroup> = groups
            .iter()
            .filter(|g| filter.is_empty() || g.name.to_lowercase().contains(&filter))
            .collect();
        self.render_selection_bar(ui, &groups, &shown, acts);
        ui.label(widgets::faint(format!(
            "{} app groups, {} processes. Refreshed every 3 seconds; growth is observed for up to 10 minutes. Tick apps to close several at once, or click a name for more actions.",
            shown.len(),
            fmt_num(s.procs.len() as u64)
        )));
        if shown.is_empty() {
            ui.label(widgets::dim(
                "No apps match. Try part of an executable name.",
            ));
            return;
        }
        let selectable: Vec<String> = shown
            .iter()
            .filter(|g| eligible(g))
            .map(|g| g.name.to_lowercase())
            .collect();
        ui.horizontal(|ui| {
            let all =
                !selectable.is_empty() && selectable.iter().all(|n| self.mem.checked.contains(n));
            let mut value = all;
            let header = ui
                .add_enabled(!selectable.is_empty(), egui::Checkbox::new(&mut value, ""))
                .on_hover_text("Select every app shown that can be closed");
            if header.changed() {
                acts.push(MAct::CheckMany(selectable.clone(), value));
            }
            ui.label(widgets::faint("App"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (width, label) in [
                    (68.0, "CPU"),
                    (90.0, "RAM"),
                    (96.0, "Private"),
                    (98.0, "Growth"),
                ] {
                    widgets::cell(ui, width, true, widgets::faint(label));
                }
            });
        });
        ScrollArea::vertical()
            .id_salt("procs_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, 42.0, shown.len(), |ui, range| {
                for group in &shown[range] {
                    self.render_memory_row(ui, group, acts);
                }
            });
    }

    fn render_memory_summary(&self, ui: &mut Ui, color: egui::Color32, acts: &mut Vec<MAct>) {
        let s = &self.sys;
        widgets::card(ui, |ui| {
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

    fn render_selection_bar(
        &self,
        ui: &mut Ui,
        groups: &[ProcGroup],
        shown: &[&ProcGroup],
        acts: &mut Vec<MAct>,
    ) {
        let busy = self.mem.busy() && self.mem.closing;
        let checked: Vec<&ProcGroup> = groups
            .iter()
            .filter(|g| self.mem.checked.contains(&g.name.to_lowercase()))
            .collect();
        if checked.is_empty() && !busy {
            return;
        }
        let processes: usize = checked.iter().map(|g| g.count).sum();
        let ram: u64 = checked.iter().map(|g| g.mem).sum();
        let hidden = checked
            .iter()
            .filter(|g| !shown.iter().any(|v| v.name == g.name))
            .count();
        widgets::tight_frame().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                if busy {
                    ui.spinner();
                    ui.label(widgets::dim("Waiting to see which apps close…"));
                } else {
                    ui.label(widgets::strong(
                        format!("{} selected", plural(checked.len(), "app", "apps")),
                        14.5,
                    ));
                    ui.label(widgets::dim(format!(
                        "{}, using {} RAM right now",
                        plural(processes, "process", "processes"),
                        fmt_bytes(ram)
                    )));
                    if hidden > 0 {
                        ui.label(widgets::faint(format!("{hidden} hidden by the search")));
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if busy {
                        if ui.add(widgets::small("Stop waiting")).clicked() {
                            acts.push(MAct::StopWaiting);
                        }
                    } else {
                        let close = widgets::primary("Close selected…");
                        if ui.add_enabled(!self.mem.busy(), close).clicked() {
                            acts.push(MAct::AskCloseChecked);
                        }
                        if ui.add(widgets::small("Clear")).clicked() {
                            acts.push(MAct::ClearChecked);
                        }
                    }
                });
            });
        });
        ui.add_space(4.0);
    }

    fn render_memory_row(&self, ui: &mut Ui, group: &ProcGroup, acts: &mut Vec<MAct>) {
        let key = group.name.to_lowercase();
        widgets::row(ui, 42.0, |ui| {
            let can_close = eligible(group);
            let mut checked = self.mem.checked.contains(&key);
            let response = ui.add_enabled(can_close, egui::Checkbox::new(&mut checked, ""));
            if !can_close {
                response.on_disabled_hover_text(PROTECTED_HINT);
            } else if response.changed() {
                acts.push(MAct::Check(key.clone(), checked));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                widgets::cell(ui, 68.0, true, widgets::faint(format!("{:.1}%", group.cpu)));
                widgets::cell(ui, 90.0, true, widgets::strong(fmt_bytes(group.mem), 14.0));
                widgets::cell(ui, 96.0, true, widgets::dim(private_text(group)))
                    .on_hover_text(PRIVATE_HINT);
                let growth = if group.private_known == 0 {
                    RichText::new("unavailable").color(TEXT_FAINT)
                } else {
                    let color = if group.growth > 128 * 1024 * 1024 {
                        WARN
                    } else {
                        TEXT_DIM
                    };
                    let partial = if group.private_known < group.count {
                        "~"
                    } else {
                        ""
                    };
                    let text = growth_text(group.growth, group.observed_secs);
                    RichText::new(format!("{partial}{text}")).color(color)
                };
                let hint = format!(
                    "Change in private committed memory over {} seconds observed. Growth alone does not prove a leak.",
                    group.observed_secs
                );
                widgets::cell(ui, 98.0, true, growth).on_hover_text(hint);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let selected = self
                        .mem
                        .selected
                        .as_ref()
                        .is_some_and(|name| name.eq_ignore_ascii_case(&group.name));
                    let label = if group.count > 1 {
                        format!("{} ({})", group.name, group.count)
                    } else {
                        group.name.clone()
                    };
                    let button = egui::Button::new(RichText::new(label).color(TEXT))
                        .selected(selected)
                        .frame(false)
                        .truncate();
                    if ui.add(button).on_hover_text(&group.name).clicked() {
                        acts.push(MAct::Select(group.name.clone()));
                    }
                });
            });
        });
    }

    fn render_selected_memory(&self, ui: &mut Ui, group: &ProcGroup, acts: &mut Vec<MAct>) {
        let protected =
            system::is_critical(&group.name) || group.pids.contains(&std::process::id());
        let enabled = !protected && !self.mem.busy();
        let has_undo = self.sys.procs.iter().any(|p| {
            group.pids.contains(&p.pid) && self.mem.efficiency.contains_key(&(p.pid, p.start_time))
        });
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::icon(
                    if protected { ic::LOCK } else { ic::APPS },
                    18.0,
                    ACCENT,
                ));
                ui.label(widgets::strong(&group.name, 17.0));
                ui.label(widgets::dim(format!(
                    "{} RAM · {} process(es)",
                    fmt_bytes(group.mem),
                    group.count
                )));
                if has_undo {
                    widgets::badge(ui, "Efficiency set by PCSweep", GOOD);
                }
                if self.mem.busy() {
                    ui.spinner();
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(enabled, widgets::primary("Close app"))
                    .clicked()
                {
                    acts.push(MAct::Ask(MemoryKind::Close, group.clone()));
                }
                if ui
                    .add_enabled(
                        enabled,
                        widgets::ghost(if has_undo {
                            "Undo efficiency"
                        } else {
                            "Efficiency mode"
                        }),
                    )
                    .clicked()
                {
                    acts.push(MAct::Ask(
                        if has_undo {
                            MemoryKind::RestoreEfficiency
                        } else {
                            MemoryKind::Efficiency
                        },
                        group.clone(),
                    ));
                }
                if ui
                    .add_enabled(enabled, widgets::ghost("Force end…"))
                    .clicked()
                {
                    acts.push(MAct::AskKill(group.clone()));
                }
                let mut advanced = self.mem.advanced;
                if ui.checkbox(&mut advanced, "Advanced").changed() {
                    acts.push(MAct::Advanced(advanced));
                }
            });
            if protected {
                ui.label(widgets::faint(
                    "Windows, security and PCSweep processes are protected from these actions.",
                ));
            } else {
                ui.label(widgets::faint("Close requests preserve the app's chance to save. Efficiency affects CPU/power, not memory allocation."));
            }
            if self.mem.advanced {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(enabled, widgets::small("Trim this app's resident RAM…")).clicked() { acts.push(MAct::Ask(MemoryKind::Trim, group.clone())); }
                    ui.label(widgets::faint("Temporary only. Reloading pages can cause stutter; private commit is unchanged."));
                });
            }
        });
    }
}
#[cfg(test)]
pub(crate) fn preview_fixture() -> MemState {
    let mut state = MemState {
        selected: Some("chrome.exe".into()),
        advanced: true,
        ..MemState::default()
    };
    state.checked.insert("chrome.exe".into());
    state.efficiency.insert(
        (7001, 10),
        PowerPolicy {
            control: 0,
            state: 0,
        },
    );
    state
}
#[cfg(test)]
pub(crate) fn bulk_preview_fixture() -> MemState {
    let mut state = MemState::default();
    state.checked.insert("chrome.exe".into());
    state.checked.insert("code.exe".into());
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClose {
        calls: Vec<(u32, u64)>,
        exits: HashSet<(u32, u64)>,
        denied: HashSet<u32>,
    }
    impl CloseBackend for FakeClose {
        fn request(&mut self, target: &MemoryTarget) -> Result<usize, String> {
            self.calls.push((target.pid, target.start));
            if self.denied.contains(&target.pid) {
                Err("Identity changed or protected".into())
            } else {
                Ok(1)
            }
        }
        fn exited(&mut self, target: &MemoryTarget) -> bool {
            self.exits.contains(&(target.pid, target.start))
        }
    }
    fn close_action() -> MemoryAction {
        MemoryAction {
            kind: MemoryKind::Close,
            name: "3 apps".into(),
            targets: (1..=3)
                .map(|pid| MemoryTarget {
                    pid,
                    start: 10,
                    restore: None,
                    app: format!("app{pid}.exe"),
                })
                .collect(),
        }
    }
    #[test]
    fn bulk_close_uses_reviewed_identities_and_reports_verified_results() {
        let mut backend = FakeClose {
            calls: vec![],
            exits: [(1, 10)].into(),
            denied: [3].into(),
        };
        let report = close_apps_with(
            close_action(),
            &AtomicBool::new(false),
            &mut backend,
            Duration::ZERO,
        );
        assert_eq!(backend.calls, vec![(1, 10), (2, 10), (3, 10)]);
        assert_eq!(report.closed_apps, vec!["app1.exe"]);
        assert_eq!(report.open_apps, vec!["app2.exe"]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.changed.len(), 1);
        assert!(report.failed[0].contains("Identity changed"));
    }
    #[test]
    fn canceled_bulk_close_never_sends_remaining_requests() {
        let mut backend = FakeClose {
            calls: vec![],
            exits: HashSet::new(),
            denied: HashSet::new(),
        };
        let report = close_apps_with(
            close_action(),
            &AtomicBool::new(true),
            &mut backend,
            Duration::ZERO,
        );
        assert!(backend.calls.is_empty());
        assert!(report.closed_apps.is_empty());
        assert_eq!(report.failed.len(), 3);
    }

    fn closing_report(closed: &[&str], open: &[&str], failed: &[&str]) -> MemoryReport {
        MemoryReport {
            kind: MemoryKind::Close,
            name: "apps".into(),
            attempted: closed.len() + open.len() + failed.len(),
            changed: Vec::new(),
            before: 0,
            after: 0,
            failed: failed.iter().map(|s| s.to_string()).collect(),
            closed_apps: closed.iter().map(|s| s.to_string()).collect(),
            open_apps: open.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn close_summary_reports_only_verified_exits_as_closed() {
        let text = close_summary(&closing_report(
            &["a.exe"],
            &["b.exe"],
            &["c.exe: no windows"],
        ));
        assert!(text.contains("Closed a.exe."));
        assert!(text.contains("b.exe still open"));
        assert!(text.contains("1 app could not be asked to close."));
        let none = close_summary(&closing_report(&[], &[], &[]));
        assert_eq!(none, "No apps were changed.");
    }

    #[test]
    fn long_name_lists_stay_readable() {
        let names: Vec<String> = (1..=5).map(|i| format!("app{i}.exe")).collect();
        assert_eq!(list_names(&names[..2]), "app1.exe, app2.exe");
        assert_eq!(list_names(&names), "app1.exe, app2.exe and 3 more");
    }

    #[test]
    fn unreadable_private_memory_is_shown_as_unavailable_not_zero() {
        let mut group = ProcGroup {
            name: "elevated.exe".into(),
            count: 2,
            mem: 1024,
            cpu: 0.0,
            private_bytes: 0,
            private_known: 0,
            growth: 0,
            observed_secs: 0,
            pids: vec![10, 11],
        };
        assert_eq!(private_text(&group), "unavailable");
        group.private_known = 1;
        group.private_bytes = 2048;
        assert_eq!(private_text(&group), "~2.00 KB");
    }

    #[test]
    fn protected_and_own_processes_cannot_be_selected() {
        let mut group = ProcGroup {
            name: "explorer.exe".into(),
            count: 1,
            mem: 1,
            cpu: 0.0,
            private_bytes: 0,
            private_known: 0,
            growth: 0,
            observed_secs: 0,
            pids: vec![42],
        };
        assert!(!eligible(&group));
        group.name = "notepad.exe".into();
        assert!(eligible(&group));
        group.pids.push(std::process::id());
        assert!(!eligible(&group));
    }
    #[test]
    fn memory_growth_preserves_sign_and_waits_for_observation() {
        assert_eq!(growth_text(4, 0), "measuring");
        assert_eq!(growth_text(1024, 4), "+1.00 KB");
        assert_eq!(growth_text(-1024, 4), "−1.00 KB");
        assert_eq!(growth_text(0, 4), "0 B");
    }
}
