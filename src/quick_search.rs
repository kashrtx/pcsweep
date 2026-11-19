use crate::app::{self, App, Page};
use crate::recovery::RecoveryAction;
use crate::running::{Identity, Snapshot};
use crate::theme::*;
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Align2, Id, Key, Modifiers, ScrollArea, TextEdit, Vec2};

#[derive(Default)]
pub struct QuickSearch {
    pub open: bool,
    query: String,
    selected: usize,
    focus: bool,
    reveal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
    Page(Page),
    Run,
    Bin,
    Updates,
    OpenExplorer,
    RestartExplorer,
    Recent,
    Appearance,
    Process(Identity),
}

struct ResultItem<'a> {
    destination: Destination,
    title: &'a str,
    subtitle: &'a str,
    icon: char,
}

fn score(title: &str, keywords: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let title = title.to_lowercase();
    if title.starts_with(query) {
        return Some(0);
    }
    if !query
        .split_whitespace()
        .all(|part| title.contains(part) || keywords.contains(part))
    {
        return None;
    }
    Some(if title.contains(query) { 1 } else { 2 })
}

fn items<'a>(snapshot: &'a Snapshot, query: &str) -> Vec<ResultItem<'a>> {
    let query = query.trim().to_lowercase();
    let mut found = Vec::new();
    let mut add = |title: &'a str, subtitle: &'a str, keywords: &str, icon, destination| {
        if let Some(rank) = score(title, keywords, &query) {
            found.push((
                rank,
                ResultItem {
                    destination,
                    title,
                    subtitle,
                    icon,
                },
            ));
        }
    };
    for (title, keywords, icon, destination) in [
        (
            "Review Recycle Bin",
            "trash deleted files size original path recycle",
            ic::CLEAN,
            Destination::Bin,
        ),
        (
            "Run an app or command",
            "launch start open file folder ctrl+r",
            ic::PLAY,
            Destination::Run,
        ),
        (
            "Check for PCSweep updates",
            "version upgrade download release",
            ic::UPDATE,
            Destination::Updates,
        ),
        (
            "Restore taskbar",
            "open explorer.exe taskbar shell file desktop recover",
            ic::APPS,
            Destination::OpenExplorer,
        ),
        (
            "Restart Explorer…",
            "explorer.exe taskbar shell file desktop restore",
            ic::UPDATE,
            Destination::RestartExplorer,
        ),
        (
            "Recently closed apps",
            "relaunch reopen desktop process recovery",
            ic::HISTORY,
            Destination::Recent,
        ),
        (
            "Appearance and text size",
            "skin theme glass aero retro accessibility scale bold",
            ic::TOOLS,
            Destination::Appearance,
        ),
    ] {
        add(title, "Action", keywords, icon, destination);
    }
    for group in app::nav_groups() {
        for &(page, icon, title) in group {
            let words = match page {
                Page::Cleaner => "cache temporary files space cleanup",
                Page::Analyzer => "disk drives folders storage scan map",
                Page::Storage => "disk smart trim ssd defragment optimize",
                Page::Memory => "ram cpu task manager processes running",
                Page::Apps => "installed software uninstall packages",
                Page::Network => "ethernet wifi nic network",
                Page::WindowsControls => "windows update pause resume",
                Page::Ads => "dns adguard ublock extensions",
                _ => "",
            };
            add(title, "Page", words, icon, Destination::Page(page));
        }
    }
    // Empty searches emphasize actions and pages. Process matches use the
    // actual window-owning PID, never a name or command supplied by the query.
    if !query.is_empty() {
        for process in &snapshot.processes {
            if !process.windows.is_empty() {
                let keywords = format!(
                    "{} {}",
                    process.identity.pid,
                    process.windows[0].title.to_lowercase()
                );
                add(
                    &process.name,
                    "Running app",
                    &keywords,
                    ic::APPS,
                    Destination::Process(process.identity),
                );
            }
        }
    }
    found.sort_by_key(|(score, _)| *score);
    found.into_iter().take(36).map(|(_, item)| item).collect()
}

impl App {
    pub(crate) fn open_quick_search(&mut self) {
        self.app_options_open = false;
        self.launcher.open = false;
        self.quick_search = QuickSearch {
            open: true,
            focus: true,
            ..Default::default()
        };
        self.sys.refresh_processes_soon();
    }

