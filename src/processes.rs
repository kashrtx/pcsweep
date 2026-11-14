use crate::app::{Action, App, Modal, Page};
use crate::system;
use crate::theme::*;
use crate::util::{fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};

#[derive(Default)]
pub struct MemState {
    pub filter: String,
    pub by_cpu: bool,
}

enum MAct {
    Filter(String),
    SortCpu(bool),
    AskKill(Vec<u32>, String, u64),
    Go(Page),
}

impl App {
    pub fn page_memory(&mut self, ui: &mut Ui) {
        let mut acts = Vec::new();
        self.render_memory(ui, &mut acts);
        for a in acts {
            match a {
                MAct::Filter(f) => self.mem.filter = f,
                MAct::SortCpu(v) => self.mem.by_cpu = v,
                MAct::Go(p) => self.page = p,
                MAct::AskKill(pids, name, mem) => {
                    self.modal = Some(Modal {
                        title: format!("End {name}?"),
                        body: format!(
                            "{} process(es) using {} will be closed immediately. Unsaved work in this app will be lost.",
                            pids.len(),
                            fmt_bytes(mem)
                        ),
                        details: Vec::new(),
                        note: Some("If it keeps coming back, it's probably a startup app or a background service.".into()),
                        confirm: "End task".into(),
                        danger: true,
                        action: Action::Kill { pids, name },
                    });
                }
            }
        }
    }

    fn render_memory(&self, ui: &mut Ui, acts: &mut Vec<MAct>) {
        let s = &self.sys;
        widgets::page_header(
            ui,
            "Memory and running apps",
            "What is using your memory and processor right now.",
        );

        ui.columns(2, |cols| {
            widgets::card(&mut cols[0], |ui| {
                ui.horizontal(|ui| {
                    let f = s.mem_frac();
                    let col = if f > 0.9 { BAD } else if f > 0.8 { WARN } else { ACCENT };
                    widgets::ring(ui, f, 120.0, 11.0, col, &format!("{:.0}%", f * 100.0), "memory");
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.label(widgets::strong("Memory", 16.0));
                        ui.label(widgets::dim(format!("{} in use of {}", fmt_bytes(s.mem_used), fmt_bytes(s.mem_total))));
                        ui.label(widgets::dim(format!("{} available", fmt_bytes(s.mem_avail))));
                        if s.swap_total > 0 {
                            ui.label(widgets::faint(format!(
                                "Page file: {} of {} used",
                                fmt_bytes(s.swap_used),
                                fmt_bytes(s.swap_total)
                            )));
                        }
                    });
                });
            });
            widgets::card(&mut cols[1], |ui| {
                ui.horizontal(|ui| {
                    let f = s.cpu / 100.0;
                    let col = if f > 0.9 { BAD } else if f > 0.7 { WARN } else { GOOD };
                    widgets::ring(ui, f, 120.0, 11.0, col, &format!("{:.0}%", s.cpu), "processor");
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.label(widgets::strong("Processor", 16.0));
                        ui.label(widgets::dim(s.cpu_name.clone()));
                        ui.label(widgets::dim(format!("{} logical cores", s.cores)));
                    });
                });
            });
        });

        ui.add_space(4.0);
        widgets::banner(ui, ACCENT, ic::INFO, |ui| {
            ui.label("Windows already manages memory well, and unused RAM is wasted RAM. \"Memory cleaners\" only make apps reload. The real fix for a slow PC is closing heavy apps and keeping them from starting with Windows.");
            if ui.add(widgets::small("Startup apps")).clicked() {
                acts.push(MAct::Go(Page::Startup));
            }
        });
        ui.add_space(6.0);

        let mut groups = s.groups();
        if self.mem.by_cpu {
            groups.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
        }
        let filter = self.mem.filter.to_lowercase();
        let groups: Vec<_> = groups
            .into_iter()
            .filter(|g| filter.is_empty() || g.name.to_lowercase().contains(&filter))
            .take(150)
            .collect();
        let top = groups.iter().map(|g| g.mem).max().unwrap_or(1).max(1) as f32;

        ui.horizontal(|ui| {
            ui.label(widgets::dim("Sort by"));
            if ui.add(widgets::chip("Memory", !self.mem.by_cpu)).clicked() {
                acts.push(MAct::SortCpu(false));
            }
            if ui.add(widgets::chip("Processor", self.mem.by_cpu)).clicked() {
                acts.push(MAct::SortCpu(true));
            }
            ui.add_space(12.0);
            let mut f = self.mem.filter.clone();
            if widgets::search_box(ui, &mut f, "Search apps", 200.0).changed() {
                acts.push(MAct::Filter(f));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(widgets::faint(format!("{} processes, refreshed every 3 seconds", fmt_num(s.procs.len() as u64))));
            });
        });

        ScrollArea::vertical()
            .id_source("procs_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, 36.0, groups.len(), |ui, range| {
                for g in &groups[range] {
                    widgets::row(ui, 36.0, |ui| {
                        ui.label(widgets::icon(ic::CHIP, 16.0, ACCENT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            if system::is_critical(&g.name) {
                                ui.add_enabled(false, widgets::small("End task"))
                                    .on_disabled_hover_text("Part of Windows. Ending it could crash your PC.");
                            } else if ui.add(widgets::small("End task")).clicked() {
                                acts.push(MAct::AskKill(g.pids.clone(), g.name.clone(), g.mem));
                            }
                            ui.add_sized([70.0, 20.0], Label::new(widgets::faint(format!("{:.1}% CPU", g.cpu))));
                            ui.add_sized([84.0, 20.0], Label::new(widgets::strong(fmt_bytes(g.mem), 14.0)));
                            widgets::bar(ui, g.mem as f32 / top, ACCENT_STRONG, egui::vec2(140.0, 6.0));
                            ui.add_space(8.0);
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(Label::new(RichText::new(&g.name).color(TEXT)).truncate());
                                if g.count > 1 {
                                    ui.label(widgets::faint(format!("{} processes", g.count)));
                                }
                            });
                        });
                    });
                }
            });
    }
}
