use crate::app::{Action, App, Modal, ToastKind};
use crate::theme::*;
use crate::util::{self, fmt_bytes};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Color32, Layout, RichText, ScrollArea, Ui};
use std::fs;
use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Cmd {
        program: &'static str,
        args: &'static [&'static str],
    },
    Open(&'static str),
    Launch(&'static str),
}

#[derive(Clone, Copy)]
pub struct ToolDef {
    pub section: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub icon: char,
    pub admin: bool,
    pub duration: &'static str,
    pub confirm: Option<&'static str>,
    pub kind: ToolKind,
}

pub fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            section: "Safety net",
            name: "Create a restore point",
            desc: "Saves a snapshot of system settings you can roll back to. A good idea before big changes. Windows allows one every 24 hours.",
            icon: ic::SHIELD,
            admin: true,
            duration: "Under a minute",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "powershell.exe",
                args: &[
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    "Checkpoint-Computer -Description 'PCSweep' -RestorePointType 'MODIFY_SETTINGS' -ErrorAction Stop; 'Restore point created.'",
                ],
            },
        },
        ToolDef {
            section: "Free up more space",
            name: "Clean up the Windows component store",
            desc: "Removes old versions of system files left behind by updates (WinSxS). Often frees several GB. Safe, built into Windows.",
            icon: ic::CLEAN,
            admin: true,
            duration: "5 to 20 minutes",
            confirm: Some("After this, already-installed updates can no longer be uninstalled. Windows keeps working normally."),
            kind: ToolKind::Cmd {
                program: "Dism.exe",
                args: &["/Online", "/Cleanup-Image", "/StartComponentCleanup"],
            },
        },
        ToolDef {
            section: "Free up more space",
            name: "Measure the component store",
            desc: "Reports how much space WinSxS really uses and whether cleanup is recommended. Read-only.",
            icon: ic::PIE,
            admin: true,
            duration: "1 to 3 minutes",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "Dism.exe",
                args: &["/Online", "/Cleanup-Image", "/AnalyzeComponentStore"],
            },
        },
        ToolDef {
            section: "Free up more space",
            name: "Turn off hibernation",
            desc: "Deletes hiberfil.sys, which is often as big as your RAM. This also turns off Fast Startup and the Hibernate option.",
            icon: ic::POWER,
            admin: true,
            duration: "Instant",
            confirm: Some("Hibernate and Fast Startup will be turned off. You can turn them back on here at any time."),
            kind: ToolKind::Cmd {
                program: "powercfg.exe",
                args: &["/hibernate", "off"],
            },
        },
        ToolDef {
            section: "Free up more space",
            name: "Turn hibernation back on",
            desc: "Restores Hibernate and Fast Startup.",
            icon: ic::POWER,
            admin: true,
            duration: "Instant",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "powercfg.exe",
                args: &["/hibernate", "on"],
            },
        },
        ToolDef {
            section: "Free up more space",
            name: "Windows Disk Cleanup",
            desc: "Windows' own tool. Use \"Clean up system files\" to remove a previous Windows installation (Windows.old) after an upgrade.",
            icon: ic::CLEAN,
            admin: false,
            duration: "Opens a window",
            confirm: None,
            kind: ToolKind::Launch("cleanmgr.exe"),
        },
        ToolDef {
            section: "Free up more space",
            name: "Storage Sense settings",
            desc: "Let Windows empty temp files and the Recycle Bin automatically on a schedule.",
            icon: ic::TOOLS,
            admin: false,
            duration: "Opens Settings",
            confirm: None,
            kind: ToolKind::Open("ms-settings:storagesense"),
        },
        ToolDef {
            section: "Repair and health",
            name: "Repair system files",
            desc: "System File Checker scans protected Windows files and replaces any that are damaged.",
            icon: ic::TOOLS,
            admin: true,
            duration: "10 to 30 minutes",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "sfc.exe",
                args: &["/scannow"],
            },
        },
        ToolDef {
            section: "Repair and health",
            name: "Repair the Windows image",
            desc: "DISM checks the files that System File Checker repairs from, and downloads fixes from Windows Update if needed. Run this first if SFC can't fix something.",
            icon: ic::TOOLS,
            admin: true,
            duration: "10 to 30 minutes",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "Dism.exe",
                args: &["/Online", "/Cleanup-Image", "/RestoreHealth"],
            },
        },
        ToolDef {
            section: "Repair and health",
            name: "Check the system drive for errors",
            desc: "An online, read-only scan of the file system. Nothing is changed and you can keep working.",
            icon: ic::DRIVE,
            admin: true,
            duration: "1 to 10 minutes",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "chkdsk.exe",
                args: &["%SystemDrive%", "/scan"],
            },
        },
        ToolDef {
            section: "Performance",
            name: "Optimize the system drive",
            desc: "Sends TRIM to SSDs or defragments hard drives. Windows does this weekly; run it now if a drive feels slow.",
            icon: ic::SPEED,
            admin: true,
            duration: "1 to 15 minutes",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "defrag.exe",
                args: &["%SystemDrive%", "/O", "/U", "/V"],
            },
        },
        ToolDef {
            section: "Performance",
            name: "Use the High performance power plan",
            desc: "Keeps the CPU at higher clock speeds. Snappier, but uses more power. Some laptops don't offer this plan.",
            icon: ic::BOLT,
            admin: false,
            duration: "Instant",
            confirm: Some("Your PC will use more power and may run warmer. Switch back to Balanced at any time."),
            kind: ToolKind::Cmd {
                program: "powercfg.exe",
                args: &["/setactive", "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c"],
            },
        },
        ToolDef {
            section: "Performance",
            name: "Use the Balanced power plan",
            desc: "Windows' default. Speeds up when needed and saves power otherwise.",
            icon: ic::BOLT,
            admin: false,
            duration: "Instant",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "powercfg.exe",
                args: &["/setactive", "381b4222-f694-41f0-9685-ff5bb260df2e"],
            },
        },
        ToolDef {
            section: "Performance",
            name: "Flush the DNS cache",
            desc: "Fixes websites that won't load after a network or DNS change.",
            icon: ic::GLOBE,
            admin: false,
            duration: "Instant",
            confirm: None,
            kind: ToolKind::Cmd {
                program: "ipconfig.exe",
                args: &["/flushdns"],
            },
        },
    ]
}

