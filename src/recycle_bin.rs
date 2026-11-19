//! The Shell provides the current user's deleted-item identities and metadata.
//! Removal receives those reviewed identities, never the original file paths.
use crate::{
    app::{Action, App, Modal, Page, ToastKind},
    desktop, journal,
    task::Task,
    theme::*,
    util,
    widgets::{self, ic},
};
use eframe::egui::{self, Align, Layout, ScrollArea, Ui};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};
use windows::{
    core::Interface,
    Win32::{
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER},
        UI::Shell::{
            FOLDERID_RecycleBinFolder, FileOperation, IFileOperation, ILGetSize, IShellItem,
            IShellItem2, SHGetIDListFromObject, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
            SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEEDITING,
        },
    },
};

const LIMIT: usize = 12_000;
const RECYCLE_FOLDER: &str = "shell:RecycleBinFolder";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    id: Vec<u8>,
    name: String,
    folder: Option<String>,
    deleted: Option<u64>,
    size: Option<u64>,
    search: String,
}

impl Entry {
    fn original(&self) -> PathBuf {
        self.folder.as_ref().map_or_else(
            || PathBuf::from(&self.name),
            |folder| PathBuf::from(folder).join(&self.name),
        )
    }

    fn deleted_text(&self) -> Option<String> {
        let seconds = self.deleted?.checked_sub(116_444_736_000_000_000)? / 10_000_000;
        chrono::DateTime::from_timestamp(seconds as i64, 0).map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%b %-d, %Y %H:%M")
                .to_string()
        })
    }
}

#[derive(Default)]
struct Inventory {
    items: Vec<Entry>,
    limited: bool,
    skipped: usize,
}

struct ShellEntry {
    item: IShellItem,
    row: Entry,
}

fn row_id(item: &IShellItem) -> Option<Vec<u8>> {
    let ptr = unsafe { SHGetIDListFromObject(item) }.ok()?;
    if ptr.is_null() {
        return None;
    }
    let size = unsafe { ILGetSize(Some(ptr)) } as usize;
    let bytes = (2..=65_536)
        .contains(&size)
        .then(|| unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) }.to_vec());
    unsafe {
        CoTaskMemFree(Some(ptr.cast()));
    }
    bytes
}

fn enumerate() -> Result<(Vec<ShellEntry>, bool, usize), String> {
    let (items, limited) = desktop::items(&FOLDERID_RecycleBinFolder, LIMIT)
        .map_err(|e| format!("Windows could not read the Recycle Bin. {e}"))?;
    let size = desktop::property_key("System.Size");
    let origin = desktop::property_key("System.Recycle.DeletedFrom");
    let date = desktop::property_key("System.Recycle.DateDeleted");
    let mut found = Vec::new();
    let mut skipped = 0;
    for item in items {
        let name = unsafe { item.GetDisplayName(SIGDN_PARENTRELATIVEEDITING) }
            .ok()
            .and_then(desktop::owned_string)
            .filter(|name| !name.is_empty() && !name.starts_with("$R"))
            .or_else(|| {
                unsafe { item.GetDisplayName(SIGDN_NORMALDISPLAY) }
                    .ok()
                    .and_then(desktop::owned_string)
            });
        let (Some(name), Some(id)) = (name, row_id(&item)) else {
            skipped += 1;
            continue;
        };
        let props = item.cast::<IShellItem2>().ok();
        let folder = props
            .as_ref()
            .and_then(|props| {
                unsafe { props.GetString(&origin?) }
                    .ok()
                    .and_then(desktop::owned_string)
            })
            .filter(|folder| !folder.is_empty());
        let deleted = props
            .as_ref()
            .and_then(|props| {
                let time = unsafe { props.GetFileTime(&date?) }.ok()?;
                Some((u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime))
            })
            .filter(|time| *time > 116_444_736_000_000_000);
        let size = props
            .as_ref()
            .and_then(|props| unsafe { props.GetUInt64(&size?) }.ok());
        let search = format!("{} {}", name, folder.as_deref().unwrap_or("")).to_lowercase();
        found.push(ShellEntry {
            item,
            row: Entry {
                id,
                name,
                folder,
                deleted,
                size,
                search,
            },
        });
    }
    Ok((found, limited, skipped))
}

