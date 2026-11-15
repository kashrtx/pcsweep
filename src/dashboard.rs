use crate::app::{App, Page};
use crate::system;
use crate::theme::*;
use crate::util::{self, fmt_bytes};
use crate::widgets::{self, ic};
use chrono::Timelike;
use eframe::egui::{self, Align, Color32, Label, Layout, RichText, ScrollArea, Ui};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sev {
    High,
    Medium,
    Low,
}

pub struct Rec {
    pub sev: Sev,
    pub text: String,
    pub go: Option<Page>,
    pub action: &'static str,
}

enum DAct {
    Go(Page),
    Analyze(PathBuf),
    Rescan,
}

impl App {
    /// A 0–100 score from things that actually affect speed and reliability.
    pub fn health(&self) -> (u32, Vec<Rec>) {
        let mut score: i32 = 100;
        let mut recs = Vec::new();

        if let Some(d) = self.sys.system_drive() {
            let free = 1.0 - d.used_frac();
            if free < 0.10 {
                score -= 25;
                recs.push(Rec {
                    sev: Sev::High,
                    text: format!("{} is almost full ({} free). Windows slows down and updates fail when space runs out.", d.letter(), fmt_bytes(d.free)),
                    go: Some(Page::Analyzer),
                    action: "Find space",
                });
            } else if free < 0.20 {
                score -= 10;
                recs.push(Rec {
                    sev: Sev::Medium,
                    text: format!(
                        "{} has {} free. Keeping at least 20% free helps performance.",
                        d.letter(),
                        fmt_bytes(d.free)
                    ),
                    go: Some(Page::Analyzer),
                    action: "Find space",
                });
            }
        }

        let junk = self.cleaner.recommended_total();
        const GB: u64 = 1024 * 1024 * 1024;
        if junk > 5 * GB {
            score -= 15;
            recs.push(Rec {
                sev: Sev::High,
                text: format!("{} of junk files can be removed safely.", fmt_bytes(junk)),
                go: Some(Page::Cleaner),
                action: "Review",
            });
        } else if junk > GB {
            score -= 8;
            recs.push(Rec {
                sev: Sev::Medium,
                text: format!("{} of junk files can be removed safely.", fmt_bytes(junk)),
                go: Some(Page::Cleaner),
                action: "Review",
            });
        } else if junk > 200 * 1024 * 1024 {
            score -= 3;
            recs.push(Rec {
                sev: Sev::Low,
                text: format!("{} of junk files found.", fmt_bytes(junk)),
                go: Some(Page::Cleaner),
                action: "Review",
            });
        }

        let bin = self.cleaner.recycle_bin_bytes();
        if bin > 2 * GB {
            score -= 5;
            recs.push(Rec {
                sev: Sev::Low,
                text: format!("The Recycle Bin is holding {}.", fmt_bytes(bin)),
                go: Some(Page::Cleaner),
                action: "Review",
            });
        }

        let mf = self.sys.mem_frac();
        if mf > 0.90 {
            score -= 15;
            recs.push(Rec {
                sev: Sev::High,
                text: format!(
                    "Memory is {:.0}% full. Close heavy apps to stop slowdowns.",
                    mf * 100.0
                ),
                go: Some(Page::Memory),
                action: "See apps",
            });
        } else if mf > 0.80 {
            score -= 7;
            recs.push(Rec {
                sev: Sev::Medium,
                text: format!("Memory is {:.0}% full.", mf * 100.0),
                go: Some(Page::Memory),
                action: "See apps",
            });
        }

        let st = self.startup.enabled_count();
        if st > 15 {
            score -= 10;
            recs.push(Rec {
                sev: Sev::Medium,
                text: format!("{st} apps start with Windows. Turning some off speeds up sign-in."),
                go: Some(Page::Startup),
                action: "Manage",
            });
        } else if st > 8 {
            score -= 5;
            recs.push(Rec {
                sev: Sev::Low,
                text: format!("{st} apps start with Windows."),
                go: Some(Page::Startup),
                action: "Manage",
            });
        }

        let up = system::uptime_secs();
        if up > 7 * 86_400 {
            score -= 5;
            recs.push(Rec { sev: Sev::Low, text: format!("Your PC has been running for {}. A restart clears out memory and finishes updates.", util::fmt_duration(up)), go: None, action: "" });
        }

        recs.sort_by_key(|r| match r.sev {
            Sev::High => 0,
            Sev::Medium => 1,
            Sev::Low => 2,
        });
        (score.clamp(0, 100) as u32, recs)
    }