// ---------------------------------------------------------------- running

#[derive(Default)]
struct Decoder {
    utf16: Option<bool>,
    pending: Vec<u8>,
}

impl Decoder {
    /// Windows tools print either the OEM code page or UTF-16 (sfc). Detect which.
    fn feed(&mut self, data: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(data);
        let utf16 = match self.utf16 {
            Some(v) => v,
            None => {
                let odd_zero = bytes.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
                let v = bytes.len() >= 4 && odd_zero * 4 >= bytes.len();
                self.utf16 = Some(v);
                v
            }
        };
        if utf16 {
            let even = bytes.len() & !1;
            if even < bytes.len() {
                self.pending.push(bytes[even]);
            }
            let units: Vec<u16> = bytes[..even]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units).replace('\u{feff}', "")
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }
}

/// Appends text, honoring carriage returns so progress lines overwrite in place.
fn append(out: &Mutex<String>, text: &str) {
    let Ok(mut s) = out.lock() else { return };
    for ch in text.chars() {
        match ch {
            '\0' => {}
            '\r' => s.push('\r'),
            '\n' => {
                if s.ends_with('\r') {
                    s.pop();
                }
                s.push('\n');
            }
            c => {
                if s.ends_with('\r') {
                    s.pop();
                    let cut = s.rfind('\n').map_or(0, |i| i + 1);
                    s.truncate(cut);
                }
                s.push(c);
            }
        }
    }
    if s.len() > 200_000 {
        let mut start = s.len() - 150_000;
        while !s.is_char_boundary(start) {
            start += 1;
        }
        s.drain(..start);
    }
}

pub struct ToolRun {
    pub name: &'static str,
    pub output: Arc<Mutex<String>>,
    pub done: Arc<AtomicBool>,
    pub exit: Arc<Mutex<Option<i32>>>,
    pub started: Instant,
    pub logged: bool,
}

impl ToolRun {
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

fn reader<R: Read + Send + 'static>(mut r: R, out: Arc<Mutex<String>>, ctx: egui::Context) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut dec = Decoder::default();
        let mut buf = [0u8; 4096];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = dec.feed(&buf[..n]);
                    append(&out, &text);
                    ctx.request_repaint();
                }
            }
        }
    })
}

