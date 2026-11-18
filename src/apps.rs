use crate::app::{Action, App, Modal, ToastKind};
use crate::app_icons::{IconCache, IconSource};
use crate::app_packages::{self, AppUpgrade, PackageAction, PackageResult, StorePackage};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes, fmt_num};
use crate::widgets;
use chrono::NaiveDate;
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use std::cell::RefCell;
use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::Instant;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

#[derive(Clone)]
pub enum AppKind {
    Desktop,
    Store(StorePackage),
}

#[derive(Clone)]
pub struct InstalledApp {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub size: u64,
    pub installed: Option<NaiveDate>,
    pub uninstall: String,
    pub scope: &'static str,
    pub location: String,
    pub location_reported: bool,
    pub kind: AppKind,
    pub running: Option<bool>,
    pub startup: Option<bool>,
    pub icon: IconSource,
}

impl InstalledApp {
    pub fn drive_label(&self) -> String {
        use std::path::{Component, Path, Prefix};
        let path = util::expand_env(self.location.trim().trim_matches('"'));
        match Path::new(&path).components().next() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    format!("{}:", (d as char).to_ascii_uppercase())
                }
                Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => "Network".into(),
                _ => "Drive unknown".into(),
            },
            _ => "Drive unknown".into(),
        }
    }
}

pub fn list() -> Vec<InstalledApp> {
    list_desktop(false)
}
pub(crate) fn ownership_inventory() -> Vec<InstalledApp> {
    list_desktop(true)
}
fn list_desktop(include_hidden: bool) -> Vec<InstalledApp> {
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
        (
            false,
            r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
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
            if !include_hidden && k.get_value::<u32, _>("SystemComponent").unwrap_or(0) == 1 {
                continue;
            }
            let parent: String = k.get_value("ParentKeyName").unwrap_or_default();
            if !include_hidden && !parent.is_empty() {
                continue;
            }
            let release: String = k.get_value("ReleaseType").unwrap_or_default();
            if !include_hidden
                && matches!(release.as_str(), "Update" | "Hotfix" | "Security Update")
            {
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
            let mut location: String = k
                .get_value::<String, _>("InstallLocation")
                .unwrap_or_default()
                .trim()
                .to_string();
            let location_reported = !location.is_empty();
            let display = k.get_value::<String, _>("DisplayIcon").unwrap_or_default();
            if location.is_empty() {
                location = inferred_location(&display, &uninstall);
            }
            let icon = IconSource::desktop(
                display,
                uninstall_command(&uninstall)
                    .map(|(exe, _)| exe)
                    .unwrap_or_default(),
                location.clone(),
                &version,
            );
            out.push(InstalledApp {
                name,
                version: version.trim().to_string(),
                publisher: publisher.trim().to_string(),
                size,
                installed,
                uninstall,
                scope,
                location,
                location_reported,
                kind: AppKind::Desktop,
                running: None,
                startup: None,
                icon,
            });
        }
    }
    out
}