    pub fn page_dashboard(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        self.render_dashboard(ui, &mut acts);
        for a in acts {
            match a {
                DAct::Go(p) => self.page = p,
                DAct::Analyze(p) => self.start_analyze(p, &ctx),
                DAct::Rescan => self.start_junk_scan(&ctx),
            }
        }
    }

    fn render_dashboard(&self, ui: &mut Ui, acts: &mut Vec<DAct>) {
        let hour = chrono::Local::now().hour();
        let greet = match hour {
            5..=11 => "Good morning",
            12..=17 => "Good afternoon",
            _ => "Good evening",
        };
        widgets::page_header(
            ui,
            greet,
            &format!(
                "{}, {}, running for {}",
                self.sys.host,
                self.sys.os,
                util::fmt_duration(system::uptime_secs())
            ),
        );

        ScrollArea::vertical()
            .id_salt("dash_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (score, recs) = self.health();
                let scanning = !self.cleaner.scanned_once;
                ui.columns(2, |cols| {
                    // Health
                    widgets::card(&mut cols[0], |ui| {
                        ui.set_min_height(250.0);
                        ui.horizontal(|ui| {
                            let col = if score >= 85 { GOOD } else if score >= 65 { WARN } else { BAD };
                            let word = if scanning {
                                "checking"
                            } else if score >= 85 {
                                "healthy"
                            } else if score >= 65 {
                                "could be better"
                            } else {
                                "needs attention"
                            };
                            widgets::ring(ui, score as f32 / 100.0, 150.0, 12.0, col, &score.to_string(), word);
                            ui.add_space(14.0);
                            ui.vertical(|ui| {
                                ui.label(widgets::strong("PC health", 18.0));
                                if recs.is_empty() && !scanning {
                                    ui.label(RichText::new("Everything looks good. Nothing needs your attention.").color(GOOD));
                                }
                                for r in recs.iter().take(4) {
                                    ui.add_space(2.0);
                                    ui.horizontal_wrapped(|ui| {
                                        let c = match r.sev {
                                            Sev::High => BAD,
                                            Sev::Medium => WARN,
                                            Sev::Low => TEXT_FAINT,
                                        };
                                        widgets::dot(ui, c);
                                        ui.label(RichText::new(&r.text).color(TEXT_DIM));
                                        if let Some(p) = r.go {
                                            if ui.add(widgets::small(r.action)).clicked() {
                                                acts.push(DAct::Go(p));
                                            }
                                        }
                                    });
                                }
                            });
                        });
                    });

                    // Junk
                    widgets::card(&mut cols[1], |ui| {
                        ui.set_min_height(250.0);
                        ui.horizontal(|ui| {
                            ui.label(widgets::strong("Junk found", 18.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if self.cleaner.scan.running() {
                                    ui.spinner();
                                } else if ui.add(widgets::small(widgets::icon_text(ic::REFRESH, "Scan again", 13.0, TEXT))).clicked() {
                                    acts.push(DAct::Rescan);
                                }
                            });
                        });
                        if scanning {
                            ui.add_space(30.0);
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(widgets::dim("Looking for caches and leftovers…"));
                            });
                            return;
                        }
                        let junk = self.cleaner.recommended_total();
                        ui.label(RichText::new(fmt_bytes(junk)).font(widgets::light(46.0)).color(TEXT));
                        ui.label(widgets::dim("safe to remove in the recommended categories"));
                        ui.add_space(8.0);
                        let top = self.cleaner.top(4);
                        let max = top.first().map(|t| t.1).unwrap_or(1).max(1) as f32;
                        for (i, b) in top {
                            ui.horizontal(|ui| {
                                widgets::cell(ui, 190.0, false, RichText::new(self.cleaner.cats[i].name).color(TEXT_DIM));
                                let bw = (ui.available_width() - 90.0).max(20.0);
                                widgets::bar(ui, b as f32 / max, ACCENT, egui::vec2(bw, 6.0));
                                widgets::cell(ui, 80.0, true, widgets::faint(fmt_bytes(b)));
                            });
                        }
                        let optional = self.cleaner.optional_total();
                        if optional > 0 {
                            ui.label(widgets::faint(format!(
                                "Plus {} in optional categories like package and shader caches. You can choose those in the Junk cleaner.",
                                fmt_bytes(optional)
                            )));
                        }
                        ui.add_space(8.0);
                        if ui.add(widgets::primary(widgets::icon_text(ic::CLEAN, "Review and clean", 14.0, Color32::WHITE))).clicked() {
                            acts.push(DAct::Go(Page::Cleaner));
                        }
                    });
                });

