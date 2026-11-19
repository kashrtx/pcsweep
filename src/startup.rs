use crate::app::{App, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util;
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use std::fs;
use std::path::PathBuf;
use winreg::enums::{RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::{RegKey, RegValue};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN32: &str = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_RUN: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const APPROVED_RUN32: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32";
const APPROVED_FOLDER: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";
pub(crate) const STARTUP_INFO: &str = "Turning an app off only stops it launching at sign-in. It stays installed, and you can turn it back on any time. This is the same switch Task Manager uses.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Loc {
    UserRun,
    MachineRun,
    MachineRun32,
    UserFolder,
    CommonFolder,
}

impl Loc {
    pub fn label(self) -> &'static str {
        match self {
            Loc::UserRun => "Your account",
            Loc::MachineRun | Loc::MachineRun32 => "All users",
            Loc::UserFolder => "Startup folder",
            Loc::CommonFolder => "Startup folder, all users",
        }
    }
    pub fn machine(self) -> bool {
        matches!(
            self,
            Loc::MachineRun | Loc::MachineRun32 | Loc::CommonFolder
        )
    }
    fn approved_key(self) -> &'static str {
        match self {
            Loc::UserRun | Loc::MachineRun => APPROVED_RUN,
            Loc::MachineRun32 => APPROVED_RUN32,
            Loc::UserFolder | Loc::CommonFolder => APPROVED_FOLDER,
        }
    }
}

#[derive(Clone)]
pub struct StartupItem {
    /// Registry value name or startup-folder file name.
    pub key: String,
    pub display: String,
    pub command: String,
    pub loc: Loc,
    pub enabled: bool,
    pub file: Option<PathBuf>,
}

fn root(machine: bool) -> RegKey {
    RegKey::predef(if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    })
}

/// Task Manager's convention: an odd first byte means "disabled".
fn approved(machine: bool, sub: &str, name: &str) -> bool {
    let Ok(key) = root(machine).open_subkey(sub) else {
        return true;
    };
    match key.get_raw_value(name) {
        Ok(v) => v.bytes.first().map_or(true, |b| b & 1 == 0),
        Err(_) => true,
    }
}

pub fn list() -> Vec<StartupItem> {
    let mut out = Vec::new();
    for (loc, path) in [
        (Loc::UserRun, RUN),
        (Loc::MachineRun, RUN),
        (Loc::MachineRun32, RUN32),
    ] {
        let machine = loc.machine();
        let Ok(k) = root(machine).open_subkey(path) else {
            continue;
        };
        for (name, _) in k.enum_values().flatten() {
            if name.is_empty() {
                continue;
            }
            let command: String = k.get_value(&name).unwrap_or_default();
            if command.trim().is_empty() {
                continue;
            }
            out.push(StartupItem {
                enabled: approved(machine, loc.approved_key(), &name),
                display: name.clone(),
                key: name,
                file: exe_from_command(&command),
                command,
                loc,
            });
        }
    }
    let folders = [
        (
            Loc::UserFolder,
            util::env("APPDATA").map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs\Startup")),
        ),
        (
            Loc::CommonFolder,
            util::env("ProgramData")
                .map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs\StartUp")),
        ),
    ];
    for (loc, dir) in folders {
        let Some(dir) = dir else { continue };
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            // Folders here (like Autoruns' "AutorunsDisabled") don't launch anything.
            if e.file_type().map(|t| t.is_dir()).unwrap_or(true) {
                continue;
            }
            let display = name
                .strip_suffix(".lnk")
                .or_else(|| name.strip_suffix(".LNK"))
                .unwrap_or(&name)
                .to_string();
            out.push(StartupItem {
                enabled: approved(loc.machine(), APPROVED_FOLDER, &name),
                display,
                command: e.path().display().to_string(),
                key: name,
                loc,
                file: Some(e.path()),
            });
        }
    }
    out.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    out
}

fn filetime_now() -> u64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs + 11_644_473_600) * 10_000_000
}

