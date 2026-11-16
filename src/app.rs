use crate::analyzer::AnalyzerState;
use crate::apps::AppsState;
use crate::cleaner::CleanerState;
use crate::duplicates::DupState;
use crate::processes::MemState;
use crate::startup::StartupState;
use crate::system::{self, SysMon};
use crate::task::Task;
use crate::theme::{self, *};
use crate::tools::ToolsState;
use crate::util::{self, fmt_bytes};
use crate::widgets::{self, ic};
use eframe::egui::{
    self, vec2, Align, Align2, Color32, Frame, Id, Key, Label, LayerId, Layout, Margin, Order,
    RichText, Rounding, ScrollArea, Stroke, Ui,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Page {
    Dashboard,
    Cleaner,
    Analyzer,
    Duplicates,
    Apps,
    Startup,
    Memory,
    Privacy,
    Security,
    Background,
    Ads,
    Network,
    Power,
    Updates,
    Tools,
    Activity,
    Storage,
    WindowsControls,
    Speed,
    FileChanges,
}

fn nav_groups() -> [&'static [(Page, char, &'static str)]; 4] {
    [
        &[(Page::Dashboard, ic::HOME, "Overview")],
        &[
            (Page::Cleaner, ic::CLEAN, "Junk cleaner"),
            (Page::Analyzer, ic::PIE, "Space analyzer"),
            (Page::Storage, ic::DRIVE, "Disk health"),
            (Page::Duplicates, ic::COPY, "Duplicates"),
            (Page::Apps, ic::APPS, "Installed apps"),
        ],
        &[
            (Page::Ads, ic::GLOBE, "Ads and tracking"),
            (Page::Privacy, ic::LOCK, "Privacy and ads"),
            (Page::Security, ic::SHIELD, "Security check"),
            (Page::Startup, ic::POWER, "Startup apps"),
            (Page::Background, ic::BOLT, "Background activity"),
            (Page::Memory, ic::SPEED, "Memory and apps"),
            (Page::Network, ic::GLOBE, "Connection health"),
            (Page::Speed, ic::SPEED, "Internet speed test"),
            (Page::Power, ic::POWER, "Battery and power"),
        ],
        &[
            (Page::WindowsControls, ic::TOOLS, "Windows controls"),
            (Page::FileChanges, ic::HISTORY, "Windows file changes"),
            (Page::Tools, ic::TOOLS, "Tools"),
            (Page::Updates, ic::UPDATE, "Updates"),
            (Page::Activity, ic::HISTORY, "Activity log"),
        ],
    ]
}

const SESSION_KEY: &str = r"Software\PCSweep\Session";

fn load_session() -> (Page, [f32; 2]) {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(SESSION_KEY) else {
        return (Page::Dashboard, [1320.0, 860.0]);
    };
    let page = key
        .get_value::<String, _>("Page")
        .ok()
        .filter(|s| s.len() < 128)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Page::Dashboard);
    let width = key
        .get_value::<u32, _>("Width")
        .ok()
        .filter(|n| (900..=7680).contains(n))
        .unwrap_or(1320);
    let height = key
        .get_value::<u32, _>("Height")
        .ok()
        .filter(|n| (520..=4320).contains(n))
        .unwrap_or(860);
    (page, [width as f32, height as f32])
}

pub fn saved_window_size() -> [f32; 2] {
    load_session().1
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub born: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Analyzer,
    Duplicates,
}

/// Everything that changes the system goes through a confirmation dialog first.
pub enum Action {
    QuitApp,
    CleanJunk,
    Recycle {
        items: Vec<(PathBuf, u64)>,
        origin: Origin,
    },
    Kill {
        pids: Vec<(u32, u64)>,
        name: String,
    },
    RunTool(usize),
    Uninstall(crate::apps::InstalledApp),
    RemoveLeftovers(crate::leftovers::Target, Vec<crate::leftovers::Candidate>),
    RestoreLeftoverRegistry(u64),
    RecycleChecked(Vec<util::CheckedPath>),
    Privacy(crate::privacy::PrivacyAction),
    Preferences(crate::preferences::PreferencesAction),
    Security(crate::security::SecurityAction),
    Background(crate::background::BackgroundAction),
    Update(crate::updates::UpdateAction),
    Memory(crate::processes::MemoryAction),
    AppPackage(crate::app_packages::PackageAction),
    Ads(crate::ads::AdsAction),
    Network(crate::network::NetworkAction),
    Power(crate::power::PowerAction),
    Storage(crate::storage::StorageAction),
    WindowsControls(crate::windows_controls::WindowsControlsAction),
    Speed(crate::speed::SpeedAction),
}

pub struct Modal {
    pub title: String,
    pub body: String,
    pub details: Vec<String>,
    pub note: Option<String>,
    pub confirm: String,
    pub danger: bool,
    pub action: Action,
}