fn inventory() -> Result<Inventory, String> {
    let _apartment = desktop::Apartment::enter().map_err(|e| e.to_string())?;
    let (found, limited, skipped) = enumerate()?;
    Ok(Inventory {
        items: found.into_iter().map(|entry| entry.row).collect(),
        limited,
        skipped,
    })
}

fn operation(entries: &[&ShellEntry]) -> windows::core::Result<IFileOperation> {
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        op.SetOperationFlags(FOF_SILENT | FOF_NOERRORUI | FOF_NOCONFIRMATION)?;
        for entry in entries {
            op.DeleteItem(&entry.item, None)?;
        }
        Ok(op)
    }
}

fn remove(reviewed: Vec<Entry>) -> Result<String, String> {
    let _apartment = desktop::Apartment::enter().map_err(|e| e.to_string())?;
    let (live, _, _) = enumerate()?;
    let live: HashMap<_, _> = live
        .iter()
        .map(|entry| (entry.row.id.as_slice(), entry))
        .collect();
    let chosen: Vec<_> = reviewed
        .iter()
        .filter_map(|row| {
            live.get(row.id.as_slice())
                .copied()
                .filter(|entry| &entry.row == row)
        })
        .collect();
    let skipped = reviewed.len() - chosen.len();
    if chosen.is_empty() {
        return Err(
            "The selected items have changed. Refresh the Recycle Bin and review the current list."
                .into(),
        );
    }
    let op = operation(&chosen).map_err(|e| format!("Windows could not prepare deletion. {e}"))?;
    let mut journal = journal::Session::begin("Recycle Bin")?;
    let mut ids = Vec::with_capacity(chosen.len());
    for batch in chosen.chunks(128) {
        let paths = batch
            .iter()
            .map(|entry| (entry.row.original(), entry.row.size.unwrap_or(0)))
            .collect::<Vec<_>>();
        ids.extend(journal.intend("Recycle Bin", journal::Act::Delete, &paths)?);
    }
    let performed = unsafe {
        op.PerformOperations()
            .and_then(|()| op.GetAnyOperationsAborted())
    };
    let (remaining, limited, missed) = enumerate().map_err(|error| {
        format!("Windows could not verify the deletion. Check the refreshed list. {error}")
    })?;
    let left: HashSet<_> = remaining
        .iter()
        .map(|entry| entry.row.id.as_slice())
        .collect();
    let completed = performed.as_ref().is_ok_and(|aborted| !aborted.as_bool());
    let verified = !limited && missed == 0;
    let mut deleted = 0usize;
    let mut outcomes = Vec::new();
    for (&id, entry) in ids.iter().zip(&chosen) {
        if left.contains(entry.row.id.as_slice()) {
            outcomes.push((
                id,
                journal::Outcome::Failed,
                Some("The item is still in the Recycle Bin.".into()),
            ));
        } else if completed && verified {
            deleted += 1;
            outcomes.push((id, journal::Outcome::Deleted, None));
        }
        // Leave an unresolved intention when Windows cannot verify the outcome.
    }
    for batch in outcomes.chunks(128) {
        journal.finish(batch)?;
    }
    let uncertain = chosen.len() - outcomes.len();
    if uncertain > 0 {
        return Err(format!("Verified {deleted} deletions. Windows could not confirm {uncertain} outcomes. Review the refreshed list."));
    }
    let kept = reviewed.len() - deleted;
    if kept == 0 {
        Ok(format!(
            "Permanently deleted {deleted} reviewed Recycle Bin items."
        ))
    } else {
        Ok(format!(
            "Deleted {deleted}. Kept {kept}, including {skipped} that changed during review."
        ))
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Sort {
    #[default]
    Newest,
    Largest,
    Name,
}

#[derive(Default)]
pub struct BinState {
    pub open: bool,
    inventory: Option<Inventory>,
    load: Task<Result<Inventory, String>>,
    delete: Task<Result<String, String>>,
    error: Option<String>,
    filter: String,
    sort: Sort,
    visible: Vec<usize>,
    checked: HashSet<usize>,
}

impl BinState {
    pub fn deleting(&self) -> bool {
        self.delete.running()
    }

    fn reindex(&mut self) {
        self.visible.clear();
        let Some(inventory) = &self.inventory else {
            return;
        };
        let needle = self.filter.trim().to_lowercase();
        self.visible.extend(
            inventory
                .items
                .iter()
                .enumerate()
                .filter(|(_, row)| row.search.contains(&needle))
                .map(|(i, _)| i),
        );
        self.visible.sort_unstable_by(|&a, &b| {
            let (a, b) = (&inventory.items[a], &inventory.items[b]);
            match self.sort {
                Sort::Newest => b.deleted.cmp(&a.deleted),
                Sort::Largest => b.size.cmp(&a.size),
                Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
            .then_with(|| a.search.cmp(&b.search))
        });
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        if !self.load.running() && !self.deleting() {
            self.checked.clear();
            self.error = None;
            self.load = Task::spawn(ctx, inventory);
        }
    }
}

impl App {
    pub(crate) fn open_bin(&mut self, ctx: &egui::Context) {
        self.page = Page::Cleaner;
        self.bin.open = true;
        self.bin.refresh(ctx);
    }

    pub(crate) fn poll_bin(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.bin.load.poll() {
            match result.and_then(|r| r) {
                Ok(result) => {
                    self.bin.inventory = Some(result);
                    self.bin.reindex();
                }
                Err(error) => self.bin.error = Some(error),
            }
        }
        if let Some(result) = self.bin.delete.poll() {
            self.bin.refresh(ctx);
            self.start_junk_scan(ctx);
            match result.and_then(|r| r) {
                Ok(message) => self.toast(ToastKind::Success, message),
                Err(error) => self.toast(ToastKind::Error, error),
            }
        }
    }

    pub(crate) fn delete_bin(&mut self, entries: Vec<Entry>, ctx: &egui::Context) {
        self.bin.delete = Task::spawn(ctx, move || remove(entries));
    }

    pub(crate) fn page_bin(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut refresh = false;
        let mut open_shell = false;
        let mut ask = false;
        let busy = self.mutations_running() || self.bin.load.running();
        ui.horizontal_wrapped(|ui| {
            if ui.add(widgets::small("Back to categories")).clicked() {
                self.bin.open = false;
            }
            refresh = ui
                .add_enabled(!busy, widgets::small("Refresh list"))
                .clicked();
            open_shell = ui
                .add(widgets::ghost("Open Recycle Bin in Explorer"))
                .clicked();
            if self.bin.load.running() || self.bin.deleting() {
                ui.spinner();
            }
        });
        widgets::page_header(
            ui,
            "Recycle Bin",
            "Review names, original folders, sizes, and deletion dates before removing items.",
        );
        if self.modal.is_none()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.bin.open = false;
        }

        if let Some(error) = &self.bin.error {
            ui.colored_label(color_error(), error);
        }
        if self.bin.inventory.is_some() {
            let mut reindex = false;
            ui.horizontal_wrapped(|ui| {
                reindex = widgets::search_box(
                    ui,
                    &mut self.bin.filter,
                    "Filter by name or original folder",
                    232.0,
                )
                .changed();
                for (sort, label) in [
                    (Sort::Newest, "Newest"),
                    (Sort::Largest, "Largest"),
                    (Sort::Name, "Name"),
                ] {
                    if ui
                        .add(widgets::chip(label, self.bin.sort == sort))
                        .clicked()
                    {
                        self.bin.sort = sort;
                        reindex = true;
                    }
                }
            });
            if reindex {
                self.bin.reindex();
            }
            let inventory = self.bin.inventory.as_ref().unwrap();
            ui.label(widgets::dim(format!(
                "{} shown · {} in this review",
                util::fmt_num(self.bin.visible.len() as u64),
                util::fmt_num(inventory.items.len() as u64)
            )));
            if inventory.limited || inventory.skipped > 0 {
                ui.label(widgets::dim("Windows could only list part of the bin. Deletion applies to selected, verified items."));
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !busy && !self.bin.visible.is_empty(),
                        widgets::small("Select shown"),
                    )
                    .clicked()
                {
                    self.bin.checked.extend(&self.bin.visible);
                }
                if ui
                    .add_enabled(
                        !self.bin.checked.is_empty(),
                        widgets::small("Clear selection"),
                    )
                    .clicked()
                {
                    self.bin.checked.clear();
                }
                let selected = self.bin.checked.len();
                if selected > 0 {
                    ui.label(widgets::dim(format!("{selected} selected")));
                }
                ask = ui
                    .add_enabled(!busy && selected > 0, widgets::danger("Delete selected…"))
                    .clicked();
            });
            if self.bin.visible.is_empty() {
                ui.label(widgets::dim(if inventory.items.is_empty() {
                    "The Recycle Bin is empty."
                } else {
                    "No items match this filter."
                }));
            } else {
                widgets::tight_card(ui, |ui| {
                    let dates = ui.available_width() >= 520.0;
                    widgets::row(ui, 25.0, |ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if dates {
                                widgets::cell(ui, 146.0, true, widgets::faint("Deleted"));
                            }
                            widgets::cell(ui, 94.0, true, widgets::faint("Size"));
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.label(widgets::faint("File and original folder"));
                            });
                        });
                    });
                    ui.separator();
                    let height =
                        (ui.clip_rect().bottom() - ui.cursor().top() - 12.0).clamp(126.0, 780.0);
                    ScrollArea::vertical()
                        .id_salt("bin_files")
                        .max_height(height)
                        .auto_shrink([false, true])
                        .show_rows(ui, 48.0, self.bin.visible.len(), |ui, range| {
                            for i in range {
                                let index = self.bin.visible[i];
                                let entry = &inventory.items[index];
                                ui.push_id(index, |ui| {
                                    widgets::row(ui, 48.0, |ui| {
                                        let mut checked = self.bin.checked.contains(&index);
                                        let checkbox = ui
                                            .add_enabled(
                                                !busy,
                                                egui::Checkbox::without_text(&mut checked),
                                            )
                                            .on_hover_text("Select this deleted item");
                                        if checkbox.changed() {
                                            if checked {
                                                self.bin.checked.insert(index);
                                            } else {
                                                self.bin.checked.remove(&index);
                                            }
                                        }
                                        ui.label(widgets::icon(ic::FILE, 17.0, color_secondary()));
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if dates {
                                                    widgets::cell(
                                                        ui,
                                                        146.0,
                                                        true,
                                                        widgets::faint(
                                                            entry.deleted_text().unwrap_or_else(
                                                                || "Date unavailable".into(),
                                                            ),
                                                        ),
                                                    );
                                                }
                                                widgets::cell(
                                                    ui,
                                                    94.0,
                                                    true,
                                                    widgets::strong(
                                                        entry
                                                            .size
                                                            .map(util::fmt_bytes)
                                                            .unwrap_or_else(|| "Unknown".into()),
                                                        13.0,
                                                    ),
                                                );
                                                ui.allocate_ui_with_layout(
                                                    ui.available_size_before_wrap(),
                                                    Layout::top_down(Align::Min),
                                                    |ui| {
                                                        ui.add(
                                                            egui::Label::new(widgets::strong(
                                                                &entry.name,
                                                                14.0,
                                                            ))
                                                            .truncate(),
                                                        )
                                                        .on_hover_text({
                                                            let timestamp = entry
                                                                .deleted_text()
                                                                .unwrap_or_else(|| {
                                                                    "Deletion date unavailable"
                                                                        .into()
                                                                });
                                                            format!("{}\n{}", entry.name, timestamp)
                                                        });
                                                        ui.add(
                                                            egui::Label::new(widgets::faint(
                                                                entry.folder.as_deref().unwrap_or(
                                                                    "Original location unavailable",
                                                                ),
                                                            ))
                                                            .truncate(),
                                                        )
                                                        .on_hover_text(
                                                            entry.original().display().to_string(),
                                                        );
                                                    },
                                                );
                                            },
                                        );
                                    });
                                });
                            }
                        });
                });
            }
        }
        if refresh {
            self.bin.refresh(&ctx);
        }
        if open_shell {
            util::open_uri(RECYCLE_FOLDER);
        }
        if ask {
            let entries: Vec<_> = self
                .bin
                .inventory
                .as_ref()
                .into_iter()
                .flat_map(|inventory| inventory.items.iter().enumerate())
                .filter(|(index, _)| self.bin.checked.contains(index))
                .map(|(_, entry)| entry.clone())
                .collect();
            let mut details: Vec<_> = entries
                .iter()
                .take(50)
                .map(|entry| {
                    format!(
                        "{} · {}",
                        entry.original().display(),
                        entry
                            .size
                            .map(util::fmt_bytes)
                            .unwrap_or_else(|| "Size unknown".into())
                    )
                })
                .collect();
            if entries.len() > 50 {
                details.push(format!(
                    "{} more selected items from the review",
                    entries.len() - 50
                ));
            }
            self.modal = Some(Modal {
                title: format!("Permanently delete {} items?", entries.len()),
                body: "Remove these selected items from the Recycle Bin. This cannot be undone.".into(),
                details,
                note: Some("PCSweep records the original location and checks each selected item again before removal.".into()),
                confirm: "Delete permanently".into(), danger: true, action: Action::DeleteBin(entries),
            });
        }
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> BinState {
    let files = [
        ("Recording.webm", r"C:\Users\Sample\Videos", 309802312),
        ("Meeting notes.txt", r"C:\Users\Sample\Documents", 4291),
        ("Sketches.zip", r"D:\Projects\Artwork", 48471831),
    ];
    let mut bin = BinState {
        open: true,
        inventory: Some(Inventory {
            items: files
                .into_iter()
                .enumerate()
                .map(|(i, (name, folder, size))| Entry {
                    id: vec![i as u8],
                    name: name.into(),
                    folder: Some(folder.into()),
                    size: Some(size),
                    deleted: Some(134_395_353_958_215_281 + i as u64 * 100_000_000_000),
                    search: format!("{name} {folder}").to_lowercase(),
                })
                .collect(),
            ..Default::default()
        }),
        ..Default::default()
    };
    bin.reindex();
    bin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bin_can_review_and_remove_only_a_disposable_recycled_file() {
        let _apartment = desktop::Apartment::enter().unwrap();
        let name = format!(
            "PCSweep fixture {}.txt",
            crate::update_install::nonce().unwrap()
        );
        let path = std::env::temp_dir().join(&name);
        std::fs::write(&path, b"expected content").unwrap();
        trash::delete(&path).unwrap();
        let (entries, _, _) = enumerate().unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.row.name == name || entry.row.name == name.trim_end_matches(".txt"))
            .expect("the reviewed bin should include the fixture");
        assert_eq!(entry.row.size, Some(16));
        assert!(entry
            .row
            .folder
            .as_ref()
            .is_some_and(|folder| PathBuf::from(folder).is_absolute()));
        assert!(entry.row.deleted.is_some());
        let id = entry.row.id.clone();
        let op = operation(&[entry]).unwrap();
        unsafe {
            op.PerformOperations().unwrap();
            assert!(!op.GetAnyOperationsAborted().unwrap().as_bool());
        }
        assert!(!enumerate()
            .unwrap()
            .0
            .iter()
            .any(|entry| entry.row.id == id));
        assert!(!path.exists());
    }
}