/// Fallback only for display and inventory. Never infer a cleanup folder from
/// a shared Windows installer executable.
fn inferred_location(display: &str, uninstall: &str) -> String {
    let from_icon = crate::app_icons::parse_display_icon(display).map(|(p, _)| p);
    let from_uninstall = uninstall_command(uninstall)
        .ok()
        .map(|(p, _)| std::path::PathBuf::from(p));
    from_icon
        .or(from_uninstall)
        .filter(|p| p.is_absolute())
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .filter(|p| {
            !util::is_within(
                &util::norm(p),
                &util::norm(&util::env("WINDIR").unwrap_or_else(|| r"C:\Windows".into())),
            )
        })
        .map(|p| p.display().to_string())
        .unwrap_or_default()
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppFilter {
    All,
    Desktop,
    Store,
    Protected,
    Updates,
}

pub struct AppInventory {
    pub(crate) list: Vec<InstalledApp>,
    store_error: Option<String>,
}

pub(crate) fn inventory() -> AppInventory {
    let mut list = list();
    let store_error = match app_packages::store_packages() {
        Ok(packages) => {
            list.extend(packages.into_iter().map(|p| InstalledApp {
                name: p.name.clone(),
                version: p.version.clone(),
                publisher: p.publisher.clone(),
                size: 0,
                installed: None,
                uninstall: String::new(),
                scope: "Your account",
                location: p.location.clone(),
                location_reported: true,
                icon: IconSource::Store {
                    family: p.family.clone(),
                    location: p.location.clone(),
                    identity: p.full_name.clone(),
                },
                kind: AppKind::Store(p),
                running: None,
                startup: None,
            }));
            None
        }
        Err(error) => Some(error),
    };
    AppInventory { list, store_error }
}

pub struct AppsState {
    pub list: Vec<InstalledApp>,
    usage_updated: Option<(Option<Instant>, Option<Instant>)>,
    pub loaded: bool,
    pub load: Task<AppInventory>,
    pub sort: AppSort,
    pub filter: String,
    pub view: AppFilter,
    pub action: Task<Vec<PackageResult>>,
    pub update_scan: Task<Result<Vec<AppUpgrade>, String>>,
    pub updates: Vec<AppUpgrade>,
    pub selected_store: HashSet<String>,
    pub selected_updates: HashSet<String>,
    pub store_error: Option<String>,
    pub updates_error: Option<String>,
    pub updates_checked: bool,
    pub results: Vec<PackageResult>,
    action_text: String,
    refresh_updates: bool,
    icons: RefCell<IconCache>,
    pub leftovers: crate::leftovers::State,
}

impl Default for AppsState {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            usage_updated: None,
            loaded: false,
            load: Task::default(),
            sort: AppSort::Size,
            filter: String::new(),
            view: AppFilter::All,
            action: Task::default(),
            update_scan: Task::default(),
            updates: Vec::new(),
            selected_store: HashSet::new(),
            selected_updates: HashSet::new(),
            store_error: None,
            updates_error: None,
            updates_checked: false,
            results: Vec::new(),
            action_text: String::new(),
            refresh_updates: false,
            icons: RefCell::new(IconCache::default()),
            leftovers: Default::default(),
        }
    }
}
impl AppsState {
    pub fn busy(&self) -> bool {
        self.action.running() || self.leftovers.busy()
    }
}

enum PAct {
    Sort(AppSort),
    Filter(String),
    View(AppFilter),
    Uninstall(usize),
    Reload,
    Settings,
    ToggleStore(String, bool),
    RemoveSelected,
    CheckUpdates,
    SetupUpdates,
    ToggleUpdate(String, bool),
    UpgradeSelected,
    OpenStore(String),
    OpenLocation(String),
}

impl App {
    pub fn start_apps_load(&mut self, ctx: &egui::Context) {
        if !self.apps.load.running() && !self.apps.busy() {
            self.apps.load = Task::spawn(ctx, inventory);
        }
    }