pub struct App {
    pub page: Page,
    pub is_admin: bool,
    pub sys: SysMon,
    pub cleaner: CleanerState,
    pub analyzer: AnalyzerState,
    pub dups: DupState,
    pub startup: StartupState,
    pub apps: AppsState,
    pub tools: ToolsState,
    pub mem: MemState,
    pub privacy: crate::privacy::PrivacyState,
    pub preferences: crate::preferences::PreferencesState,
    pub security: crate::security::SecurityState,
    pub background: crate::background::BackgroundState,
    pub updates: crate::updates::UpdateState,
    pub ads: crate::ads::AdsState,
    pub network: crate::network::NetworkState,
    pub power: crate::power::PowerState,
    pub history: crate::journal::HistoryState,
    pub storage: crate::storage::StorageState,
    pub windows_controls: crate::windows_controls::WindowsControlsState,
    pub speed: crate::speed::SpeedState,
    pub file_changes: crate::file_changes::FileChangesState,
    pub modal: Option<Modal>,
    pub toasts: Vec<Toast>,
    pub relaunch: Task<bool>,
    activity_filter: String,
    last_nav_page: Option<Page>,
    reveal_nav_frames: u8,
    /// When the current confirmation first appeared; its action stays disarmed
    /// briefly so a click or key press meant for the page can't confirm it.
    modal_since: Option<Instant>,
    last_window_size: [f32; 2],
    tray: crate::tray::Tray,
    keep_background: bool,
    background_hidden: bool,
    app_options_open: bool,
    tray_error: Option<String>,
    pub(crate) force_exit: bool,
    pub(crate) appearance: crate::appearance::Appearance,
}

/// How long a new confirmation ignores its confirm button.
const MODAL_ARMING: Duration = Duration::from_millis(400);

fn hide_on_close(force_exit: bool, keep_background: bool, tray_available: bool) -> bool {
    !force_exit && keep_background && tray_available
}

