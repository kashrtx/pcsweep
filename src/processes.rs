use crate::app::{Action, App, Modal, Page, ToastKind};
use crate::system::{self, MemoryPressure, PowerPolicy, ProcGroup};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Layout, RichText, ScrollArea, Ui};
use std::collections::HashMap;
use std::time::Instant;

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
        self.mem.job = Task::spawn(ctx, move || {
            let mut report = MemoryReport {
                kind: action.kind,
                name: action.name,
                attempted: action.targets.len(),
                changed: Vec::new(),
                before: 0,
                after: 0,
                failed: Vec::new(),
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
            self.mem.last_identity_refresh = Some(Instant::now());
        }
        let Some(result) = self.mem.job.poll() else {
            return;
        };
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
                    MemoryKind::Close => format!("Asked {done} of {} processes for {} to close. Save prompts may still be open.", report.attempted, report.name),
                    MemoryKind::Trim => format!("Trimmed {done} of {} processes: resident memory fell by {} at measurement. It can grow back immediately.", report.attempted, fmt_bytes(report.before.saturating_sub(report.after))),
                    MemoryKind::Efficiency => format!("Efficiency mode enabled for {done} of {} processes for {}. Undo is available here while PCSweep stays open.", report.attempted, report.name),
                    MemoryKind::RestoreEfficiency => format!("Restored the previous efficiency policy for {done} of {} processes for {}.", report.attempted, report.name),
                };
                util::log(&message);
                for error in report.failed.iter().take(20) {
                    util::log(format!("  {error}"));
                }
                let kind = if done == 0 {
                    ToastKind::Error
                } else if report.failed.is_empty() {
                    ToastKind::Success
                } else {
                    ToastKind::Info
                };
                self.toast(kind, message);
                if done == 0 {
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
                })
            })
            .collect()
    }
    pub fn page_memory(&mut self, ui: &mut Ui) {
        let mut acts = Vec::new();
        self.render_memory(ui, &mut acts);
        for act in acts {
            match act {
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
        ui.columns(2, |cols| {
            widgets::card(&mut cols[0], |ui| {
                ui.horizontal(|ui| {
                    widgets::ring(ui, s.mem_frac(), 96.0, 9.0, color, &format!("{:.0}%", s.mem_frac() * 100.0), "RAM in use");
                    ui.vertical(|ui| {
                        ui.label(widgets::strong(format!("{} available", fmt_bytes(s.mem_avail)), 16.0));
                        ui.label(widgets::dim(format!("{} in use / {} total", fmt_bytes(s.mem_used), fmt_bytes(s.mem_total))));
                        if s.memory_info_available {
                            ui.label(widgets::faint(format!("System cache: {}", fmt_bytes(s.system_cache)))).on_hover_text("Cache overlaps available memory; these amounts must not be added together. Windows reuses standby cache when apps need RAM.");
                        }
                        ui.label(widgets::faint(format!("CPU {:.0}% · {} logical cores", s.cpu, s.cores))).on_hover_text(&s.cpu_name);
                    });
                });
            });
            widgets::card(&mut cols[1], |ui| {
                ui.label(widgets::strong("Committed memory", 16.0));
                if s.memory_info_available {
                    ui.label(widgets::dim(format!("{} / {} limit", fmt_bytes(s.commit_total), fmt_bytes(s.commit_limit))));
                    let fraction = if s.commit_limit > 0 { s.commit_total as f32 / s.commit_limit as f32 } else { 0.0 };
                    widgets::bar(ui, fraction, color, egui::vec2(ui.available_width(), 8.0));
                    ui.label(widgets::faint(format!("Peak since boot: {}", fmt_bytes(s.commit_peak))));
                } else { ui.label(widgets::dim("Windows memory counters are unavailable.")); }
                if ui.add(widgets::small("Virtual memory settings")).clicked() { acts.push(MAct::PageFile); }
                ui.label(widgets::faint("Advanced → Virtual memory. Let Windows manage sizing."));
            });
        });
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
                ui.label(message);
                ui.horizontal_wrapped(|ui| {
                if ui.add(widgets::small("Manage startup apps")).clicked() { acts.push(MAct::Go(Page::Startup)); }
                ui.label(widgets::faint("Private commit can grow without resident RAM growing. It is not the same as page-file usage."));
            });
            },
        );
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
        } else {
            ui.label(widgets::dim("Select an app below to close it normally, use efficiency mode, or review advanced controls."));
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
        groups.retain(|g| filter.is_empty() || g.name.to_lowercase().contains(&filter));
        ui.label(widgets::faint(format!("{} app groups · {} processes · refreshed every 3 seconds · growth observed for up to 10 minutes", groups.len(), fmt_num(s.procs.len() as u64))));
        if groups.is_empty() {
            ui.label(widgets::dim(
                "No apps match. Try part of an executable name.",
            ));
            return;
        }
        ui.horizontal(|ui| {
            ui.label(widgets::faint("App · select for actions"));
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
        ScrollArea::vertical().id_salt("procs_scroll").auto_shrink([false, false]).show_rows(ui, 42.0, groups.len(), |ui, range| {
            for group in &groups[range] {
                widgets::row(ui, 42.0, |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        widgets::cell(ui, 68.0, true, widgets::faint(format!("{:.1}%", group.cpu)));
                        widgets::cell(ui, 90.0, true, widgets::strong(fmt_bytes(group.mem), 14.0));
                        let private = if group.private_known == 0 { "—".into() } else { format!("{}{}", if group.private_known < group.count { "~" } else { "" }, fmt_bytes(group.private_bytes)) };
                        widgets::cell(ui, 96.0, true, widgets::dim(private));
                        let change_color = if group.growth > 128 * 1024 * 1024 { WARN } else { TEXT_DIM };
                        widgets::cell(ui, 98.0, true, RichText::new(growth_text(group.growth, group.observed_secs)).color(change_color)).on_hover_text(format!("Change in private committed memory over {} seconds observed. Growth alone does not prove a leak.", group.observed_secs));
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let selected = self.mem.selected.as_ref().is_some_and(|name| name.eq_ignore_ascii_case(&group.name));
                            let label = if group.count > 1 { format!("{} ({})", group.name, group.count) } else { group.name.clone() };
                            if ui.add(egui::Button::new(RichText::new(label).color(TEXT)).selected(selected).frame(false).truncate()).on_hover_text(&group.name).clicked() { acts.push(MAct::Select(group.name.clone())); }
                        });
                    });
                });
            }
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
mod tests {
    use super::growth_text;
    #[test]
    fn memory_growth_preserves_sign_and_waits_for_observation() {
        assert_eq!(growth_text(4, 0), "measuring");
        assert_eq!(growth_text(1024, 4), "+1.00 KB");
        assert_eq!(growth_text(-1024, 4), "−1.00 KB");
        assert_eq!(growth_text(0, 4), "0 B");
    }
}