/// Enables or disables an item exactly like Task Manager does. Nothing is deleted.
fn set_enabled(item: &StartupItem, enabled: bool) -> Result<(), String> {
    let hive = root(item.loc.machine());
    let unchanged = match item.loc {
        Loc::UserRun | Loc::MachineRun | Loc::MachineRun32 => {
            let path = if item.loc == Loc::MachineRun32 {
                RUN32
            } else {
                RUN
            };
            hive.open_subkey(path)
                .and_then(|key| key.get_value::<String, _>(&item.key))
                .is_ok_and(|command| command == item.command)
        }
        Loc::UserFolder | Loc::CommonFolder => {
            item.file.as_ref().is_some_and(|file| file.is_file())
        }
    };
    if !unchanged {
        return Err(
            "The startup entry changed since it was listed. Check the refreshed list.".into(),
        );
    }
    if approved(item.loc.machine(), item.loc.approved_key(), &item.key) != item.enabled {
        return Err(
            "Windows changed this startup state. Check the refreshed list before trying again."
                .into(),
        );
    }
    let (key, _) = hive
        .create_subkey(item.loc.approved_key())
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0u8; 12];
    if enabled {
        bytes[0] = 2;
    } else {
        bytes[0] = 3;
        bytes[4..12].copy_from_slice(&filetime_now().to_le_bytes());
    }
    key.set_raw_value(
        &item.key,
        &RegValue {
            bytes,
            vtype: RegType::REG_BINARY,
        },
    )
    .map_err(|e| e.to_string())
}