impl App {
    fn show_from_background(&mut self, ctx: &egui::Context) {
        self.background_hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        for event in self.tray.poll() {
            match event {
                crate::tray::TrayEvent::Ready => self.tray_error = None,
                crate::tray::TrayEvent::Failed(reason) => {
                    self.tray_error = Some(reason);
                    if self.background_hidden {
                        self.show_from_background(ctx);
                    }
                }
                crate::tray::TrayEvent::Show => self.show_from_background(ctx),
                crate::tray::TrayEvent::CheckUpdates => {
                    self.show_from_background(ctx);
                    self.page = Page::Updates;
                    self.begin_update_check(ctx);
                }
                crate::tray::TrayEvent::Quit => {
                    if self.mutations_running() {
                        self.show_from_background(ctx);
                        self.modal = Some(Modal {
                            title: "Quit while an operation is running?".into(),
                            body: "An active cleanup, download or Windows change may be interrupted. Completed changes remain applied; unfinished history may be uncertain. You can keep PCSweep running in the background until it finishes.".into(),
                            details: Vec::new(), note: None, confirm: "Quit anyway".into(), danger: true, action: Action::QuitApp,
                        });
                    } else {
                        self.force_exit = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
    }

    pub(crate) fn app_options_ui(&mut self, ctx: &egui::Context) {
        if !self.app_options_open || self.modal.is_some() {
            return;
        }
        let mut open = self.app_options_open;
        let mut keep = self.keep_background;
        let mut automatic = self.updates.automatic_checks();
        let previous_appearance = self.appearance.clone();
        let mut elevated = crate::appearance::start_elevated();
        egui::Window::new("PCSweep options").open(&mut open).collapsible(false).resizable(false).default_width(480.0).vscroll(true).show(ctx, |ui| {
            ui.add(egui::Label::new("Choose how PCSweep stays available. No background cleanup, speed tests or Windows changes run automatically.").wrap());
            ui.add_space(8.0);
            ui.add_enabled(self.tray.available(), egui::Checkbox::new(&mut keep, "Keep running in the notification area when I close the window"));
            ui.add(egui::Label::new("Use the PCSweep notification icon to show the window, check for updates, or Quit. The normal Minimize button also keeps the app running.").wrap());
            if let Some(reason) = &self.tray_error { ui.colored_label(WARN, format!("Notification icon unavailable: {reason}")); }
            else if !self.tray.available() { ui.label(widgets::dim("Starting the notification icon…")); }
            ui.add_space(10.0);
            ui.separator();
            egui::CollapsingHeader::new("Readability and text").show(ui,|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Interface size");
                    for scale in [100,110,125,150] {
                        ui.selectable_value(&mut self.appearance.scale,scale,format!("{scale}%"));
                    }
                });
                ui.checkbox(&mut self.appearance.bold,"Use bolder text");
                ui.add(egui::Label::new("Size and weight are remembered. Larger layouts can scroll sideways when needed. Windows display scaling also applies.").wrap());
            });
            ui.separator();
            ui.checkbox(&mut automatic, "Check for PCSweep updates automatically once a day");
            ui.add(egui::Label::new("Works while the window is open or PCSweep is running in the background. Checks contact GitHub; updates install only when you choose. PCSweep does not add itself to Windows startup.").wrap());
            if ui.add(widgets::small("Open Updates")).clicked() { self.page = Page::Updates; }
            ui.separator();
            ui.checkbox(&mut elevated,"Ask to start as administrator on future launches");
            ui.add(egui::Label::new("Windows asks for UAC approval. Canceling opens PCSweep normally. Administrator restart is also available when needed.").wrap());
        });
        self.app_options_open = open;
        if self.appearance != previous_appearance {
            self.appearance.save();
            self.appearance.apply(ctx);
            theme::setup_fonts(ctx);
            theme::fit_to_viewport(ctx);
        }
        if elevated != crate::appearance::start_elevated() {
            crate::appearance::set_start_elevated(elevated);
        }
        if keep != self.keep_background {
            self.keep_background = keep;
            if let Ok((key, _)) =
                winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER).create_subkey(SESSION_KEY)
            {
                let _ = key.set_value("KeepBackground", &u32::from(keep));
            }
        }
        if automatic != self.updates.automatic_checks() {
            self.set_automatic_update_checks(automatic);
        }
    }
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let appearance = crate::appearance::Appearance::load();
        appearance.apply(&cc.egui_ctx);
        theme::setup_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        theme::read_windows_motion(&cc.egui_ctx);
        let is_admin = system::is_admin();
        let (page, last_window_size) = load_session();
        let keep_background = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
            .open_subkey(SESSION_KEY)
            .and_then(|k| k.get_value::<u32, _>("KeepBackground"))
            .ok()
            == Some(1);
        let mut app = Self {
            page,
            is_admin,
            sys: SysMon::new(),
            cleaner: CleanerState::new(),
            analyzer: AnalyzerState::default(),
            dups: DupState::default(),
            startup: StartupState::default(),
            apps: AppsState::default(),
            tools: ToolsState::default(),
            mem: MemState::default(),
            privacy: Default::default(),
            preferences: Default::default(),
            security: Default::default(),
            background: Default::default(),
            updates: Default::default(),
            ads: Default::default(),
            network: Default::default(),
            power: Default::default(),
            history: Default::default(),
            storage: Default::default(),
            windows_controls: Default::default(),
            speed: Default::default(),
            file_changes: Default::default(),
            modal: None,
            toasts: Vec::new(),
            relaunch: Task::default(),
            activity_filter: String::new(),
            last_nav_page: None,
            reveal_nav_frames: 0,
            modal_since: None,
            last_window_size,
            tray: crate::tray::Tray::start(&cc.egui_ctx),
            keep_background,
            background_hidden: false,
            app_options_open: false,
            tray_error: None,
            force_exit: false,
            appearance: Default::default(),
        };
        app.appearance = appearance;
        use raw_window_handle::HasWindowHandle;
        if let Ok(handle) = cc.window_handle() {
            if let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() {
                app.tray.bind_window(h.hwnd.get());
            }
        }
        util::load_log_history();
        util::log(format!(
            "PCSweep {} started{}",
            env!("CARGO_PKG_VERSION"),
            if is_admin { " as administrator" } else { "" }
        ));
        app.startup.reload();
        app.restore_scan_caches(&cc.egui_ctx);
        app.start_apps_load(&cc.egui_ctx);
        app.apps.leftovers.restore = Task::spawn(&cc.egui_ctx, || {
            crate::scan_cache::load::<crate::leftovers::Target>("last-uninstall.json")
                .map(|s| s.map(|s| s.data))
        });
        app.refresh_windows_controls(&cc.egui_ctx);
        app
    }

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            born: Instant::now(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    pub fn relaunch_admin(&mut self, ctx: &egui::Context) {
        if self.mutations_running() {
            self.toast(
                ToastKind::Info,
                "Wait for the current operation to finish before restarting PCSweep.",
            );
            return;
        }
        if !self.relaunch.running() {
            self.relaunch = Task::spawn(ctx, system::relaunch_as_admin);
        }
    }

    fn start_recycle(&mut self, items: Vec<(PathBuf, u64)>, origin: Origin, ctx: &egui::Context) {
        if self.cleaner.clean.running()
            || self.analyzer.recycle.running()
            || self.dups.recycle.running()
        {
            self.toast(
                ToastKind::Info,
                "A cleanup is already running. Wait for it to finish before moving more files.",
            );
            return;
        }
        match origin {
            Origin::Analyzer => {
                self.analyzer.recycle =
                    Task::spawn(ctx, move || util::recycle(items, "Space analyzer"));
            }
            Origin::Duplicates => {
                let groups = self.dups.groups.clone().unwrap_or_default();
                self.dups.recycle = Task::spawn(ctx, move || {
                    crate::duplicates::recycle_verified(items, groups)
                });
            }
        }
    }

    fn execute(&mut self, action: Action, ctx: &egui::Context) {
        let resolving_dns = matches!(&action, Action::Ads(action) if action.is_resolution());
        if self.mutations_running() && !resolving_dns && !matches!(action, Action::QuitApp) {
            self.toast(ToastKind::Info, "An operation is already running. Wait for it to finish before starting another change.");
            return;
        }
        match action {
            Action::QuitApp => {
                self.force_exit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::CleanJunk => self.start_clean(ctx),
            Action::RecycleChecked(items) => {
                self.analyzer.recycle = Task::spawn(ctx, move || util::recycle_checked(items));
            }
            Action::Recycle { items, origin } => self.start_recycle(items, origin, ctx),
            Action::Kill { pids, name } => {
                let n = self.sys.kill(&pids);
                util::log(format!("Ended {name} ({n} process(es))"));
                if n == pids.len() && n > 0 {
                    self.toast(ToastKind::Success, format!("Ended {name}"));
                } else if n > 0 {
                    self.toast(
                        ToastKind::Info,
                        format!(
                        "Ended {n} of {} processes for {name}. Some exited or could not be ended.",
                        pids.len()
                    ),
                    );
                } else {
                    self.toast(
                        ToastKind::Error,
                        format!("{name} could not be ended. It may need administrator rights."),
                    );
                }
            }
            Action::RunTool(i) => self.start_tool(i, ctx),
            Action::Uninstall(app) => self.launch_uninstall(&app, ctx),
            Action::RemoveLeftovers(target, items) => {
                if !self.mutations_running() {
                    self.apps.leftovers.removal =
                        Task::spawn(ctx, move || crate::leftovers::remove(target, items));
                }
            }
            Action::RestoreLeftoverRegistry(stamp) => {
                if !self.mutations_running() {
                    self.apps.leftovers.removal =
                        Task::spawn(ctx, move || crate::leftovers::restore_registry(stamp));
                }
            }
            Action::Privacy(action) => self.execute_privacy(action, ctx),
            Action::Preferences(action) => self.execute_preferences(action, ctx),
            Action::Security(action) => self.execute_security(action, ctx),
            Action::Background(action) => self.execute_background(action, ctx),
            Action::Update(action) => self.execute_update(action, ctx),
            Action::Memory(action) => self.execute_memory(action, ctx),
            Action::AppPackage(action) => self.start_package_action(action, ctx),
            Action::Ads(action) => self.execute_ads(action, ctx),
            Action::Network(action) => self.execute_network(action, ctx),
            Action::Power(action) => self.execute_power(action, ctx),
            Action::Storage(action) => self.execute_storage(action, ctx),
            Action::WindowsControls(action) => self.execute_windows_controls(action, ctx),
            Action::Speed(action) => self.execute_speed(action, ctx),
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_history();
        self.poll_scan_caches(ctx);
        self.poll_cleaner(ctx);
        self.poll_analyzer();
        self.poll_duplicates();
        self.poll_apps(ctx);
        self.poll_leftovers(ctx);
        self.poll_tools();
        self.poll_memory();
        self.poll_privacy(ctx);
        self.poll_preferences(ctx);
        self.poll_security(ctx);
        self.poll_background(ctx);
        self.poll_updates(ctx);
        self.poll_ads(ctx);
        self.poll_network(ctx);
        self.poll_power(ctx);
        self.poll_storage(ctx);
        self.poll_windows_controls(ctx);
        self.poll_speed(ctx);
        self.poll_file_changes();
        if let Some(r) = self.relaunch.poll() {
            match r {
                Ok(true) => {
                    util::log("Restarting as administrator");
                    self.force_exit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                _ => self.toast(ToastKind::Info, "Restart as administrator was cancelled."),
            }
        }
    }

    pub fn mutations_running(&self) -> bool {
        self.cleaner.clean.running()
            || self.analyzer.recycle.running()
            || self.dups.recycle.running()
            || self.apps.busy()
            || self.mem.job.running()
            || self.privacy.busy()
            || self.preferences.busy()
            || self.security.busy()
            || self.background.busy()
            || self.ads.busy()
            || self.network.busy()
            || self.power.busy()
            || self.storage.busy()
            || self.windows_controls.busy()
            || self.speed.busy()
            || self.updates.installing()
            || self.relaunch.running()
            || self.tools.run.as_ref().is_some_and(|run| !run.is_done())
    }

    fn any_running(&self) -> bool {
        self.mutations_running()
            || self.updates.running()
            || self.mem.job.running()
            || self.privacy.busy()
            || self.security.busy()
            || self.background.busy()
            || self.cleaner.scan.running()
            || self.cleaner.clean.running()
            || self.analyzer.scan.running()
            || self.analyzer.selection_check.running()
            || self.analyzer.cache_running()
            || self.analyzer.recycle.running()
            || self.dups.scan.running()
            || self.dups.recycle.running()
            || self.apps.load.running()
            || self.relaunch.running()
            || self.tools.run.as_ref().map_or(false, |r| !r.is_done())
            || !self.toasts.is_empty()
    }

    // ------------------------------------------------------------ sidebar

    pub(crate) fn compact_navigation(&self, ctx: &egui::Context) -> bool {
        self.appearance.scale > 100 && ctx.screen_rect().width() < 1120.0
    }
    pub(crate) fn navigation_width(&self, ctx: &egui::Context) -> f32 {
        if self.compact_navigation(ctx) {
            80.0
        } else {
            236.0
        }
    }
    pub(crate) fn compact_sidebar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mode = egui::Id::new("pcsweep.nav_compact");
        let was_compact = ctx.data(|d| d.get_temp::<bool>(mode)).unwrap_or(false);
        ctx.data_mut(|d| d.insert_temp(mode, true));
        if !was_compact || self.last_nav_page != Some(self.page) {
            self.reveal_nav_frames = 3;
        }
        let reveal = self.reveal_nav_frames > 0;
        self.reveal_nav_frames = self.reveal_nav_frames.saturating_sub(1);
        self.last_nav_page = Some(self.page);
        ui.vertical_centered(|ui| {
            ui.label(widgets::strong("PC", 21.0))
                .on_hover_text("PCSweep");
            ui.add_space(10.0);
            ScrollArea::vertical()
                .id_salt("compact_navigation")
                .max_height((ui.available_height() - 120.0).max(80.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for group in nav_groups() {
                        for &(page, icon, label) in group {
                            let response = ui
                                .add(
                                    egui::Button::new(widgets::icon(
                                        icon,
                                        20.0,
                                        if self.page == page { ACCENT } else { TEXT_DIM },
                                    ))
                                    .fill(if self.page == page {
                                        ACCENT_SOFT
                                    } else {
                                        SIDEBAR
                                    })
                                    .min_size(vec2(40.0, 38.0)),
                                )
                                .on_hover_text(label);
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::SelectableLabel,
                                    true,
                                    self.page == page,
                                    label,
                                )
                            });
                            if response.clicked() {
                                self.page = page;
                            }
                            if self.page == page && reveal {
                                response.scroll_to_me(Some(Align::Center));
                            }
                        }
                        ui.add_space(8.0);
                    }
                });
        });
        ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
            if ui
                .add(widgets::small(widgets::icon(ic::TOOLS, 19.0, TEXT_DIM)))
                .on_hover_text("PCSweep options and text size")
                .clicked()
            {
                self.app_options_open = true;
            }
            if self.is_admin {
                ui.label(widgets::icon(ic::SHIELD, 17.0, GOOD))
                    .on_hover_text("Running as administrator");
            } else if ui
                .add_enabled(
                    !self.relaunch.running(),
                    widgets::small(widgets::icon(ic::SHIELD, 19.0, WARN)),
                )
                .on_hover_text("Restart as administrator")
                .clicked()
            {
                self.relaunch_admin(&ctx);
            }
        });
    }

    pub(crate) fn sidebar(&mut self, ui: &mut Ui) {
        if self.compact_navigation(ui.ctx()) {
            self.compact_sidebar(ui);
            return;
        }
        let ctx = ui.ctx().clone();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("pcsweep.nav_compact"), false));
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, Rounding::same(10.0), ACCENT_STRONG);
            ui.painter()
                .circle_stroke(rect.center(), 9.0, Stroke::new(2.5_f32, Color32::WHITE));
            ui.painter()
                .circle_filled(rect.center(), 3.0, Color32::WHITE);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    RichText::new("PCSweep")
                        .font(widgets::semibold(19.0))
                        .color(TEXT),
                );
                ui.label(
                    RichText::new("Keep your PC clean and quick")
                        .size(11.5)
                        .color(TEXT_FAINT),
                );
            });
        });
        ui.add_space(22.0);

        let junk_badge = if self.cleaner.scanned_once {
            let b = self.cleaner.recommended_total();
            if b > 0 {
                Some(fmt_bytes(b))
            } else {
                None
            }
        } else {
            None
        };
        let startup_badge = if self.startup.items.is_empty() {
            None
        } else {
            Some(format!("{} on", self.startup.enabled_count()))
        };

        let nav = nav_groups();
        if self.last_nav_page != Some(self.page) {
            // ScrollArea settles its viewport over the first layout passes.
            self.reveal_nav_frames = 3;
        }
        let reveal_selected = self.reveal_nav_frames > 0;
        self.reveal_nav_frames = self.reveal_nav_frames.saturating_sub(1);
        self.last_nav_page = Some(self.page);
        let nav_height = (ui.available_height() - 134.0).max(100.0);
        let mut navigation = ScrollArea::vertical()
            .id_salt("navigation_scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .max_height(nav_height)
            .auto_shrink([false, true]);
        if reveal_selected {
            let row_height = 38.0 + ui.spacing().item_spacing.y + 1.0;
            let mut top = 0.0;
            let mut selected = 0.0;
            for group in nav {
                for &(page, _, _) in group {
                    if self.page == page {
                        selected = top + 19.0;
                    }
                    top += row_height;
                }
                top += 14.0;
            }
            navigation = navigation.vertical_scroll_offset((selected - nav_height / 2.0).max(0.0));
        }
        navigation.show(ui, |ui| {
            for group in nav {
                for &(page, icon_char, label) in group {
                    let badge = match page {
                        Page::Cleaner => junk_badge.as_deref(),
                        Page::Startup => startup_badge.as_deref(),
                        Page::Updates if self.updates.has_available_update() => Some("New"),
                        _ => None,
                    };
                    let response =
                        widgets::nav_item(ui, icon_char, label, self.page == page, badge);
                    if response.clicked() {
                        if page == Page::Startup {
                            self.startup.reload();
                        }
                        self.page = page;
                    }
                    ui.add_space(1.0);
                }
                ui.add_space(14.0);
            }
        });

        let mut want_relaunch = false;
        let busy = self.relaunch.running();
        let is_admin = self.is_admin;
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(widgets::faint(format!(
                "Version {}",
                env!("CARGO_PKG_VERSION")
            )));
            ui.add_space(6.0);
            if ui.add(widgets::small("App options")).clicked() {
                self.app_options_open = true;
            }
            ui.add_space(4.0);
            if is_admin {
                ui.horizontal(|ui| {
                    ui.label(widgets::icon(ic::SHIELD, 13.0, GOOD));
                    ui.label(
                        RichText::new("Running as administrator")
                            .color(GOOD)
                            .size(12.5),
                    );
                });
            } else {
                ui.label(widgets::faint("Unlocks system-level cleanup"));
                let w = ui.available_width();
                let btn = widgets::ghost(widgets::icon_text(
                    ic::SHIELD,
                    "Restart as admin",
                    13.5,
                    WARN,
                ))
                .min_size(vec2(w, 34.0));
                if ui.add_enabled(!busy, btn).clicked() {
                    want_relaunch = true;
                }
            }
        });
        if want_relaunch {
            self.relaunch_admin(&ctx);
        }
    }

    // ------------------------------------------------------------ activity

    pub(crate) fn render_page(&mut self, ui: &mut Ui) {
        match self.page {
            Page::Dashboard => self.page_dashboard(ui),
            Page::Cleaner => self.page_cleaner(ui),
            Page::Analyzer => self.page_analyzer(ui),
            Page::Duplicates => self.page_duplicates(ui),
            Page::Apps => self.page_apps(ui),
            Page::Startup => self.page_startup(ui),
            Page::Memory => self.page_memory(ui),
            Page::Privacy => self.page_privacy(ui),
            Page::Security => self.page_security(ui),
            Page::Background => self.page_background(ui),
            Page::Ads => self.page_ads(ui),
            Page::Network => self.page_network(ui),
            Page::Power => self.page_power(ui),
            Page::Updates => self.page_updates(ui),
            Page::Tools => self.page_tools(ui),
            Page::Activity => self.page_activity(ui),
            Page::Storage => self.page_storage(ui),
            Page::WindowsControls => self.page_windows_controls(ui),
            Page::Speed => self.page_speed(ui),
            Page::FileChanges => self.page_file_changes(ui),
        }
    }

    pub(crate) fn render_readable_page(&mut self, ui: &mut Ui) {
        if self.appearance.scale > 100 {
            // Keep full controls reachable at enlarged text sizes instead
            // of shrinking the user's chosen text back down to fit.
            let area = ui.available_size().max(vec2(640.0, 680.0));
            egui::ScrollArea::both()
                .id_salt("readable_page")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // A bounded page canvas keeps virtualized inner lists
                    // useful when larger text leaves little vertical room.
                    ui.set_width(area.x);
                    ui.set_height(area.y);
                    self.render_page(ui);
                });
        } else {
            self.render_page(ui);
        }
    }

    #[cfg(test)]
    pub(crate) fn test_fixture() -> Self {
        Self {
            page: Page::Dashboard,
            is_admin: false,
            sys: crate::system::SysMon::preview_fixture(),
            cleaner: CleanerState::new(),
            analyzer: Default::default(),
            dups: Default::default(),
            startup: Default::default(),
            apps: crate::apps::preview_fixture(),
            tools: Default::default(),
            mem: crate::processes::preview_fixture(),
            privacy: crate::privacy::preview_fixture(),
            preferences: crate::preferences::preview_fixture(),
            security: crate::security::preview_fixture(),
            background: crate::background::preview_fixture(),
            updates: crate::updates::preview_fixture(),
            ads: crate::ads::preview_fixture(),
            network: crate::network::preview_fixture(),
            power: crate::power::preview_fixture(),
            history: Default::default(),
            storage: crate::storage::preview_fixture(),
            windows_controls: crate::windows_controls::preview_fixture(),
            speed: crate::speed::preview_fixture(),
            file_changes: crate::file_changes::preview_fixture(),
            modal: None,
            toasts: Vec::new(),
            relaunch: Default::default(),
            activity_filter: String::new(),
            last_nav_page: None,
            reveal_nav_frames: 0,
            modal_since: None,
            last_window_size: [1320.0, 860.0],
            tray: crate::tray::Tray::inert(),
            keep_background: false,
            background_hidden: false,
            app_options_open: false,
            tray_error: None,
            force_exit: false,
            appearance: Default::default(),
        }
    }

    fn page_activity(&mut self, ui: &mut Ui) {
        if self.history.open {
            self.page_history(ui);
            return;
        }
        widgets::page_header(
            ui,
            "Activity log",
            "Everything PCSweep has cleaned, moved or changed, newest first. It's saved on disk, so earlier sessions are here too.",
        );
        let path = util::log_path();
        let lines = util::log_lines();
        ui.horizontal_wrapped(|ui| {
            if ui.add(widgets::small("Windows file changes")).clicked() {
                self.page = Page::FileChanges;
            }
            if ui.add(widgets::small("Open log file")).clicked() {
                util::open_path(&path);
            }
            if ui.add(widgets::small("Show in Explorer")).clicked() {
                util::show_in_explorer(&path);
            }
            let history = widgets::icon_text(ic::HISTORY, "Cleanup history", 13.0, TEXT);
            if ui.add(widgets::small(history)).clicked() {
                self.history.open = true;
                self.history.refresh(ui.ctx());
            }
            ui.label(widgets::faint(path.display().to_string()));
        });
        ui.horizontal(|ui| {
            widgets::search_box(ui, &mut self.activity_filter, "Search activity", 260.0);
            if !self.activity_filter.is_empty() && ui.add(widgets::small("Clear")).clicked() {
                self.activity_filter.clear();
            }
            if ui.add(widgets::small("Copy visible log")).clicked() {
                let query = self.activity_filter.trim().to_lowercase();
                let text = lines
                    .iter()
                    .rev()
                    .filter(|line| query.is_empty() || line.to_lowercase().contains(&query))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.output_mut(|output| output.copied_text = text);
            }
        });
        ui.add_space(4.0);
        let query = self.activity_filter.trim().to_lowercase();
        let visible: Vec<_> = lines
            .iter()
            .rev()
            .filter(|line| query.is_empty() || line.to_lowercase().contains(&query))
            .collect();
        ui.label(widgets::faint(format!(
            "{} of {} entries",
            visible.len(),
            lines.len()
        )));
        widgets::card(ui, |ui| {
            ScrollArea::vertical()
                .id_salt("activity_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if visible.is_empty() {
                        ui.label(widgets::dim(if query.is_empty() {
                            "No activity recorded yet."
                        } else {
                            "No activity matches your search."
                        }));
                    }
                    for &l in &visible {
                        ui.label(RichText::new(l).monospace().size(12.5).color(TEXT_DIM));
                    }
                });
        });
    }

    // ------------------------------------------------------------ overlays

    pub(crate) fn modal_ui(&mut self, ctx: &egui::Context) {
        let Some(modal) = &self.modal else {
            self.modal_since = None;
            return;
        };
        let since = *self.modal_since.get_or_insert_with(Instant::now);
        let armed = since.elapsed() >= MODAL_ARMING;
        if !armed {
            ctx.request_repaint_after(MODAL_ARMING);
        }
        ctx.layer_painter(LayerId::new(Order::PanelResizeLine, Id::new("modal_dim")))
            .rect_filled(
                ctx.screen_rect(),
                Rounding::ZERO,
                Color32::from_black_alpha(160),
            );

        let mut decision: Option<bool> = None;
        egui::Window::new("pcsweep_confirm")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .frame(
                Frame::window(&ctx.style())
                    .fill(CARD)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .rounding(Rounding::same(16.0))
                    .inner_margin(Margin::same(24.0)),
            )
            .show(ctx, |ui| {
                ui.set_width(470.0);
                ScrollArea::vertical()
                    .id_salt("confirmation_content")
                    .max_height((ctx.screen_rect().height() - 160.0).clamp(120.0, 620.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (c, col) = if modal.danger {
                                (ic::WARN, BAD)
                            } else {
                                (ic::INFO, ACCENT)
                            };
                            ui.label(widgets::icon(c, 20.0, col));
                            ui.add(Label::new(widgets::strong(&modal.title, 19.0)).wrap());
                        });
                        ui.add_space(4.0);
                        ui.label(RichText::new(&modal.body).color(TEXT_DIM));
                        if !modal.details.is_empty() {
                            ui.add_space(4.0);
                            // One scroll region per dialog: the details grow inside the
                            // content scroll, so its scrollbar sits at the dialog's edge.
                            Frame::none()
                                .fill(BG)
                                .rounding(Rounding::same(8.0))
                                .inner_margin(Margin::same(10.0))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    for d in &modal.details {
                                        ui.add(Label::new(widgets::faint(d)).wrap())
                                            .on_hover_text(d);
                                    }
                                });
                        }
                        if let Some(n) = &modal.note {
                            ui.add_space(2.0);
                            ui.label(RichText::new(n).color(WARN).size(13.0));
                        }
                    });
                ui.add_space(12.0);
                let w = ui.available_width();
                ui.allocate_ui_with_layout(
                    vec2(w, 36.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        let btn = if modal.danger {
                            widgets::danger(modal.confirm.clone())
                        } else {
                            widgets::primary(modal.confirm.clone())
                        };
                        if ui.add_enabled(armed, btn).clicked() {
                            decision = Some(true);
                        }
                        if ui.add(widgets::ghost("Cancel")).clicked() {
                            decision = Some(false);
                        }
                    },
                );
            });
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            decision = Some(false);
        }
        if let Some(confirmed) = decision {
            self.modal_since = None;
            if let Some(m) = self.modal.take() {
                if confirmed {
                    self.execute(m.action, ctx);
                }
            }
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|t| t.born.elapsed() < Duration::from_secs(6));
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(Id::new("pcsweep_toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -20.0))
            .order(Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                for t in &self.toasts {
                    let (c, col) = match t.kind {
                        ToastKind::Info => (ic::INFO, ACCENT),
                        ToastKind::Success => (ic::CHECK, GOOD),
                        ToastKind::Error => (ic::WARN, BAD),
                    };
                    Frame::none()
                        .fill(CARD_HI)
                        .stroke(Stroke::new(1.0_f32, mix(CARD_HI, col, 0.5)))
                        .rounding(Rounding::same(12.0))
                        .inner_margin(Margin::symmetric(14.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(widgets::icon(c, 15.0, col));
                                ui.add(Label::new(RichText::new(&t.text).color(TEXT)).wrap());
                            });
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_tray(ctx);
        if ctx.input(|i| i.viewport().close_requested())
            && hide_on_close(self.force_exit, self.keep_background, self.tray.available())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.background_hidden = true;
        }
        self.tray
            .background_poll(self.background_hidden, self.any_running());
        if self.background_hidden {
            self.poll(ctx);
            self.tray.background_poll(true, self.any_running());
            self.toasts
                .retain(|t| t.born.elapsed() < Duration::from_secs(7));
            ctx.request_repaint_after(Duration::from_secs(if self.any_running() { 1 } else { 30 }));
            return;
        }
        theme::fit_to_viewport(ctx);
        let size = ctx.screen_rect().size() * ctx.zoom_factor();
        if size.x.is_finite() && size.y.is_finite() && size.x >= 900.0 && size.y >= 520.0 {
            self.last_window_size = [size.x, size.y];
        }
        // Re-apply the theme if Windows switches light/dark mode underneath us.
        if ctx.style().visuals.panel_fill != theme::BG {
            theme::apply(ctx);
        }
        let want_procs = matches!(self.page, Page::Dashboard | Page::Memory);
        self.sys.tick(want_procs);
        self.poll(ctx);

        let enabled = self.modal.is_none();
        egui::SidePanel::left("nav")
            .exact_width(self.navigation_width(ctx))
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::none()
                    .fill(SIDEBAR)
                    .inner_margin(Margin::symmetric(14.0, 20.0)),
            )
            .show(ctx, |ui| {
                ui.add_enabled_ui(enabled, |ui| self.sidebar(ui));
            });

        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(30.0, 22.0)),
            )
            .show(ctx, |ui| {
                if self.ads.trial_pending() {
                    widgets::banner(ui, WARN, ic::GLOBE, |ui| {
                        ui.vertical(|ui| {
                            ui.label("A DNS trial is waiting for your decision. It will restore the previous settings unless you keep it.");
                            if ui.add_enabled(enabled, widgets::small("Review DNS trial")).clicked() {
                                self.page = Page::Ads;
                                self.ads.reveal_trial();
                            }
                        });
                    });
                    ui.add_space(8.0);
                }
                ui.add_enabled_ui(enabled, |ui| self.render_readable_page(ui));
            });

        self.modal_ui(ctx);
        self.app_options_ui(ctx);
        self.toasts_ui(ctx);
        crate::updates::confirm_update_startup();

        let ms = if self.any_running() { 120 } else { 1000 };
        ctx.request_repaint_after(Duration::from_millis(ms));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        use winreg::{enums::HKEY_CURRENT_USER, RegKey};
        if let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(SESSION_KEY) {
            if let Ok(page) = serde_json::to_string(&self.page) {
                let _ = key.set_value("Page", &page);
            }
            let _ = key.set_value(
                "Width",
                &(self.last_window_size[0].round().clamp(900.0, 7680.0) as u32),
            );
            let _ = key.set_value(
                "Height",
                &(self.last_window_size[1].round().clamp(520.0, 4320.0) as u32),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_close_never_traps_forced_updates_quit_or_failed_tray() {
        assert!(hide_on_close(false, true, true));
        assert!(!hide_on_close(true, true, true));
        assert!(!hide_on_close(false, true, false));
        assert!(!hide_on_close(false, false, true));
    }

    #[test]
    fn elevation_cannot_interrupt_an_active_operation() {
        let ctx = egui::Context::default();
        let mut app = App::test_fixture();
        let (release, wait) = std::sync::mpsc::channel();
        app.cleaner.clean = Task::spawn(&ctx, move || {
            let _ = wait.recv();
            crate::cleaner::CleanReport::default()
        });
        app.relaunch_admin(&ctx);
        assert!(!app.relaunch.running());
        assert!(app
            .toasts
            .iter()
            .any(|t| t.text.contains("current operation")));
        release.send(()).unwrap();
    }
}