    pub fn poll_apps(&mut self, ctx: &egui::Context) {
        self.apps.icons.borrow_mut().poll(ctx);
        if let Some(r) = self.apps.load.poll() {
            match r {
                Ok(out) => {
                    self.apps.list = out.list;
                    self.apps.usage_updated = None;
                    self.apps.loaded = true;
                    self.apps.store_error = out.store_error;
                    let available: HashSet<String> = self
                        .apps
                        .list
                        .iter()
                        .filter_map(|a| match &a.kind {
                            AppKind::Store(p) if p.protection_reason().is_none() => {
                                Some(p.full_name.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    self.apps.selected_store.retain(|p| available.contains(p));
                }
                Err(e) => self.toast(
                    ToastKind::Error,
                    format!("Couldn't read installed apps: {e}"),
                ),
            }
        }
        if self.page == crate::app::Page::Apps {
            self.refresh_app_usage();
        }
        if let Some(r) = self.apps.update_scan.poll() {
            self.apps.updates_checked = true;
            match r {
                Ok(Ok(updates)) => {
                    self.apps.updates = updates;
                    self.apps.updates_error = None;
                    self.apps.selected_updates.clear();
                }
                Ok(Err(e)) | Err(e) => {
                    self.apps.updates_error = Some(e);
                    self.apps.updates.clear();
                    self.apps.selected_updates.clear();
                }
            }
        }
        if let Some(r) = self.apps.action.poll() {
            match r {
                Ok(results) => {
                    let good = results.iter().filter(|r| r.success).count();
                    self.toast(if good==results.len(){ToastKind::Success}else{ToastKind::Info},format!("App actions finished: {good} completed, {} failed. Review the results below.",results.len()-good));
                    self.apps.results = results;
                }
                Err(e) => self.toast(
                    ToastKind::Error,
                    format!("App action stopped: {e}. Refresh to check the current state."),
                ),
            }
            self.apps.selected_store.clear();
            self.apps.selected_updates.clear();
            self.start_apps_load(ctx);
            if self.apps.refresh_updates {
                self.start_app_updates(ctx);
            }
        }
    }

    fn refresh_app_usage(&mut self) {
        let sample = (self.sys.processes.sampled_at, self.startup.sampled_at);
        if self.apps.usage_updated == Some(sample) {
            return;
        }
        self.apps.usage_updated = Some(sample);
        // Keep badges synchronized with the same inventories shown on the
        // process and startup pages. Only a known executable path is evidence.
        let running: Vec<_> = self
            .sys
            .processes
            .groups
            .iter()
            .filter_map(|row| match &row.id {
                crate::running::RowId::Image(path, _) => Some(path.as_str()),
                crate::running::RowId::Process(_) => None,
            })
            .collect();
        let startup: Vec<_> = self
            .startup
            .items
            .iter()
            .filter(|item| item.enabled)
            .filter_map(|item| item.file.as_deref())
            .map(util::norm)
            .collect();
        for app in &mut self.apps.list {
            let root = util::norm(std::path::Path::new(&app.location));
            if root.len() <= 3 {
                continue;
            }
            app.running = sample
                .0
                .map(|_| running.iter().any(|path| util::is_within(path, &root)));
            app.startup = sample
                .1
                .map(|_| startup.iter().any(|path| util::is_within(path, &root)));
        }
    }

    pub fn start_app_updates(&mut self, ctx: &egui::Context) {
        if !self.apps.update_scan.running() && !self.apps.busy() {
            self.apps.updates_error = None;
            self.apps.update_scan = Task::spawn(ctx, app_packages::available_updates);
        }
    }

    pub fn start_package_action(&mut self, action: PackageAction, ctx: &egui::Context) {
        if self.apps.busy() {
            return;
        }
        let (count, text, refresh) = match &action {
            PackageAction::SetupUpdates => (
                1,
                "Setting up Microsoft's app update module for your account",
                true,
            ),
            PackageAction::Remove(p) => (
                p.len(),
                "Removing selected Store apps from your account",
                false,
            ),
            PackageAction::Upgrade(p) => (
                p.len(),
                "Updating selected apps. Keep PCSweep open; installers may ask for permission",
                true,
            ),
        };
        if count == 0 {
            return;
        }
        self.apps.action_text = format!("{text} ({count})");
        self.apps.refresh_updates = refresh;
        self.apps.results.clear();
        self.apps.action = Task::spawn(ctx, move || app_packages::run(action));
    }

    pub fn launch_uninstall(&mut self, app: &InstalledApp, ctx: &egui::Context) {
        if self.mutations_running() {
            self.toast(ToastKind::Info, "Wait for the current operation to finish.");
            return;
        }
        let target = crate::leftovers::Target::from(app);
        self.apps.leftovers.target = Some(target.clone());
        self.apps.leftovers.review = None;
        self.apps.leftovers.error = None;
        self.apps.leftovers.message = Some(
            "Running the app's registered uninstaller. Review its own removal choices first."
                .into(),
        );
        let command = app.uninstall.clone();
        self.apps.leftovers.launch = Task::spawn(ctx, move || {
            crate::scan_cache::save("last-uninstall.json", crate::scan_cache::now(), &target)?;
            let (program, args) = uninstall_command(&command).map_err(|e| e.to_string())?;
            let status = Command::new(program)
                .raw_arg(args)
                .creation_flags(util::CREATE_NO_WINDOW)
                .status()
                .map_err(|e| e.to_string())?;
            if status.success() || matches!(status.code(), Some(1641 | 3010)) {
                Ok(())
            } else {
                Err(format!(
                    "Uninstaller returned {}. Finish or retry removal before checking leftovers.",
                    status.code().unwrap_or(-1)
                ))
            }
        });
    }

    fn ask_store_remove(&mut self, packages: Vec<StorePackage>) {
        if packages.is_empty() {
            return;
        }
        self.modal=Some(Modal{
            title:format!("Remove {} Store app(s)?",packages.len()),
            body:"Only the selected apps will be removed from your current Windows account. Other accounts and Windows provisioning are unchanged. Close the apps and save your work first.".into(),
            details:packages.iter().map(|p|format!("{} · {} · {}",p.name,p.version,p.full_name)).collect(),
            note:Some("Removal may delete local app data and settings. Reinstalling from Microsoft Store does not restore that data. Required frameworks and Windows/security components are protected; identities and dependencies are checked again before removal.".into()),
            confirm:"Remove selected apps".into(),danger:true,action:Action::AppPackage(PackageAction::Remove(packages)),
        });
    }

    pub fn page_apps(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        widgets::page_header(
            ui,
            "Installed apps",
            "Review what is installed, remove optional apps, and update the apps you choose.",
        );
        self.leftovers_ui(ui);
        self.render_apps(ui, &mut acts);
        self.apps.icons.borrow_mut().start_pending(&ctx);
        for a in acts {
            match a {
                PAct::Sort(s) => self.apps.sort = s,
                PAct::Filter(f) => self.apps.filter = f,
                PAct::View(v) => self.apps.view = v,
                PAct::Reload => {
                    self.apps.icons.borrow_mut().refresh();
                    self.start_apps_load(&ctx);
                }
                PAct::Settings => util::open_uri("ms-settings:appsfeatures"),
                PAct::ToggleStore(id, on) => {
                    if on {
                        self.apps.selected_store.insert(id);
                    } else {
                        self.apps.selected_store.remove(&id);
                    }
                }
                PAct::ToggleUpdate(id, on) => {
                    if on {
                        self.apps.selected_updates.insert(id);
                    } else {
                        self.apps.selected_updates.remove(&id);
                    }
                }
                PAct::CheckUpdates => self.start_app_updates(&ctx),
                PAct::SetupUpdates => {
                    self.modal = Some(Modal {
                        title: "Set up selected app updates?".into(),
                        body: "Download and install Microsoft's signed Microsoft.WinGet.Client PowerShell module from the official PowerShell Gallery, for your current account. If needed, its NuGet provider prerequisite is installed for your account too.".into(),
                        details: vec!["Module: Microsoft.WinGet.Client · repository: https://www.powershellgallery.com/api/v2".into(),"PCSweep verifies the Microsoft publisher, module identity, version and supported update commands before using it.".into()],
                        note: Some("This downloads software and writes to your user module/provider folders. It does not update your apps yet. Repository trust, execution policy and WinGet sources are left unchanged. Existing Windows policy may block setup; Microsoft documentation is available below.".into()),
                        confirm: "Install Microsoft update module".into(),
                        danger: false,
                        action: Action::AppPackage(PackageAction::SetupUpdates),
                    });
                }
                PAct::OpenStore(uri) => util::open_uri(&uri),
                PAct::OpenLocation(p) => util::open_path(std::path::Path::new(&p)),
                PAct::RemoveSelected => {
                    let packages = self
                        .apps
                        .list
                        .iter()
                        .filter_map(|a| match &a.kind {
                            AppKind::Store(p)
                                if self.apps.selected_store.contains(&p.full_name)
                                    && p.protection_reason().is_none() =>
                            {
                                Some(p.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    self.ask_store_remove(packages);
                }
                PAct::UpgradeSelected => {
                    let packages: Vec<AppUpgrade> = self
                        .apps
                        .updates
                        .iter()
                        .filter(|p| self.apps.selected_updates.contains(&p.key()))
                        .cloned()
                        .collect();
                    if !packages.is_empty() {
                        self.modal=Some(Modal{
                    title:format!("Update {} selected app(s)?",packages.len()),
                    body:"PCSweep will ask Windows Package Manager to install exactly these reviewed versions, one app at a time. Save your work and close the selected apps. Installers may request administrator permission.".into(),
                    details:packages.iter().map(|p|format!("{} · {} → {} · {} ({})",p.name,p.installed,p.available,p.id,p.source)).collect(),
                    note:Some("Package-manager and publisher agreements apply. Updates can change app behavior; PCSweep cannot undo an app update. A result is recorded for each app.".into()),
                    confirm:"Update selected apps".into(),danger:false,action:Action::AppPackage(PackageAction::Upgrade(packages)),
                });
                    }
                }
                PAct::Uninstall(i) => {
                    if let Some(app) = self.apps.list.get(i).cloned() {
                        match &app.kind {
                            AppKind::Store(p) => self.ask_store_remove(vec![p.clone()]),
                            AppKind::Desktop => {
                                self.modal=Some(Modal{
                    title:format!("Uninstall {}?",app.name),body:format!("PCSweep will start {}'s registered uninstaller. Review the app's own removal options and save your work first.",app.name),
                    details:vec![format!("Version: {} · Publisher: {}",app.version,app.publisher),format!("Account scope: {}",app.scope)],
                    note:Some("Removal may delete app settings and data. Reinstalling later does not guarantee they can be recovered.".into()),
                    confirm:"Start uninstaller".into(),danger:true,action:Action::Uninstall(app),
                });
                            }
                        }
                    }
                }
            }
        }
    }

    fn render_apps(&self, ui: &mut Ui, acts: &mut Vec<PAct>) {
        let s = &self.apps;

        let total: u64 = s.list.iter().map(|a| a.size).sum();
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(fmt_num(s.list.len() as u64))
                    .font(widgets::light(30.0))
                    .color(TEXT),
            );
            ui.label(widgets::dim(format!(
                "registered apps · {} reported size",
                fmt_bytes(total)
            )));
            ui.add_space(12.0);
            let mut f = s.filter.clone();
            if widgets::search_box(ui, &mut f, "Search apps, publishers or drive", 220.0).changed()
            {
                acts.push(PAct::Filter(f));
            }
            if ui
                .add_enabled(!s.load.running() && !s.busy(), widgets::small("Refresh"))
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
        ui.horizontal_wrapped(|ui| {
            for (view, label) in [
                (AppFilter::All, "All apps"),
                (AppFilter::Desktop, "Desktop"),
                (AppFilter::Store, "Store apps"),
                (AppFilter::Protected, "Protected"),
                (AppFilter::Updates, "App updates"),
            ] {
                if ui.add(widgets::chip(label, s.view == view)).clicked() {
                    acts.push(PAct::View(view));
                }
            }
        });
        if s.busy() {
            ui.horizontal_wrapped(|ui| {
                ui.spinner();
                ui.label(widgets::dim(&s.action_text));
            });
        }
        for r in s.results.iter().take(8) {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{}: {}",
                        r.name,
                        if r.success { "completed" } else { "failed" }
                    ))
                    .color(if r.success { GOOD } else { WARN }),
                );
                ui.label(widgets::faint(&r.detail));
                if let Some(uri) = &r.store_uri {
                    if ui
                        .add(widgets::small("Find in Store to reinstall"))
                        .clicked()
                    {
                        acts.push(PAct::OpenStore(uri.clone()));
                    }
                }
            });
        }
        if s.results.len() > 8 {
            ui.label(widgets::faint(
                "More app action results are recorded in Activity.",
            ));
        }
        if s.view == AppFilter::Updates {
            self.render_app_updates(ui, acts);
            return;
        }
        if let Some(error) = &s.store_error {
            ui.label(
                RichText::new(format!("Store app inventory unavailable: {error}")).color(WARN),
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(widgets::dim("Sort by"));
            for (sort, label) in [
                (AppSort::Size, "Size"),
                (AppSort::Name, "Name"),
                (AppSort::Oldest, "Install date"),
            ] {
                if ui.add(widgets::chip(label, s.sort == sort)).clicked() {
                    acts.push(PAct::Sort(sort));
                }
            }
            if !s.selected_store.is_empty()
                && ui
                    .add_enabled(
                        !s.busy(),
                        widgets::small(format!("Review removal ({})", s.selected_store.len())),
                    )
                    .clicked()
            {
                acts.push(PAct::RemoveSelected);
            }
        });
        ui.label(widgets::faint("Sizes are app-reported estimates. Store sizes and install dates are unknown. Running/startup badges use known executable locations, not last-used guesses."));
        let filter = s.filter.to_lowercase();
        let mut idx: Vec<usize> = (0..s.list.len())
            .filter(|&i| {
                let a = &s.list[i];
                let view = match s.view {
                    AppFilter::Desktop => matches!(a.kind, AppKind::Desktop),
                    AppFilter::Store => matches!(a.kind, AppKind::Store(_)),
                    AppFilter::Protected => {
                        matches!(&a.kind,AppKind::Store(p) if p.protection_reason().is_some())
                    }
                    _ => true,
                };
                view && (filter.is_empty()
                    || a.name.to_lowercase().contains(&filter)
                    || a.publisher.to_lowercase().contains(&filter)
                    || a.location.to_lowercase().contains(&filter)
                    || a.drive_label().to_lowercase().contains(&filter))
            })
            .collect();
        match s.sort {
            AppSort::Size => idx.sort_by(|&a, &b| s.list[b].size.cmp(&s.list[a].size)),
            AppSort::Name => idx.sort_by_key(|&i| s.list[i].name.to_lowercase()),
            AppSort::Oldest => {
                idx.sort_by_key(|&i| (s.list[i].installed.is_none(), s.list[i].installed))
            }
        }
        if idx.is_empty() && !s.load.running() {
            ui.label(widgets::dim(
                "No apps match this view. Refresh to read installed apps again.",
            ));
        }
        ScrollArea::vertical().id_salt("apps_scroll").auto_shrink([false,false]).show_rows(ui,66.0,idx.len(),|ui,range|{
            for &i in &idx[range] {let a=&s.list[i];widgets::row(ui,66.0,|ui|{
                if let AppKind::Store(p)=&a.kind {
                    let mut on=s.selected_store.contains(&p.full_name);let reason=p.protection_reason();
                    let response=ui.add_enabled(reason.is_none()&&!s.busy(),egui::Checkbox::without_text(&mut on));
                    if response.changed(){acts.push(PAct::ToggleStore(p.full_name.clone(),on));}
                    if let Some(reason)=reason{response.on_disabled_hover_text(reason);}
                }else{ui.add_space(24.0);}
                s.icons.borrow_mut().show(ui, &a.icon);
                ui.with_layout(Layout::right_to_left(Align::Center),|ui|{
                    let reason=match &a.kind {AppKind::Store(p)=>p.protection_reason(),AppKind::Desktop if a.uninstall.is_empty()=>Some("No registered uninstaller. Use Windows installed apps.".into()),_=>None};
                    let response=ui.add_enabled(reason.is_none()&&!s.busy(),widgets::small(if reason.is_some(){"Protected"}else{"Uninstall"}));
                    if response.clicked(){acts.push(PAct::Uninstall(i));}if let Some(reason)=reason{response.on_disabled_hover_text(reason);}
                    if let AppKind::Store(p)=&a.kind {if let Some(uri)=p.store_uri(){if ui.add(widgets::small("Store")).on_hover_text("Find this app in Microsoft Store; availability is not guaranteed").clicked(){acts.push(PAct::OpenStore(uri));}}}
                    if !a.location.is_empty()&&ui.add(widgets::small("Folder")).on_hover_text(&a.location).clicked(){acts.push(PAct::OpenLocation(a.location.clone()));}
                    widgets::cell(ui,88.0,true,widgets::strong(if a.size>0{fmt_bytes(a.size)}else{"size unknown".into()},13.0));
                    ui.with_layout(Layout::left_to_right(Align::Center),|ui|{ui.vertical(|ui|{
                        ui.spacing_mut().item_spacing.y=1.0;
                        ui.add(Label::new(widgets::strong(&a.name,14.5)).truncate()).on_hover_text(format!("{}\n{}\n{}",a.name,a.publisher,a.location));
                        let mut meta=vec![a.drive_label(),match a.kind{AppKind::Desktop=>"Desktop".to_string(),AppKind::Store(_)=>"Store / MSIX".to_string()}];
                        if !a.version.is_empty(){meta.push(a.version.clone());}if !a.publisher.is_empty(){meta.push(a.publisher.clone());}meta.push(a.scope.to_string());
                        if a.running==Some(true){meta.push("Running".into());}if a.startup==Some(true){meta.push("Starts with Windows".into());}
                        if let Some(date)=a.installed{meta.push(format!("installed {}",date.format("%b %d, %Y")));}
                        ui.add(Label::new(widgets::faint(meta.join(" · "))).truncate());
                    });});
                });
            });}
        });
    }

    fn render_app_updates(&self, ui: &mut Ui, acts: &mut Vec<PAct>) {
        let s = &self.apps;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !s.update_scan.running() && !s.busy(),
                    widgets::small("Check app updates"),
                )
                .clicked()
            {
                acts.push(PAct::CheckUpdates);
            }
            if ui
                .add_enabled(
                    !s.selected_updates.is_empty() && !s.busy() && !s.update_scan.running(),
                    widgets::small(format!("Review updates ({})", s.selected_updates.len())),
                )
                .clicked()
            {
                acts.push(PAct::UpgradeSelected);
            }
            if ui.add(widgets::small("Microsoft Store Library")).clicked() {
                acts.push(PAct::OpenStore(
                    "ms-windows-store://downloadsandupdates".into(),
                ));
            }
            if s.update_scan.running() {
                ui.spinner();
                ui.label(widgets::dim("Checking Windows Package Manager…"));
            }
        });
        ui.label(widgets::faint("Checks use Microsoft's signed WinGet PowerShell module. Setup is optional and always reviewed first. Only selected apps and reviewed versions are updated."));
        if let Some(error) = &s.updates_error {
            ui.label(RichText::new(error).color(WARN));
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!s.busy(), widgets::small("Set up app updates"))
                    .clicked()
                {
                    acts.push(PAct::SetupUpdates);
                }
                if ui
                    .add(widgets::small("Microsoft setup documentation"))
                    .clicked()
                {
                    acts.push(PAct::OpenStore(
                        "https://learn.microsoft.com/windows/package-manager/winget/".into(),
                    ));
                }
            });
        } else if s.updates_checked && s.updates.is_empty() && !s.update_scan.running() {
            ui.label(widgets::dim(
                "No verifiable app updates were reported by Windows Package Manager.",
            ));
        } else if !s.updates_checked && !s.update_scan.running() {
            ui.label(widgets::dim("Check for updates to see each app's installed and available version before selecting anything."));
        }
        let filter = s.filter.to_lowercase();
        ScrollArea::vertical()
            .id_salt("app_updates_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for p in &s.updates {
                    if !filter.is_empty()
                        && !p.name.to_lowercase().contains(&filter)
                        && !p.id.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    widgets::row(ui, 58.0, |ui| {
                        let mut selected = s.selected_updates.contains(&p.key());
                        if ui
                            .add_enabled(!s.busy(), egui::Checkbox::without_text(&mut selected))
                            .changed()
                        {
                            acts.push(PAct::ToggleUpdate(p.key(), selected));
                        }
                        ui.vertical(|ui| {
                            ui.label(widgets::strong(&p.name, 14.5));
                            ui.label(widgets::faint(format!(
                                "{} · {} → {} · {}",
                                p.id, p.installed, p.available, p.source
                            )));
                        });
                    });
                }
            });
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> AppsState {
    let mut s = AppsState::default();
    s.icons = RefCell::new(IconCache::preview_fixture());
    s.loaded = true;
    s.list = vec![
        InstalledApp {
            name: "Visual Studio Code".into(),
            version: "1.105.1".into(),
            publisher: "Microsoft Corporation".into(),
            size: 435 * 1024 * 1024,
            installed: NaiveDate::from_ymd_opt(2026, 9, 5),
            uninstall: "fixture-uninstaller.exe".into(),
            scope: "Your account",
            location: r"C:\Users\Preview\AppData\Local\Programs\Microsoft VS Code".into(),
            location_reported: true,
            kind: AppKind::Desktop,
            running: Some(true),
            startup: Some(false),
            icon: IconSource::Generic,
        },
        InstalledApp {
            name: "Mozilla Firefox".into(),
            version: "144.0".into(),
            publisher: "Mozilla".into(),
            size: 350 * 1024 * 1024,
            installed: NaiveDate::from_ymd_opt(2026, 8, 14),
            uninstall: "fixture-uninstaller.exe".into(),
            scope: "All users",
            location: r"C:\Program Files\Mozilla Firefox".into(),
            location_reported: true,
            kind: AppKind::Desktop,
            running: Some(false),
            startup: Some(false),
            icon: IconSource::Generic,
        },
    ];
    for p in [
        StorePackage {
            name: "Microsoft Photos".into(),
            identity: "Microsoft.Windows.Photos".into(),
            full_name: "Microsoft.Windows.Photos_2026.11110.1001.0_x64__8wekyb3d8bbwe".into(),
            family: "Microsoft.Windows.Photos_8wekyb3d8bbwe".into(),
            version: "2026.11110.1001.0".into(),
            publisher: "Microsoft Corporation".into(),
            location: r"C:\Program Files\WindowsApps\Microsoft.Windows.Photos".into(),
            ..Default::default()
        },
        StorePackage {
            name: "Windows Security".into(),
            identity: "Microsoft.SecHealthUI".into(),
            full_name: "Microsoft.SecHealthUI_1000.0.0.0_x64__8wekyb3d8bbwe".into(),
            family: "Microsoft.SecHealthUI_8wekyb3d8bbwe".into(),
            version: "1000.0.0.0".into(),
            publisher: "Microsoft Corporation".into(),
            system: true,
            ..Default::default()
        },
        StorePackage {
            name: "Microsoft Visual C++ runtime".into(),
            identity: "Microsoft.VCLibs.140.00".into(),
            full_name: "Microsoft.VCLibs.140.00_14.0.0.0_x64__8wekyb3d8bbwe".into(),
            family: "Microsoft.VCLibs.140.00_8wekyb3d8bbwe".into(),
            version: "14.0.0.0".into(),
            publisher: "Microsoft Corporation".into(),
            framework: true,
            dependents: vec!["Microsoft Photos".into()],
            ..Default::default()
        },
    ] {
        s.list.push(InstalledApp {
            name: p.name.clone(),
            version: p.version.clone(),
            publisher: p.publisher.clone(),
            size: 0,
            installed: None,
            uninstall: String::new(),
            scope: "Your account",
            location: p.location.clone(),
            location_reported: true,
            icon: IconSource::Generic,
            kind: AppKind::Store(p),
            running: None,
            startup: None,
        });
    }
    s.updates_checked = true;
    s.updates = vec![
        AppUpgrade {
            name: "Mozilla Firefox".into(),
            id: "Mozilla.Firefox".into(),
            source: "winget".into(),
            installed: "144.0".into(),
            available: "145.0".into(),
        },
        AppUpgrade {
            name: "Visual Studio Code".into(),
            id: "Microsoft.VisualStudioCode".into(),
            source: "winget".into(),
            installed: "1.105.1".into(),
            available: "1.106.0".into(),
        },
    ];
    s
}

#[cfg(test)]
mod drive_tests {
    use super::*;
    #[test]
    fn inventory_exposes_drive_letters_and_honest_unknown_locations() {
        let mut a = preview_fixture().list.remove(0);
        a.location = r"D:\Games\Example".into();
        assert_eq!(a.drive_label(), "D:");
        a.location = r"\\?\E:\Apps\Example".into();
        assert_eq!(a.drive_label(), "E:");
        a.location = r"\\server\apps\Example".into();
        assert_eq!(a.drive_label(), "Network");
        a.location.clear();
        assert_eq!(a.drive_label(), "Drive unknown");
        assert!(inferred_location("", r"C:\Windows\System32\msiexec.exe /x {ABC}").is_empty());
        assert_eq!(
            inferred_location(r#""D:\Apps\Example\app.exe",0"#, ""),
            r"D:\Apps\Example"
        );
    }
}
