//! A local Run box for apps, files, folders, and Windows Shell targets.
use crate::{
    app::{App, ToastKind},
    desktop,
    task::Task,
    theme::*,
    util,
    widgets::{self, ic},
};
use eframe::egui::{self, Align, Align2, Id, Key, Modifiers, ScrollArea, TextEdit, Vec2};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::Interface,
    Win32::UI::Shell::{
        FOLDERID_AppsFolder, IShellItem2, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
    },
};

const HISTORY: usize = 8;

#[derive(Clone)]
struct AppLink {
    name: String,
    search: String,
    target: String,
}

#[derive(Default)]
pub struct Launcher {
    pub open: bool,
    input: String,
    focus: bool,
    selected: Option<usize>,
    apps: Vec<AppLink>,
    catalog: Task<Result<Vec<AppLink>, String>>,
    updated: Option<Instant>,
    browse: Task<Option<PathBuf>>,
    opening: Task<Result<String, String>>,
    recent: Vec<String>,
    error: Option<String>,
}

impl Launcher {
    pub fn busy(&self) -> bool {
        self.catalog.running() || self.browse.running() || self.opening.running()
    }
}

fn published_apps() -> Result<Vec<AppLink>, String> {
    let _apartment = desktop::Apartment::enter().map_err(|e| e.to_string())?;
    let (items, _) = desktop::items(&FOLDERID_AppsFolder, 2048)
        .map_err(|e| format!("Could not read Windows’ app list. {e}"))?;
    let key = desktop::property_key("System.AppUserModel.ID");
    let mut links = Vec::new();
    for item in items {
        let Some(name) = (unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) })
            .ok()
            .and_then(desktop::owned_string)
        else {
            continue;
        };
        let app_id = key
            .and_then(|key| {
                item.cast::<IShellItem2>().ok().and_then(|item| {
                    unsafe { item.GetString(&key) }
                        .ok()
                        .and_then(desktop::owned_string)
                })
            })
            .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control));
        let target = app_id
            .map(|id| format!(r"shell:AppsFolder\{id}"))
            .or_else(|| {
                unsafe { item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }
                    .ok()
                    .and_then(desktop::owned_string)
            });
        let Some(target) = target.filter(|target| target.len() <= 4096 && !target.is_empty())
        else {
            continue;
        };
        links.push(AppLink {
            search: name.to_lowercase(),
            name,
            target,
        });
    }
    links.sort_by(|a, b| {
        a.search
            .cmp(&b.search)
            .then_with(|| a.target.cmp(&b.target))
    });
    links.dedup_by(|a, b| a.name == b.name && a.target == b.target);
    Ok(links)
}

fn matching<'a>(links: &'a [AppLink], input: &str) -> Vec<&'a AppLink> {
    let text = input.trim().to_lowercase();
    if text.is_empty() {
        return Vec::new();
    }
    let words: Vec<_> = text.split_whitespace().collect();
    let mut found: Vec<_> = links
        .iter()
        .filter(|app| words.iter().all(|word| app.search.contains(word)))
        .collect();
    found.sort_by_key(|app| (app.search != text, !app.search.starts_with(&text)));
    found.truncate(24);
    found
}

fn parse(input: &str, exists: impl Fn(&Path) -> bool) -> Result<(String, String), String> {
    let value = input.trim();
    if value.is_empty() || value.len() > 8192 || value.chars().any(|c| c.is_control() && c != '\t')
    {
        return Err("Enter a valid app, command, file path, or web address.".into());
    }
    if let Some(rest) = value.strip_prefix('"') {
        let (target, tail) = rest
            .split_once('"')
            .ok_or("Close the quotation mark around the path.")?;
        if target.is_empty() || tail.starts_with(|c: char| !c.is_whitespace()) {
            return Err("Put a space between the quoted path and its arguments.".into());
        }
        return Ok((
            util::expand_env(target),
            util::expand_env(tail.trim_start()),
        ));
    }
    let expanded = util::expand_env(value);
    if !expanded.contains('"') && exists(Path::new(&expanded)) {
        return Ok((expanded, String::new()));
    }
    let at = value.find(char::is_whitespace).unwrap_or(value.len());
    let target = &value[..at];
    let arguments = value[at..].trim_start();
    if !arguments.is_empty()
        && (target.contains(['\\', '/']) || target.as_bytes().get(1) == Some(&b':'))
        && !target
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .contains('.')
    {
        return Err("Put quotation marks around a path that contains spaces.".into());
    }
    if target.contains('"') {
        return Err("Put quotation marks around the entire executable path.".into());
    }
    Ok((util::expand_env(target), util::expand_env(arguments)))
}

