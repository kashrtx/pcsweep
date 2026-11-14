use crate::app::{Action, App, Modal, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use chrono::NaiveDate;
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::process::Command;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

#[derive(Clone)]
pub struct InstalledApp {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub size: u64,
    pub installed: Option<NaiveDate>,
    pub uninstall: String,
    pub scope: &'static str,
}

pub fn list() -> Vec<InstalledApp> {
    let mut out: Vec<InstalledApp> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let sources = [
        (true, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall", "All users"),
        (true, r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall", "All users"),
        (false, r"Software\Microsoft\Windows\CurrentVersion\Uninstall", "Your account"),
    ];
    for (machine, path, scope) in sources {
        let root = RegKey::predef(if machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER });
        let Ok(key) = root.open_subkey(path) else { continue };
        for sub in key.enum_keys().flatten() {
            let Ok(k) = key.open_subkey(&sub) else { continue };
            let name: String = k.get_value("DisplayName").unwrap_or_default();
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            if k.get_value::<u32, _>("SystemComponent").unwrap_or(0) == 1 {
                continue;
            }
            let parent: String = k.get_value("ParentKeyName").unwrap_or_default();
            if !parent.is_empty() {
                continue;
            }
            let release: String = k.get_value("ReleaseType").unwrap_or_default();
            if matches!(release.as_str(), "Update" | "Hotfix" | "Security Update") {
                continue;
            }
            let version: String = k.get_value("DisplayVersion").unwrap_or_default();
            if !seen.insert(format!("{}|{}", name.to_lowercase(), version)) {
                continue;
            }
            let publisher: String = k.get_value("Publisher").unwrap_or_default();
            let size = k.get_value::<u32, _>("EstimatedSize").unwrap_or(0) as u64 * 1024;
            let date: String = k.get_value("InstallDate").unwrap_or_default();
            let installed = NaiveDate::parse_from_str(date.trim(), "%Y%m%d").ok();
            let uninstall: String = k
                .get_value::<String, _>("UninstallString")
                .unwrap_or_default()
                .trim()
                .to_string();
            out.push(InstalledApp {
                name,
                version: version.trim().to_string(),
                publisher: publisher.trim().to_string(),
                size,
                installed,
                uninstall,
                scope,
            });
        }
    }
    out
}

/// Runs the app's own uninstaller, which shows its own confirmation.
pub fn launch_uninstaller(cmd: &str) -> std::io::Result<()> {
    let mut c = cmd.trim().to_string();
    let lower = c.to_lowercase();
    // MsiExec /I opens "modify"; /X goes straight to removal.
    if lower.starts_with("msiexec") && lower.len() == c.len() {
        if let Some(pos) = lower.find("/i{").or_else(|| lower.find("/i {")) {
            c.replace_range(pos..pos + 2, "/X");
        }
    }
    Command::new("cmd.exe")
        .raw_arg(format!("/S /C \"{}\"", c))
        .creation_flags(util::CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppSort {
    Size,
    Name,
    Oldest,
}

pub struct AppsState {
    pub list: Vec<InstalledApp>,
    pub load: Task<Vec<InstalledApp>>,
    pub sort: AppSort,
    pub filter: String,
}

impl Default for AppsState {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            load: Task::default(),
            sort: AppSort::Size,
            filter: String::new(),
        }
    }
}

enum PAct {
    Sort(AppSort),
    Filter(String),
    Uninstall(usize),
    Reload,
    Settings,
}

impl App {
    pub fn start_apps_load(&mut self, ctx: &egui::Context) {
        if !self.apps.load.running() {
            self.apps.load = Task::spawn(ctx, list);
        }
    }

    pub fn poll_apps(&mut self) {
        if let Some(r) = self.apps.load.poll() {
            match r {
                Ok(v) => self.apps.list = v,
                Err(e) => self.toast(ToastKind::Error, format!("Couldn't read installed apps: {e}")),
            }
        }
    }

    pub fn launch_uninstall(&mut self, i: usize) {
        let Some(app) = self.apps.list.get(i).cloned() else { return };
        match launch_uninstaller(&app.uninstall) {
            Ok(()) => {
                util::log(format!("Started the uninstaller for {}", app.name));
                self.toast(ToastKind::Info, format!("{}'s uninstaller is starting. Refresh the list when it's done.", app.name));
            }
            Err(e) => self.toast(ToastKind::Error, format!("Couldn't start the uninstaller: {e}")),
        }
    }

    pub fn page_apps(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        self.render_apps(ui, &mut acts);
        for a in acts {
            match a {
                PAct::Sort(s) => self.apps.sort = s,
                PAct::Filter(f) => self.apps.filter = f,
                PAct::Reload => self.start_apps_load(&ctx),
                PAct::Settings => util::open_uri("ms-settings:appsfeatures"),
                PAct::Uninstall(i) => {
                    if let Some(app) = self.apps.list.get(i) {
                        let name = app.name.clone();
                        self.modal = Some(Modal {
                            title: format!("Uninstall {name}?"),
                            body: format!(
                                "PCSweep will start {name}'s own uninstaller. It will ask you to confirm and walk you through removal."
                            ),
                            details: Vec::new(),
                            note: None,
                            confirm: "Start uninstaller".into(),
                            danger: true,
                            action: Action::Uninstall(i),
                        });
                    }
                }
            }
        }
    }

    fn render_apps(&self, ui: &mut Ui, acts: &mut Vec<PAct>) {
        let s = &self.apps;
        widgets::page_header(
            ui,
            "Installed apps",
            "Big programs you no longer use are often the easiest space to win back.",
        );
        let total: u64 = s.list.iter().map(|a| a.size).sum();
        ui.horizontal(|ui| {
            ui.label(RichText::new(fmt_num(s.list.len() as u64)).font(widgets::light(30.0)).color(TEXT));
            ui.label(widgets::dim(format!("apps using about {}", fmt_bytes(total))));
            ui.add_space(16.0);
            let mut f = s.filter.clone();
            if widgets::search_box(ui, &mut f, "Search apps or publishers", 220.0).changed() {
                acts.push(PAct::Filter(f));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(widgets::small(widgets::icon_text(ic::REFRESH, "Refresh", 13.0, TEXT))).clicked() {
                    acts.push(PAct::Reload);
                }
                if ui.add(widgets::small("Windows installed apps")).clicked() {
                    acts.push(PAct::Settings);
                }
                if s.load.running() {
                    ui.spinner();
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(widgets::dim("Sort by"));
            for (srt, label) in [(AppSort::Size, "Size"), (AppSort::Name, "Name"), (AppSort::Oldest, "Install date")] {
                if ui.add(widgets::chip(label, s.sort == srt)).clicked() {
                    acts.push(PAct::Sort(srt));
                }
            }
        });
        ui.label(widgets::faint(
            "Sizes are what each app reports to Windows and can be missing or approximate. Microsoft Store apps are managed in Windows settings.",
        ));
        ui.add_space(4.0);

        let filter = s.filter.to_lowercase();
        let mut idx: Vec<usize> = (0..s.list.len())
            .filter(|&i| {
                filter.is_empty()
                    || s.list[i].name.to_lowercase().contains(&filter)
                    || s.list[i].publisher.to_lowercase().contains(&filter)
            })
            .collect();
        match s.sort {
            AppSort::Size => idx.sort_by(|&a, &b| s.list[b].size.cmp(&s.list[a].size)),
            AppSort::Name => idx.sort_by(|&a, &b| s.list[a].name.to_lowercase().cmp(&s.list[b].name.to_lowercase())),
            AppSort::Oldest => idx.sort_by(|&a, &b| match (s.list[a].installed, s.list[b].installed) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }),
        }
        let top = s.list.iter().map(|a| a.size).max().unwrap_or(1).max(1) as f32;
        let today = chrono::Local::now().date_naive();

        ScrollArea::vertical()
            .id_source("apps_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, 48.0, idx.len(), |ui, range| {
                for &i in &idx[range] {
                    let app = &s.list[i];
                    widgets::row(ui, 48.0, |ui| {
                        ui.label(widgets::icon(ic::APPS, 18.0, ACCENT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            if !app.uninstall.is_empty() && ui.add(widgets::small("Uninstall")).clicked() {
                                acts.push(PAct::Uninstall(i));
                            }
                            let age = app
                                .installed
                                .map(|d| {
                                    let days = (today - d).num_days().max(0);
                                    if days > 365 {
                                        format!("installed {:.0} years ago", (days as f64 / 365.25).floor().max(1.0))
                                    } else if days > 60 {
                                        format!("installed {} months ago", days / 30)
                                    } else {
                                        format!("installed {}", d.format("%b %d, %Y"))
                                    }
                                })
                                .unwrap_or_default();
                            ui.add_sized([150.0, 20.0], Label::new(widgets::faint(age)));
                            let size = if app.size > 0 { fmt_bytes(app.size) } else { "unknown".into() };
                            ui.add_sized([80.0, 20.0], Label::new(widgets::strong(size, 14.0)));
                            widgets::bar(ui, app.size as f32 / top, ACCENT_STRONG, egui::vec2(90.0, 6.0));
                            ui.add_space(8.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.add(Label::new(widgets::strong(&app.name, 14.5)).truncate());
                                    let mut meta = Vec::new();
                                    if !app.publisher.is_empty() {
                                        meta.push(app.publisher.clone());
                                    }
                                    if !app.version.is_empty() {
                                        meta.push(format!("version {}", app.version));
                                    }
                                    meta.push(app.scope.to_string());
                                    ui.add(Label::new(widgets::faint(meta.join(", "))).truncate());
                                });
                            });
                        });
                    });
                }
            });
    }
}
