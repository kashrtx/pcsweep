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
        (
            true,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            "All users",
        ),
        (
            true,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            "All users",
        ),
        (
            false,
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
            "Your account",
        ),
    ];
    for (machine, path, scope) in sources {
        let root = RegKey::predef(if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        });
        let Ok(key) = root.open_subkey(path) else {
            continue;
        };
        for sub in key.enum_keys().flatten() {
            let Ok(k) = key.open_subkey(&sub) else {
                continue;
            };
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
            let publisher: String = k.get_value("Publisher").unwrap_or_default();
            let size = k.get_value::<u32, _>("EstimatedSize").unwrap_or(0) as u64 * 1024;
            let date: String = k.get_value("InstallDate").unwrap_or_default();
            let installed = NaiveDate::parse_from_str(date.trim(), "%Y%m%d").ok();
            let uninstall: String = k
                .get_value::<String, _>("UninstallString")
                .unwrap_or_default()
                .trim()
                .to_string();
            if !seen.insert(format!(
                "{}|{}|{}|{}",
                name.to_lowercase(),
                version,
                scope,
                uninstall.to_ascii_lowercase()
            )) {
                continue;
            }
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

/// Split the executable from the Windows argument tail without interpreting shell
/// metacharacters. Registry uninstall strings commonly omit quotes around .exe paths.
fn uninstall_command(command: &str) -> std::io::Result<(String, String)> {
    let command = util::expand_env(command.trim());
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid uninstaller command",
        )
    };
    if command.is_empty() || command.contains(['\r', '\n', '\0']) {
        return Err(invalid());
    }
    let (program, args) = if let Some(rest) = command.strip_prefix('"') {
        let end = rest.find('"').ok_or_else(invalid)?;
        (&rest[..end], rest[end + 1..].trim_start())
    } else {
        // ASCII case conversion preserves byte positions even for Unicode paths.
        let lower = command.to_ascii_lowercase();
        let end = lower
            .match_indices(".exe")
            .find(|(i, _)| {
                command[i + 4..]
                    .chars()
                    .next()
                    .map_or(true, char::is_whitespace)
            })
            .map(|(i, _)| i + 4)
            .unwrap_or_else(|| command.find(char::is_whitespace).unwrap_or(command.len()));
        (&command[..end], command[end..].trim_start())
    };
    if program.is_empty() {
        return Err(invalid());
    }
    let is_msi = program.rsplit(['\\', '/']).next().map_or(false, |p| {
        p.eq_ignore_ascii_case("msiexec") || p.eq_ignore_ascii_case("msiexec.exe")
    });
    let mut args = args.to_string();
    // /I opens modify/repair. Change only a leading install switch followed by a
    // product code; never rewrite text inside a quoted path or another argument.
    if is_msi
        && args
            .get(..2)
            .map_or(false, |s| s.eq_ignore_ascii_case("/i"))
        && args[2..].trim_start().starts_with('{')
    {
        args.replace_range(..2, "/X");
    }
    Ok((program.to_string(), args))
}