enum Request {
    Command(String),
    App(AppLink),
}

impl Request {
    fn open(self) -> Result<String, String> {
        match self {
            Self::Command(command) => {
                let (target, args) = parse(&command, |path| path.is_absolute() && path.exists())?;
                desktop::open(OsStr::new(&target), &args)?;
                Ok(command)
            }
            Self::App(app) => {
                desktop::open(OsStr::new(&app.target), "")?;
                Ok(app.name)
            }
        }
    }
}

impl App {
    pub(crate) fn open_launcher(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() {
            return;
        }
        self.quick_search.open = false;
        self.app_options_open = false;
        self.launcher.open = true;
        self.launcher.focus = true;
        self.launcher.selected = None;
        if !self.launcher.catalog.running()
            && self
                .launcher
                .updated
                .is_none_or(|stamp| stamp.elapsed() > Duration::from_secs(300))
        {
            self.launcher.catalog = Task::spawn(ctx, published_apps);
        }
    }

    pub(crate) fn launcher_shortcut(&mut self, ctx: &egui::Context) {
        if self.modal.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::R)) {
            if self.launcher.open {
                self.launcher.open = false;
            } else {
                self.open_launcher(ctx);
            }
        }
    }

    pub(crate) fn poll_launcher(&mut self) {
        if let Some(result) = self.launcher.catalog.poll() {
            self.launcher.updated = Some(Instant::now());
            if let Ok(links) = result.and_then(|r| r) {
                self.launcher.apps = links;
            }
        }
        if let Some(result) = self.launcher.browse.poll() {
            match result {
                Ok(Some(file)) => {
                    self.launcher.input = format!("\"{}\"", file.display());
                    self.launcher.focus = true;
                    self.launcher.selected = None;
                }
                Ok(None) => self.launcher.focus = true,
                Err(error) => self.launcher.error = Some(error),
            }
        }
        if let Some(result) = self.launcher.opening.poll() {
            match result.and_then(|r| r) {
                Ok(command) => {
                    self.launcher.recent.retain(|old| old != &command);
                    self.launcher.recent.insert(0, command);
                    self.launcher.recent.truncate(HISTORY);
                    self.launcher.open = false;
                    self.launcher.input.clear();
                    self.toast(ToastKind::Success, "Opened through Windows.");
                }
                Err(error) => self.launcher.error = Some(error),
            }
        }
    }

    pub(crate) fn launcher_ui(&mut self, ctx: &egui::Context) {
        if !self.launcher.open || self.modal.is_some() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.launcher.open = false;
            return;
        }
        let select = ctx.input_mut(|i| {
            i32::from(i.consume_key(Modifiers::NONE, Key::ArrowDown))
                - i32::from(i.consume_key(Modifiers::NONE, Key::ArrowUp))
        });
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let launcher = &mut self.launcher;
        let busy = launcher.opening.running() || launcher.browse.running();
        let screen = ctx.screen_rect();
        let width = screen.width().min(630.0) - 48.0;
        let list_height = (screen.height() - 244.0).clamp(96.0, 290.0);
        let mut open = true;
        let mut requested = None;
        let mut browse = false;
        egui::Window::new("Run")
            .id(Id::new("pcsweep_run"))
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 48.0))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(width)
            .show(ctx, |ui| {
                ui.add(
                    egui::Label::new(widgets::dim("Open an app, file, folder, or web address."))
                        .wrap(),
                );
                let field = ui.add_enabled(
                    !busy,
                    TextEdit::singleline(&mut launcher.input)
                        .char_limit(2048)
                        .hint_text("App name, command or path")
                        .desired_width(f32::INFINITY),
                );
                if launcher.focus {
                    field.request_focus();
                    launcher.focus = false;
                }
                if field.changed() {
                    launcher.error = None;
                    launcher.selected = None;
                }
                let links = matching(&launcher.apps, &launcher.input);
                if !links.is_empty() && select != 0 {
                    let next = launcher.selected.map_or(-1, |i| i as i32) + select;
                    launcher.selected = usize::try_from(next).ok().map(|i| i.min(links.len() - 1));
                }
                if let Some(error) = &launcher.error {
                    ui.colored_label(color_error(), error);
                }

                if !links.is_empty() {
                    ScrollArea::vertical()
                        .id_salt("run_app_matches")
                        .max_height(list_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, app) in links.iter().enumerate() {
                                let selected = launcher.selected == Some(i);
                                let label =
                                    widgets::icon_text(ic::APPS, &app.name, 14.0, color_ink());
                                let row = ui.add_enabled(
                                    !busy,
                                    widgets::ghost(label)
                                        .fill(if selected {
                                            color_selected()
                                        } else {
                                            color_raised()
                                        })
                                        .min_size(Vec2::new(ui.available_width(), 29.0)),
                                );
                                if row.clicked() {
                                    requested = Some(Request::App((*app).clone()));
                                }
                                if selected && select != 0 {
                                    row.scroll_to_me(Some(Align::Center));
                                }
                            }
                        });
                } else if launcher.input.trim().is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for entry in ["Notepad", "Calculator", "File Explorer"] {
                            if ui.add_enabled(!busy, widgets::small(entry)).clicked() {
                                let command = match entry {
                                    "Calculator" => "calc.exe",
                                    "File Explorer" => "explorer.exe",
                                    _ => "notepad.exe",
                                };
                                requested = Some(Request::Command(command.into()));
                            }
                        }
                    });
                    if !launcher.recent.is_empty() {
                        egui::CollapsingHeader::new("Recent commands").show(ui, |ui| {
                            ScrollArea::vertical()
                                .id_salt("run_recent")
                                .max_height(list_height / 2.0)
                                .show(ui, |ui| {
                                    for command in &launcher.recent {
                                        let response = ui.add_enabled(
                                            !busy,
                                            egui::Button::new(command).truncate(),
                                        );
                                        if response.clicked() {
                                            launcher.input.clone_from(command);
                                            launcher.focus = true;
                                        }
                                    }
                                });
                        });
                    }
                }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    let valid = !busy && !launcher.input.trim().is_empty();
                    if ui.add_enabled(valid, widgets::primary("Open")).clicked() || (valid && enter)
                    {
                        let from_app = launcher
                            .selected
                            .and_then(|i| links.get(i).copied())
                            .or_else(|| {
                                links.first().copied().filter(|app| {
                                    app.name.eq_ignore_ascii_case(launcher.input.trim())
                                })
                            });
                        requested = Some(
                            from_app
                                .map(|app| Request::App(app.clone()))
                                .unwrap_or_else(|| Request::Command(launcher.input.clone())),
                        );
                    }
                    browse = ui.add_enabled(!busy, widgets::ghost("Browse…")).clicked();
                    if busy {
                        ui.spinner();
                    }
                    if launcher.catalog.running() {
                        ui.label(widgets::faint("Loading apps…"));
                    }
                });
            });
        launcher.open = open;
        if browse {
            launcher.browse = Task::spawn(ctx, || {
                rfd::FileDialog::new()
                    .set_title("Choose a file to open")
                    .pick_file()
            });
        }
        if let Some(request) = requested {
            launcher.error = None;
            launcher.opening = Task::spawn(ctx, move || request.open());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_paths_and_literal_arguments_like_run() {
        let exists = |path: &Path| path == Path::new(r"C:\Work files\notes.txt");
        assert_eq!(
            parse(
                r#""C:\Program Files\Editor\edit.exe" /new "three four""#,
                exists
            )
            .unwrap(),
            (
                r"C:\Program Files\Editor\edit.exe".into(),
                r#"/new "three four""#.into()
            )
        );
        assert_eq!(
            parse(r"C:\Work files\notes.txt", exists).unwrap(),
            (r"C:\Work files\notes.txt".into(), String::new())
        );
        assert_eq!(
            parse("notepad.exe report.txt & literal", exists).unwrap(),
            ("notepad.exe".into(), "report.txt & literal".into())
        );
        assert!(parse(r"C:\Program Files\Editor\missing.exe", exists).is_err());
        assert!(parse("calc\nnotepad", exists).is_err());
    }
}