/// Best-effort: the program a startup command launches.
pub fn exe_from_command(cmd: &str) -> Option<PathBuf> {
    let cmd = util::expand_env(cmd.trim());
    let path = if let Some(rest) = cmd.strip_prefix('"') {
        rest.split('"').next()?.to_string()
    } else {
        let lower = cmd.to_ascii_lowercase();
        match lower.find(".exe") {
            Some(i) => cmd[..i + 4].to_string(),
            None => cmd.split_whitespace().next()?.to_string(),
        }
    };
    let p = PathBuf::from(path);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[derive(Default)]
pub struct StartupState {
    pub items: Vec<StartupItem>,
    pub sampled_at: Option<std::time::Instant>,
    pub filter: String,
    load: Task<Vec<StartupItem>>,
    change: Task<Result<(String, bool), String>>,
    error: Option<String>,
}

impl StartupState {
    pub fn reload(&mut self, ctx: &egui::Context) {
        if !self.load.running() && !self.change.running() {
            self.load = Task::spawn(ctx, list);
        }
    }
    pub fn busy(&self) -> bool {
        self.change.running()
    }
    pub fn working(&self) -> bool {
        self.load.running() || self.busy()
    }
    pub fn enabled_count(&self) -> usize {
        self.items.iter().filter(|i| i.enabled).count()
    }
}

enum SAct {
    Set(usize, bool),
    Show(PathBuf),
    Reload,
    Relaunch,
    Settings,
    Filter(String),
}

impl App {
    pub fn page_startup(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        self.render_startup(ui, &mut acts);
        for a in acts {
            match a {
                SAct::Set(i, on) => {
                    if self.mutations_running() {
                        self.toast(ToastKind::Info, "Wait for the current operation to finish before changing startup apps.");
                        continue;
                    }
                    let Some(item) = self.startup.items.get(i).cloned() else {
                        continue;
                    };
                    if !self.startup.working() {
                        self.startup.change = Task::spawn(&ctx, move || {
                            set_enabled(&item, on)?;
                            util::log(format!(
                                "{} startup item: {}",
                                if on { "Enabled" } else { "Disabled" },
                                item.display
                            ));
                            Ok((item.display, on))
                        });
                    }
                }
                SAct::Show(p) => util::show_in_explorer(&p),
                SAct::Reload => self.startup.reload(&ctx),
                SAct::Relaunch => self.relaunch_admin(&ctx),
                SAct::Settings => util::open_uri("ms-settings:startupapps"),
                SAct::Filter(f) => self.startup.filter = f,
            }
        }
    }

    pub fn poll_startup(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.startup.change.poll() {
            match result.and_then(|r| r) {
                Ok((name, true)) => {
                    self.toast(ToastKind::Success, format!("{name} will start at sign-in."))
                }
                Ok((name, false)) => self.toast(
                    ToastKind::Success,
                    format!("{name} is disabled at sign-in."),
                ),
                Err(error) => self.toast(ToastKind::Error, error),
            }
            self.startup.reload(ctx);
        }
        if let Some(result) = self.startup.load.poll() {
            match result {
                Ok(items) => {
                    self.startup.items = items;
                    self.startup.sampled_at = Some(std::time::Instant::now());
                    self.startup.error = None;
                }
                Err(error) => self.startup.error = Some(error),
            }
        }
    }

    fn render_startup(&self, ui: &mut Ui, acts: &mut Vec<SAct>) {
        let s = &self.startup;
        widgets::page_header(
            ui,
            "Startup apps",
            "Apps that launch every time you sign in. Fewer of them means a faster boot and more free memory.",
        );
        widgets::banner(ui, color_accent(), ic::INFO, |ui| {
            ui.add(Label::new(STARTUP_INFO).wrap());
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{}", s.enabled_count()))
                    .font(widgets::light(30.0))
                    .color(color_ink()),
            );
            ui.label(widgets::dim(format!(
                "of {} start automatically",
                s.items.len()
            )));
            ui.add_space(16.0);
            let mut f = s.filter.clone();
            if widgets::search_box(ui, &mut f, "Search", 200.0).changed() {
                acts.push(SAct::Filter(f));
            }
            if ui.add(widgets::small("Windows startup settings")).clicked() {
                acts.push(SAct::Settings);
            }
            if ui
                .add_enabled(
                    !s.working(),
                    widgets::small(widgets::icon_text(
                        ic::REFRESH,
                        "Refresh",
                        13.0,
                        color_ink(),
                    )),
                )
                .clicked()
            {
                acts.push(SAct::Reload);
            }
            if !self.is_admin && s.items.iter().any(|i| i.loc.machine()) {
                if ui
                    .add(widgets::small(widgets::icon_text(
                        ic::SHIELD,
                        "Restart as admin",
                        13.0,
                        color_warning(),
                    )))
                    .clicked()
                {
                    acts.push(SAct::Relaunch);
                }
            }
        });
        if s.working() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(widgets::dim("Updating startup apps…"));
            });
        }
        if let Some(error) = &s.error {
            ui.add(Label::new(RichText::new(error).color(color_warning())).wrap());
        }
        ui.add_space(4.0);
        let filter = s.filter.to_lowercase();
        ScrollArea::vertical()
            .id_salt("startup_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if s.items.is_empty() && !s.working() && s.error.is_none() {
                    ui.label(widgets::dim("No startup apps found."));
                }
                for (i, item) in s.items.iter().enumerate() {
                    if !filter.is_empty()
                        && !item.display.to_lowercase().contains(&filter)
                        && !item.command.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    let allowed = !item.loc.machine() || self.is_admin;
                    let can_change = allowed && !s.working();
                    widgets::row(ui, 52.0, |ui| {
                        ui.add_space(4.0);
                        let resp = widgets::toggle(ui, item.enabled, can_change);
                        if !allowed {
                            resp.on_disabled_hover_text("Restart PCSweep as administrator to change apps that start for all users.");
                        } else if resp.clicked() {
                            acts.push(SAct::Set(i, !item.enabled));
                        }
                        ui.add_space(8.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            let target = item.file.clone();
                            let open = ui.add_enabled(target.is_some(), widgets::small(widgets::icon(ic::OPEN, 13.0, color_muted())));
                            match target {
                                Some(t) => {
                                    if open.on_hover_text("Show in Explorer").clicked() {
                                        acts.push(SAct::Show(t));
                                    }
                                }
                                None => {
                                    open.on_disabled_hover_text("The program's file couldn't be found");
                                }
                            }
                            ui.allocate_ui_with_layout(egui::vec2(170.0, 20.0), Layout::right_to_left(Align::Center), |ui| {
                                widgets::badge(ui, item.loc.label(), color_muted());
                            });
                            widgets::cell(
                                ui,
                                30.0,
                                true,
                                if item.enabled { RichText::new("On").color(color_success()) } else { RichText::new("Off").color(color_secondary()) },
                            );
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.add(Label::new(widgets::strong(&item.display, 14.5)).truncate()).on_hover_text(&item.display);
                                    ui.add(Label::new(widgets::faint(&item.command)).truncate()).on_hover_text(&item.command);
                                });
                            });
                        });
                    });
                }
                ui.add_space(20.0);
            });
    }
}