pub fn spawn(name: &'static str, program: &'static str, args: Vec<String>, ctx: egui::Context) -> ToolRun {
    let output = Arc::new(Mutex::new(String::new()));
    let done = Arc::new(AtomicBool::new(false));
    let exit = Arc::new(Mutex::new(None));
    let (o, d, x) = (output.clone(), done.clone(), exit.clone());
    std::thread::spawn(move || {
        append(&o, &format!("> {} {}\n\n", program, args.join(" ")));
        let child = util::hidden_command(program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match child {
            Err(e) => append(&o, &format!("Couldn't start {program}: {e}\n")),
            Ok(mut ch) => {
                let h1 = ch.stdout.take().map(|s| reader(s, o.clone(), ctx.clone()));
                let h2 = ch.stderr.take().map(|s| reader(s, o.clone(), ctx.clone()));
                let status = ch.wait();
                if let Some(h) = h1 {
                    let _ = h.join();
                }
                if let Some(h) = h2 {
                    let _ = h.join();
                }
                let code = status.ok().and_then(|s| s.code());
                if let Ok(mut e) = x.lock() {
                    *e = code;
                }
                append(&o, &format!("\n\nFinished with exit code {}.\n", code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())));
            }
        }
        d.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    });
    ToolRun {
        name,
        output,
        done,
        exit,
        started: Instant::now(),
        logged: false,
    }
}

/// Sizes of Windows-managed files in the system drive root (pagefile, hibernation file).
pub fn system_files() -> Vec<(String, u64)> {
    let root = format!("{}\\", std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()));
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if matches!(name.as_str(), "pagefile.sys" | "hiberfil.sys" | "swapfile.sys") {
                if let Ok(md) = e.metadata() {
                    out.push((name, md.len()));
                }
            }
        }
    }
    let old = format!("{root}Windows.old");
    if fs::symlink_metadata(&old).is_ok() {
        out.push(("windows.old".into(), 0));
    }
    out
}

pub struct ToolsState {
    pub defs: Vec<ToolDef>,
    pub run: Option<ToolRun>,
    pub sysfiles: Vec<(String, u64)>,
}

impl Default for ToolsState {
    fn default() -> Self {
        Self {
            defs: tools(),
            run: None,
            sysfiles: system_files(),
        }
    }
}

enum TAct {
    Run(usize),
    Copy(String),
    CloseOutput,
    Relaunch,
}

impl App {
    pub fn start_tool(&mut self, i: usize, ctx: &egui::Context) {
        let Some(def) = self.tools.defs.get(i).copied() else { return };
        match def.kind {
            ToolKind::Open(uri) => util::open_uri(uri),
            ToolKind::Launch(p) => util::launch(p),
            ToolKind::Cmd { program, args } => {
                if self.tools.run.as_ref().map_or(false, |r| !r.is_done()) {
                    self.toast(ToastKind::Info, "Another tool is still running.");
                    return;
                }
                let args: Vec<String> = args.iter().map(|a| util::expand_env(a)).collect();
                util::log(format!("Started tool: {}", def.name));
                self.tools.run = Some(spawn(def.name, program, args, ctx.clone()));
            }
        }
    }

    pub fn poll_tools(&mut self) {
        let mut finished: Option<(&'static str, Option<i32>)> = None;
        if let Some(run) = &mut self.tools.run {
            if run.is_done() && !run.logged {
                run.logged = true;
                let code = run.exit.lock().ok().and_then(|e| *e);
                finished = Some((run.name, code));
            }
        }
        if let Some((name, code)) = finished {
            util::log(format!(
                "Tool finished: {} (exit code {})",
                name,
                code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())
            ));
            if code == Some(0) {
                self.toast(ToastKind::Success, format!("{name} finished"));
            } else {
                self.toast(ToastKind::Error, format!("{name} reported a problem. See the output."));
            }
            self.tools.sysfiles = system_files();
            self.sys.refresh_drives();
        }
    }

    pub fn page_tools(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        self.render_tools(ui, &mut acts);
        for a in acts {
            match a {
                TAct::Run(i) => {
                    let def = self.tools.defs[i];
                    match (def.kind, def.confirm) {
                        (ToolKind::Cmd { .. }, Some(warning)) => {
                            self.modal = Some(Modal {
                                title: format!("{}?", def.name),
                                body: def.desc.to_string(),
                                details: Vec::new(),
                                note: Some(warning.to_string()),
                                confirm: "Run it".into(),
                                danger: false,
                                action: Action::RunTool(i),
                            });
                        }
                        _ => self.start_tool(i, &ctx),
                    }
                }
                TAct::Copy(s) => ctx.output_mut(|o| o.copied_text = s),
                TAct::CloseOutput => self.tools.run = None,
                TAct::Relaunch => self.relaunch_admin(&ctx),
            }
        }
    }