/// Runs the app's own uninstaller, which shows its own confirmation.
pub fn launch_uninstaller(cmd: &str) -> std::io::Result<()> {
    let (program, args) = uninstall_command(cmd)?;
    Command::new(program)
        .raw_arg(args)
        .creation_flags(util::CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::uninstall_command;

    #[test]
    fn uninstaller_keeps_quoted_arguments_and_space_in_executable() {
        assert_eq!(
            uninstall_command(r#"C:\Program Files\A & B\uninstall.exe /remove "one two""#).unwrap(),
            (
                r#"C:\Program Files\A & B\uninstall.exe"#.into(),
                r#"/remove "one two""#.into()
            )
        );
        assert_eq!(
            uninstall_command(r#""C:\İstanbul\uninstall.exe" /remove"#).unwrap(),
            (r#"C:\İstanbul\uninstall.exe"#.into(), "/remove".into())
        );
    }

    #[test]
    fn msi_install_switch_is_rewritten_only_for_product_codes() {
        assert_eq!(
            uninstall_command(r#"MsiExec.exe /I{ABC-123}"#).unwrap().1,
            "/X{ABC-123}"
        );
        assert_eq!(
            uninstall_command(r#""C:\Windows\System32\msiexec.exe" /i {ABC-123}"#)
                .unwrap()
                .1,
            "/X {ABC-123}"
        );
        assert_eq!(
            uninstall_command(r#"msiexec /i "installer.msi""#)
                .unwrap()
                .1,
            r#"/i "installer.msi""#
        );
        assert!(uninstall_command("").is_err());
        assert!(uninstall_command("\"unterminated").is_err());
    }
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
                Err(e) => self.toast(
                    ToastKind::Error,
                    format!("Couldn't read installed apps: {e}"),
                ),
            }
        }
    }

    pub fn launch_uninstall(&mut self, app: &InstalledApp) {
        match launch_uninstaller(&app.uninstall) {
            Ok(()) => {
                util::log(format!("Started the uninstaller for {}", app.name));
                self.toast(
                    ToastKind::Info,
                    format!(
                        "{}'s uninstaller is starting. Refresh the list when it's done.",
                        app.name
                    ),
                );
            }
            Err(e) => self.toast(
                ToastKind::Error,
                format!("Couldn't start the uninstaller: {e}"),
            ),
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
                            action: Action::Uninstall(app.clone()),
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
            ui.label(
                RichText::new(fmt_num(s.list.len() as u64))
                    .font(widgets::light(30.0))
                    .color(TEXT),
            );
            ui.label(widgets::dim(format!(
                "apps using about {}",
                fmt_bytes(total)
            )));
            ui.add_space(16.0);
            let mut f = s.filter.clone();
            if widgets::search_box(ui, &mut f, "Search apps or publishers", 220.0).changed() {
                acts.push(PAct::Filter(f));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(widgets::small(widgets::icon_text(
                        ic::REFRESH,
                        "Refresh",
                        13.0,
                        TEXT,
                    )))
                    .clicked()
                {
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
            for (srt, label) in [
                (AppSort::Size, "Size"),
                (AppSort::Name, "Name"),
                (AppSort::Oldest, "Install date"),
            ] {
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
            AppSort::Name => idx.sort_by(|&a, &b| {
                s.list[a]
                    .name
                    .to_lowercase()
                    .cmp(&s.list[b].name.to_lowercase())
            }),
            AppSort::Oldest => {
                idx.sort_by(|&a, &b| match (s.list[a].installed, s.list[b].installed) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
            }
        }
        if idx.is_empty() && !s.load.running() {
            ui.label(widgets::dim(if filter.is_empty() {
                "No registered desktop apps found. Open Windows installed apps to manage Store apps."
            } else {
                "No apps match your search."
            }));
        }
        let top = s.list.iter().map(|a| a.size).max().unwrap_or(1).max(1) as f32;
        let today = chrono::Local::now().date_naive();

        ScrollArea::vertical()
            .id_salt("apps_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, 48.0, idx.len(), |ui, range| {
                for &i in &idx[range] {
                    let app = &s.list[i];
                    widgets::row(ui, 48.0, |ui| {
                        ui.label(widgets::icon(ic::APPS, 18.0, ACCENT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            if app.uninstall.is_empty() {
                                ui.add_enabled(false, widgets::small("Uninstall"))
                                    .on_disabled_hover_text("This app didn't register an uninstaller. Use Windows installed apps instead.");
                            } else if ui.add(widgets::small("Uninstall")).clicked() {
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
                            widgets::cell(ui, 160.0, true, widgets::faint(age));
                            let size = if app.size > 0 { fmt_bytes(app.size) } else { "unknown".into() };
                            widgets::cell(ui, 80.0, true, widgets::strong(size, 14.0));
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