                ui.add_space(8.0);
                widgets::card(ui, |ui| {
                    ui.label(widgets::strong("Drives", 18.0));
                    ui.add_space(4.0);
                    for d in &self.sys.drives {
                        ui.horizontal(|ui| {
                            ui.label(widgets::icon(ic::DRIVE, 20.0, ACCENT));
                            ui.vertical(|ui| {
                                ui.set_width(170.0);
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(widgets::strong(d.letter(), 15.0));
                                let label = if d.label.is_empty() { d.kind.to_string() } else { format!("{}, {}", d.label, d.kind) };
                                ui.add(Label::new(widgets::faint(if d.removable { format!("{label}, removable") } else { label })).truncate());
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(widgets::small("Analyze")).clicked() {
                                    acts.push(DAct::Analyze(PathBuf::from(&d.mount)));
                                }
                                ui.add_sized([170.0, 20.0], Label::new(widgets::dim(format!("{} free of {}", fmt_bytes(d.free), fmt_bytes(d.total)))));
                                let f = d.used_frac();
                                let col = if f > 0.9 { BAD } else if f > 0.8 { WARN } else { ACCENT };
                                let bw = (ui.available_width() - 10.0).max(40.0);
                                widgets::bar(ui, f, col, egui::vec2(bw, 10.0));
                            });
                        });
                        ui.add_space(4.0);
                    }
                });

                ui.add_space(8.0);
                ui.columns(2, |cols| {
                    widgets::card(&mut cols[0], |ui| {
                        ui.horizontal(|ui| {
                            ui.label(widgets::strong("Memory", 18.0));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(widgets::small("See all")).clicked() {
                                    acts.push(DAct::Go(Page::Memory));
                                }
                            });
                        });
                        let f = self.sys.mem_frac();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{:.0}%", f * 100.0)).font(widgets::light(36.0)).color(TEXT));
                            ui.label(widgets::dim(format!(
                                "of {} in use, processor at {:.0}%",
                                fmt_bytes(self.sys.mem_total),
                                self.sys.cpu
                            )));
                        });
                        let groups = self.sys.groups();
                        let max = groups.first().map(|g| g.mem).unwrap_or(1).max(1) as f32;
                        for g in groups.iter().take(4) {
                            ui.horizontal(|ui| {
                                widgets::cell(ui, 190.0, false, RichText::new(&g.name).color(TEXT_DIM));
                                let bw = (ui.available_width() - 90.0).max(20.0);
                                widgets::bar(ui, g.mem as f32 / max, ACCENT_STRONG, egui::vec2(bw, 6.0));
                                widgets::cell(ui, 80.0, true, widgets::faint(fmt_bytes(g.mem)));
                            });
                        }
                    });
                    widgets::card(&mut cols[1], |ui| {
                        ui.label(widgets::strong("Quick actions", 18.0));
                        ui.add_space(4.0);
                        let sys_drive = self.sys.system_drive().map(|d| d.mount.clone()).unwrap_or_else(|| "C:\\".into());
                        let items: [(char, &str, &str, DActKind); 5] = [
                            (ic::PIE, "Map my system drive", "See which folders take the most space", DActKind::Analyze),
                            (ic::COPY, "Find duplicate files", "Keep one copy, free the rest", DActKind::Go(Page::Duplicates)),
                            (ic::APPS, "Review installed apps", "Uninstall big programs you no longer use", DActKind::Go(Page::Apps)),
                            (ic::POWER, "Choose startup apps", "Faster sign-in, more free memory", DActKind::Go(Page::Startup)),
                            (ic::SHIELD, "Create a restore point", "A safety net before system changes", DActKind::Go(Page::Tools)),
                        ];
                        for (icon, title, sub, kind) in items {
                            let resp = widgets::row(ui, 40.0, |ui| {
                                ui.label(widgets::icon(icon, 18.0, ACCENT));
                                ui.add_space(4.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(RichText::new(title).color(TEXT));
                                    ui.label(widgets::faint(sub));
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.add_space(6.0);
                                    ui.add(widgets::small(widgets::icon(ic::CHEVRON, 11.0, TEXT_DIM))).clicked()
                                })
                                .inner
                            });
                            if resp {
                                match kind {
                                    DActKind::Analyze => acts.push(DAct::Analyze(PathBuf::from(&sys_drive))),
                                    DActKind::Go(p) => acts.push(DAct::Go(p)),
                                }
                            }
                        }
                    });
                });
                ui.add_space(8.0);
                if !self.is_admin {
                    widgets::banner(ui, WARN, ic::SHIELD, |ui| {
                        ui.label(
                            "PCSweep is running without administrator rights, so some Windows folders can't be cleaned. Use Restart as admin in the sidebar to unlock everything.",
                        );
                    });
                }
                ui.add_space(20.0);
            });
    }
}

#[derive(Clone, Copy)]
enum DActKind {
    Analyze,
    Go(Page),
}