    fn render_tools(&self, ui: &mut Ui, acts: &mut Vec<TAct>) {
        let t = &self.tools;
        widgets::page_header(
            ui,
            "Tools",
            "Windows' own repair and maintenance tools, one click away, with their output shown live.",
        );
        let busy = t.run.as_ref().map_or(false, |r| !r.is_done());

        if let Some(run) = &t.run {
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    if run.is_done() {
                        let code = run.exit.lock().ok().and_then(|e| *e);
                        if code == Some(0) {
                            ui.label(widgets::icon(ic::CHECK, 16.0, GOOD));
                        } else {
                            ui.label(widgets::icon(ic::WARN, 16.0, WARN));
                        }
                    } else {
                        ui.spinner();
                    }
                    ui.label(widgets::strong(run.name, 16.0));
                    ui.label(widgets::faint(format!("{}s", run.started.elapsed().as_secs())));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if run.is_done() && ui.add(widgets::small("Close")).clicked() {
                            acts.push(TAct::CloseOutput);
                        }
                        if ui.add(widgets::small("Copy output")).clicked() {
                            let text = run.output.lock().map(|s| s.clone()).unwrap_or_default();
                            acts.push(TAct::Copy(text));
                        }
                    });
                });
                let text = run.output.lock().map(|s| s.clone()).unwrap_or_default();
                egui::Frame::none()
                    .fill(Color32::from_rgb(13, 15, 21))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ScrollArea::vertical()
                            .id_source("tool_output")
                            .max_height(220.0)
                            .stick_to_bottom(true)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.label(RichText::new(text).monospace().color(TEXT_DIM));
                            });
                    });
            });
            ui.add_space(6.0);
        }

        ScrollArea::vertical()
            .id_source("tools_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !t.sysfiles.is_empty() {
                    widgets::section(ui, "Windows-managed files on your system drive");
                    ui.horizontal_wrapped(|ui| {
                        for (name, size) in &t.sysfiles {
                            let text = match name.as_str() {
                                "hiberfil.sys" => format!("Hibernation file {}", fmt_bytes(*size)),
                                "pagefile.sys" => format!("Page file {}", fmt_bytes(*size)),
                                "swapfile.sys" => format!("Swap file {}", fmt_bytes(*size)),
                                _ => "A previous Windows installation (Windows.old) is present. Remove it with Disk Cleanup".to_string(),
                            };
                            widgets::badge(ui, &text, TEXT_DIM);
                        }
                    });
                    ui.label(widgets::faint(
                        "The page file is virtual memory and should stay. Windows sizes it automatically.",
                    ));
                }
                let mut last_section = "";
                for (i, def) in t.defs.iter().enumerate() {
                    if def.section != last_section {
                        widgets::section(ui, def.section);
                        last_section = def.section;
                    }
                    widgets::tight_frame().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(widgets::icon(def.icon, 20.0, ACCENT));
                            ui.add_space(4.0);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let needs_admin = def.admin && !self.is_admin;
                                if needs_admin {
                                    if ui.add(widgets::small(widgets::icon_text(ic::SHIELD, "Restart as admin", 13.0, WARN))).clicked() {
                                        acts.push(TAct::Relaunch);
                                    }
                                } else {
                                    let is_cmd = matches!(def.kind, ToolKind::Cmd { .. });
                                    let label = if is_cmd { "Run" } else { "Open" };
                                    if ui.add_enabled(!(is_cmd && busy), widgets::small(label)).clicked() {
                                        acts.push(TAct::Run(i));
                                    }
                                }
                                ui.label(widgets::faint(def.duration));
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(widgets::strong(def.name, 14.5));
                                            if def.admin {
                                                widgets::badge(ui, "Admin", TEXT_DIM);
                                            }
                                        });
                                        ui.label(widgets::dim(def.desc));
                                    });
                                });
                            });
                        });
                    });
                    ui.add_space(2.0);
                }
                ui.add_space(20.0);
            });
    }
}