    pub(crate) fn quick_search_shortcut(&mut self, ctx: &egui::Context) {
        if self.modal.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::K)) {
            if self.quick_search.open {
                self.quick_search.open = false;
            } else {
                self.open_quick_search();
            }
        }
    }

    pub(crate) fn quick_search_ui(&mut self, ctx: &egui::Context) {
        if !self.quick_search.open || self.modal.is_some() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.quick_search.open = false;
            return;
        }
        let direction = ctx.input_mut(|i| {
            i32::from(i.consume_key(Modifiers::NONE, Key::ArrowDown))
                - i32::from(i.consume_key(Modifiers::NONE, Key::ArrowUp))
        });
        let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let search = &mut self.quick_search;
        let screen = ctx.screen_rect();
        let width = (screen.width() - 56.0).clamp(270.0, 620.0);
        let height = (screen.height() - 164.0).clamp(100.0, 380.0);
        let mut choice = None;
        let mut open = true;
        egui::Window::new("Quick search")
            .id(Id::new("pcsweep_quick_search"))
            .open(&mut open)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 36.0))
            .collapsible(false)
            .resizable(false)
            .default_width(width)
            .show(ctx, |ui| {
                ui.set_width(width);
                let field = ui.add(
                    TextEdit::singleline(&mut search.query)
                        .hint_text("Find a page, action, or running app")
                        .desired_width(f32::INFINITY)
                        .char_limit(120),
                );
                if search.focus {
                    field.request_focus();
                    search.focus = false;
                }
                if field.changed() {
                    search.selected = 0;
                    search.reveal = true;
                }
                let results = items(&self.sys.processes, &search.query);
                let last = results.len().saturating_sub(1);
                search.selected =
                    (search.selected as i32 + direction).clamp(0, last as i32) as usize;
                search.reveal |= direction != 0;
                if results.is_empty() {
                    ui.add_space(8.0);
                    ui.label(widgets::dim("No matching pages, actions, or apps."));
                }
                ScrollArea::vertical()
                    .id_salt("quick_search_results")
                    .max_height(height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (index, item) in results.iter().enumerate() {
                            let selected = index == search.selected;
                            let mut label = format!("{}  ·  {}", item.title, item.subtitle);
                            if let Destination::Process(id) = item.destination {
                                label.push_str(&format!("  ·  PID {}", id.pid));
                            }
                            let button = egui::Button::new(widgets::icon_text(
                                item.icon,
                                &label,
                                14.0,
                                color_ink(),
                            ))
                            .fill(if selected {
                                color_selected()
                            } else {
                                color_surface()
                            })
                            .stroke(if selected {
                                egui::Stroke::new(1.0_f32, color_accent())
                            } else {
                                egui::Stroke::NONE
                            })
                            .rounding(corners(8.0))
                            .min_size(Vec2::new(ui.available_width(), 34.0));
                            let result = ui.add(button.wrap());
                            if result.clicked() || (selected && enter) {
                                choice = Some(item.destination);
                            }
                            if selected && search.reveal {
                                result.scroll_to_me(Some(Align::Center));
                            }
                        }
                    });
                search.reveal = false;
                ui.label(widgets::faint("↑ ↓  Move     Enter  Open     Esc  Close"));
            });
        self.quick_search.open = open && choice.is_none();
        match choice {
            Some(Destination::Page(page)) => self.page = page,
            Some(Destination::Run) => self.open_launcher(ctx),
            Some(Destination::Bin) => self.open_bin(ctx),
            Some(Destination::Updates) => {
                self.page = Page::Updates;
                self.begin_update_check(ctx);
            }
            Some(Destination::OpenExplorer) => {
                self.execute_recovery(RecoveryAction::Explorer { restart: false }, ctx)
            }
            Some(Destination::RestartExplorer) if !self.mutations_running() => {
                self.review_explorer_restart()
            }
            Some(Destination::Recent) => self.show_recent_apps(),
            Some(Destination::Appearance) => self.app_options_open = true,
            Some(Destination::Process(id)) => self.inspect_process(id),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_storage_by_synonym_and_applies_multiword_search() {
        let snapshot = Snapshot::default();
        let destinations = |query| -> Vec<_> {
            items(&snapshot, query)
                .into_iter()
                .map(|result| result.destination)
                .collect()
        };
        assert!(destinations("ssd trim").contains(&Destination::Page(Page::Storage)));
        assert!(destinations("windows pause").contains(&Destination::Page(Page::WindowsControls)));
        assert_eq!(
            destinations("Restart Explorer").first(),
            Some(&Destination::RestartExplorer)
        );
        assert!(destinations("powershell -encodedcommand").is_empty());
    }
}
