//! Review scheduled background work without deleting registrations or browser data.
use crate::app::{Action, App, Modal, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util;
use crate::widgets;
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use xxhash_rust::xxh3::xxh3_128;

// COM's GetTask takes a literal name. Unlike Get-ScheduledTask's name filter,
// names containing brackets, asterisks or apostrophes cannot select other tasks.
const TASK_HELPERS: &str = r#"
$ErrorActionPreference = 'Stop'
$service = New-Object -ComObject 'Schedule.Service'
$service.Connect()
$current = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$signatureCache = @{}
function Same([string]$a, [string]$b) { [string]::Equals($a, $b, [StringComparison]::OrdinalIgnoreCase) }
function Text($xml, [string]$xpath) {
    $node = $xml.SelectSingleNode($xpath)
    if ($null -eq $node) { return '' }
    return [string]$node.InnerText
}
function InScope($xml) {
    $principal = Text $xml "/*[local-name()='Task']/*[local-name()='Principals']/*[local-name()='Principal']/*[local-name()='UserId']"
    if ([string]::IsNullOrWhiteSpace($principal)) { return $false }
    if ($principal -match '^S-1-5-(18|19|20)$' -or $principal -match '^(SYSTEM|LOCAL SERVICE|NETWORK SERVICE|NT AUTHORITY\\(SYSTEM|LOCAL SERVICE|NETWORK SERVICE))$') { return $true }
    if ((Same $principal $current.Name) -or (Same $principal $current.User.Value)) { return $true }
    try {
        $account = New-Object System.Security.Principal.NTAccount($principal)
        return (Same ($account.Translate([System.Security.Principal.SecurityIdentifier]).Value) $current.User.Value)
    } catch { return $false }
}
function Fingerprint($xml) {
    $copy = New-Object System.Xml.XmlDocument
    $copy.PreserveWhitespace = $false
    $copy.LoadXml($xml.OuterXml)
    $node = $copy.SelectSingleNode("/*[local-name()='Task']/*[local-name()='Settings']/*[local-name()='Enabled']")
    if ($null -ne $node) { [void]$node.ParentNode.RemoveChild($node) }
    $hash = [System.Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($copy.OuterXml)))).Replace('-', '').ToLowerInvariant() }
    finally { $hash.Dispose() }
}
function Describe($task) {
    $xml = New-Object System.Xml.XmlDocument
    $xml.PreserveWhitespace = $false
    $xml.LoadXml([string]$task.Xml)
    if (-not (InScope $xml)) { return $null }
    $author = Text $xml "/*[local-name()='Task']/*[local-name()='RegistrationInfo']/*[local-name()='Author']"
    $principal = Text $xml "/*[local-name()='Task']/*[local-name()='Principals']/*[local-name()='Principal']/*[local-name()='UserId']"
    $microsoft = $task.Path.StartsWith('\Microsoft\', [StringComparison]::OrdinalIgnoreCase) -or $task.Path.StartsWith('\Windows\', [StringComparison]::OrdinalIgnoreCase) -or $author -match '(?i)microsoft|windows'
    $signed = $false
    $securityPattern = '(?i)defender|antivirus|antimalware|securityhealth|bitdefender|kaspersky|eset|crowdstrike|sentinelone|sophos|malwarebytes|avast|norton|symantec|mcafee|trend micro|f-secure|fortinet'
    $security = ($task.Path -match $securityPattern) -or ($author -match $securityPattern)
    $publishers = New-Object 'System.Collections.Generic.List[string]'
    $actions = New-Object 'System.Collections.Generic.List[string]'
    $unknownAction = $false
    foreach ($action in @($xml.SelectNodes("/*[local-name()='Task']/*[local-name()='Actions']/*"))) {
        if ($action.LocalName -ne 'Exec') {
            $actions.Add($action.LocalName + ': ' + $action.InnerText)
            $unknownAction = $true
            continue
        }
        $command = Text $action "*[local-name()='Command']"
        $arguments = Text $action "*[local-name()='Arguments']"
        $actions.Add(($command + ' ' + $arguments).Trim())
        $exe = [Environment]::ExpandEnvironmentVariables($command).Trim('"')
        if (-not [IO.Path]::IsPathRooted($exe)) { $unknownAction = $true; continue }
        if ($exe.StartsWith(($env:WINDIR.TrimEnd('\') + '\'), [StringComparison]::OrdinalIgnoreCase)) { $microsoft = $true }
        if (-not $microsoft -and (Test-Path -LiteralPath $exe -PathType Leaf)) {
            if (-not $signatureCache.ContainsKey($exe)) {
                try { $signatureCache[$exe] = Get-AuthenticodeSignature -LiteralPath $exe -ErrorAction Stop }
                catch { $signatureCache[$exe] = $null }
            }
            $signature = $signatureCache[$exe]
            if ($null -ne $signature -and $null -ne $signature.SignerCertificate) {
                $signed = $true
                $publisher = [string]$signature.SignerCertificate.Subject
                $publishers.Add($publisher)
                if ($publisher -match '(?i)\bMicrosoft\b') { $microsoft = $true }
                if ($publisher -match $securityPattern) { $security = $true }
                if ([string]$signature.Status -ne 'Valid') { $unknownAction = $true }
            } elseif ($null -eq $signature) { $unknownAction = $true }
        } elseif (-not $microsoft) { $unknownAction = $true }
    }
    if ($actions.Count -eq 0) { $unknownAction = $true }
    $triggers = @($xml.SelectNodes("/*[local-name()='Task']/*[local-name()='Triggers']/*") | ForEach-Object {
        $type = $_.LocalName -replace 'Trigger$', ''
        $start = Text $_ "*[local-name()='StartBoundary']"
        if ($start) { "$type ($start)" } else { $type }
    })
    $state = switch ([int]$task.State) { 0 {'Unknown'} 1 {'Disabled'} 2 {'Queued'} 3 {'Ready'} 4 {'Running'} default {'Unknown'} }
    return [pscustomobject]@{
        name=[string]$task.Name; path=[string]$task.Path; author=$author; principal=$principal
        actions=@($actions.ToArray()); triggers=@($triggers); enabled=[bool]$task.Enabled; state=$state
        microsoft=[bool]$microsoft; signed=[bool]$signed; security=[bool]$security; unknown_action=[bool]$unknownAction
        publishers=@($publishers.ToArray()); identity=(Fingerprint $xml)
    }
}
"#;

#[derive(Clone, Deserialize, Serialize, Debug)]
pub struct ScheduledTask {
    name: String,
    path: String,
    author: String,
    principal: String,
    actions: Vec<String>,
    triggers: Vec<String>,
    enabled: bool,
    state: String,
    microsoft: bool,
    signed: bool,
    #[serde(default)]
    security: bool,
    unknown_action: bool,
    publishers: Vec<String>,
    identity: String,
}

impl ScheduledTask {
    fn protected(&self) -> bool {
        self.microsoft || self.security || self.unknown_action
    }
    fn protection_reason(&self) -> &'static str {
        if self.microsoft {
            "Windows or Microsoft task: review in Task Scheduler."
        } else if self.security {
            "Security software task: review its own settings or Task Scheduler."
        } else {
            "The program could not be verified: review in Task Scheduler."
        }
    }
    fn matches(&self, query: &str, third_party: bool) -> bool {
        (!third_party || !self.microsoft)
            && (query.is_empty()
                || std::iter::once(self.path.as_str())
                    .chain(std::iter::once(self.author.as_str()))
                    .chain(self.actions.iter().map(String::as_str))
                    .chain(self.publishers.iter().map(String::as_str))
                    .any(|s| s.to_lowercase().contains(query)))
    }
}

#[derive(Deserialize)]
struct Inventory {
    tasks: Vec<ScheduledTask>,
    skipped: usize,
    #[serde(default)]
    errors: Vec<String>,
}

fn parse_inventory(text: &str) -> Result<Inventory, String> {
    let mut inventory: Inventory = serde_json::from_str(text.trim_start_matches('\u{feff}').trim())
        .map_err(|e| format!("Could not read the scheduled-task report: {e}"))?;
    if inventory.tasks.is_empty() && inventory.skipped > 0 {
        return Err(format!(
            "No scheduled tasks could be reviewed. Windows reported: {}",
            inventory
                .errors
                .first()
                .map(String::as_str)
                .unwrap_or("task definitions could not be read")
        ));
    }
    if inventory.tasks.iter().any(|t| {
        !t.path.starts_with('\\')
            || t.name.is_empty()
            || !t.path.ends_with(&t.name)
            || t.identity.len() != 64
            || !t.identity.bytes().all(|c| c.is_ascii_hexdigit())
    }) {
        return Err(
            "Windows returned an incomplete task identity. Refresh before changing tasks.".into(),
        );
    }
    inventory
        .tasks
        .sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
    inventory
        .tasks
        .dedup_by(|a, b| a.path.eq_ignore_ascii_case(&b.path));
    Ok(inventory)
}

fn inventory_script() -> String {
    format!(
        "{TASK_HELPERS}\n{}",
        r#"
$rows = New-Object 'System.Collections.Generic.List[object]'
$script:skipped = 0
$script:taskErrors = New-Object 'System.Collections.Generic.List[string]'
function RememberError([string]$message) {
    $script:skipped++
    if ($script:taskErrors.Count -lt 3) { $script:taskErrors.Add($message) }
}
function Visit($folder) {
    try {
        foreach ($task in @($folder.GetTasks(1))) {
            try {
                $row = Describe $task
                if ($null -ne $row) { $rows.Add($row) }
            } catch { RememberError $_.Exception.Message }
        }
        foreach ($child in @($folder.GetFolders(0))) { Visit $child }
    } catch { RememberError $_.Exception.Message }
}
Visit ($service.GetFolder('\'))
[pscustomobject]@{tasks=@($rows.ToArray()); skipped=$script:skipped; errors=@($script:taskErrors.ToArray())} | ConvertTo-Json -Depth 8 -Compress
"#
    )
}

fn enumerate() -> Result<Inventory, String> {
    parse_inventory(&crate::platform::powershell(
        &inventory_script(),
        Duration::from_secs(90),
    )?)
}

fn ps_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn mutation_script(task: &ScheduledTask, enabled: bool) -> Result<String, String> {
    let split = task.path.rfind('\\').ok_or("Invalid scheduled task path")?;
    let folder = &task.path[..=split];
    if task.path[split + 1..] != task.name {
        return Err("Invalid scheduled task identity".into());
    }
    Ok(format!(
        "{TASK_HELPERS}\n$folder = $service.GetFolder({folder})\n$task = $folder.GetTask({name})\n$row = Describe $task\n\
         if ($null -eq $row -or -not (Same $row.path {path}) -or $row.identity -ne {identity}) {{ throw 'The task definition changed. Refresh and review it again.' }}\n\
         if ($row.enabled -ne ${expected}) {{ throw 'The task state changed outside PCSweep. Refresh and review it again.' }}\n\
         if ($row.microsoft -or $row.security -or $row.unknown_action) {{ throw 'This task is protected. Use the program settings or Task Scheduler.' }}\n\
         $task.Enabled = ${enabled}\n\
         $fresh = $folder.GetTask({name})\n\
         if ([bool]$fresh.Enabled -ne ${enabled}) {{ throw 'Windows did not apply the requested state.' }}\n\
         'Task state verified'",
        folder = ps_literal(folder),
        name = ps_literal(&task.name),
        path = ps_literal(&task.path),
        identity = ps_literal(&task.identity),
        expected = task.enabled,
    ))
}

#[derive(Clone, Deserialize, Serialize)]
struct UndoRecord {
    version: u32,
    path: String,
    identity: String,
    original_enabled: bool,
    last_applied: bool,
}

fn undo_directory() -> PathBuf {
    util::log_path().with_file_name("background-undo")
}
fn undo_path(directory: &Path, task: &ScheduledTask) -> PathBuf {
    directory.join(format!(
        "{:032x}.json",
        xxh3_128(task.path.to_lowercase().as_bytes())
    ))
}
fn read_undo(directory: &Path, task: &ScheduledTask) -> Result<Option<UndoRecord>, String> {
    let path = undo_path(directory, task);
    match fs::read(path) {
        Ok(bytes) => {
            let record: UndoRecord = serde_json::from_slice(&bytes).map_err(|_| {
                "The saved original task state could not be read. No task was changed.".to_string()
            })?;
            if record.version != 1 || !record.path.eq_ignore_ascii_case(&task.path) {
                return Err("The saved task identity does not match. No task was changed.".into());
            }
            Ok(Some(record))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Could not read the saved original state: {e}")),
    }
}
fn write_undo(directory: &Path, task: &ScheduledTask, record: &UndoRecord) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|e| format!("Could not save an undo record: {e}"))?;
    let final_path = undo_path(directory, task);
    // The destination file is never truncated. A durable replacement leaves the
    // earlier original intact if the write or rename fails.
    let temp_path = final_path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|e| format!("Could not create the undo record: {e}"))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("Could not persist the undo record: {e}"))?;
        drop(file);
        if final_path.exists() {
            // Windows ReplaceFile preserves atomic replacement semantics.
            use std::os::windows::ffi::OsStrExt;
            let old: Vec<u16> = final_path
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect();
            let new: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
            let ok = unsafe {
                windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
                    old.as_ptr(),
                    new.as_ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            if ok == 0 {
                return Err(format!(
                    "Could not replace the undo record: {}",
                    std::io::Error::last_os_error()
                ));
            }
        } else {
            fs::rename(&temp_path, &final_path)
                .map_err(|e| format!("Could not save the undo record: {e}"))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn change_with_backend(
    task: &ScheduledTask,
    enabled: bool,
    restore: bool,
    directory: &Path,
    run: impl FnOnce(&ScheduledTask, bool) -> Result<(), String>,
) -> Result<String, String> {
    if task.protected() {
        return Err(task.protection_reason().into());
    }
    let existing = read_undo(directory, task)?;
    if existing
        .as_ref()
        .is_some_and(|r| r.identity != task.identity)
    {
        return Err("The task definition changed since the saved original. Review it in Task Scheduler; its old state has been preserved.".into());
    }
    let target;
    if restore {
        let record = existing.ok_or("No saved original state exists for this task.")?;
        if task.enabled != record.last_applied && task.enabled != record.original_enabled {
            return Err(
                "The task state changed outside PCSweep. Review it in Task Scheduler.".into(),
            );
        }
        target = record.original_enabled;
    } else {
        target = enabled;
        let mut record = existing.unwrap_or_else(|| UndoRecord {
            version: 1,
            path: task.path.clone(),
            identity: task.identity.clone(),
            original_enabled: task.enabled,
            last_applied: target,
        });
        record.last_applied = target;
        write_undo(directory, task, &record)?;
    }
    // The backend checks the exact current definition, scope, protection and
    // expected state again. An error leaves the undo record available to retry.
    run(task, target)?;
    if restore {
        fs::remove_file(undo_path(directory, task)).map_err(|e|
            format!("The original task state was restored, but its undo record could not be removed: {e}"))?;
    }
    Ok(format!(
        "{}: {}{}",
        task.path,
        if target { "enabled" } else { "disabled" },
        if restore {
            " (original state restored)"
        } else {
            ""
        }
    ))
}

fn change(task: ScheduledTask, enabled: bool, restore: bool) -> Result<String, String> {
    change_with_backend(
        &task,
        enabled,
        restore,
        &undo_directory(),
        |task, target| {
            let script = mutation_script(task, target)?;
            crate::platform::powershell(&script, Duration::from_secs(30)).map(|_| ())
        },
    )
}

#[derive(Clone)]
pub enum BackgroundAction {
    SetTask {
        task: ScheduledTask,
        enabled: bool,
        restore: bool,
    },
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BackgroundView {
    #[default]
    Processes,
    Tasks,
    Browsers,
}

#[derive(Default)]
pub struct BackgroundState {
    view: BackgroundView,
    load: Task<Result<(Inventory, HashMap<String, UndoRecord>, Vec<String>), String>>,
    change: Task<Result<String, String>>,
    tasks: Vec<ScheduledTask>,
    undo: HashMap<String, UndoRecord>,
    warnings: Vec<String>,
    filter: String,
    show_windows: bool,
    selected: Option<String>,
    loaded: bool,
    error: Option<String>,
    skipped: usize,
}
impl BackgroundState {
    pub(crate) fn showing_processes(&self) -> bool {
        self.view == BackgroundView::Processes
    }

    #[cfg(test)]
    pub(crate) fn show_task_preview(&mut self) {
        self.view = BackgroundView::Tasks;
    }

    pub fn busy(&self) -> bool {
        self.change.running()
    }
}

enum UiAction {
    Refresh,
    Confirm(ScheduledTask, bool, bool),
    Browser(&'static str, &'static str),
    Scheduler,
}

fn browser_path(browser: &str) -> Option<PathBuf> {
    let relative = match browser {
        "edge" => r"Microsoft\Edge\Application\msedge.exe",
        "chrome" => r"Google\Chrome\Application\chrome.exe",
        "firefox" => r"Mozilla Firefox\firefox.exe",
        _ => return None,
    };
    ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| util::env(v))
        .map(|p| p.join(relative))
        .find(|p| p.is_file())
}

impl App {
    fn refresh_background(&mut self, ctx: &egui::Context) {
        if self.background.load.running() || self.background.busy() {
            return;
        }
        self.background.error = None;
        self.background.load = Task::spawn(ctx, || {
            let inventory = enumerate()?;
            let mut undo = HashMap::new();
            let mut warnings = Vec::new();
            for task in &inventory.tasks {
                match read_undo(&undo_directory(), task) {
                    Ok(Some(record)) => {
                        undo.insert(task.path.to_lowercase(), record);
                    }
                    Ok(None) => {}
                    Err(e) => warnings.push(format!("{}: {e}", task.path)),
                }
            }
            Ok((inventory, undo, warnings))
        });
    }

    pub fn poll_background(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.background.load.poll() {
            match result.and_then(|r| r) {
                Ok((inventory, undo, warnings)) => {
                    self.background.tasks = inventory.tasks;
                    self.background.skipped = inventory.skipped;
                    self.background.undo = undo;
                    self.background.warnings = warnings;
                    self.background.loaded = true;
                }
                Err(e) => self.background.error = Some(e),
            }
        }
        if let Some(result) = self.background.change.poll() {
            match result.and_then(|r| r) {
                Ok(message) => {
                    util::log(&message);
                    self.toast(ToastKind::Success, message);
                }
                Err(e) => {
                    util::log(format!("Background task change failed: {e}"));
                    self.toast(ToastKind::Error, e);
                }
            }
            self.refresh_background(ctx);
        }
    }

    pub fn execute_background(&mut self, action: BackgroundAction, ctx: &egui::Context) {
        if self.background.busy() || self.background.load.running() {
            self.toast(
                ToastKind::Info,
                "Wait for the background-task review to finish.",
            );
            return;
        }
        match action {
            BackgroundAction::SetTask {
                task,
                enabled,
                restore,
            } => {
                self.background.change = Task::spawn(ctx, move || change(task, enabled, restore));
            }
        }
    }

    pub fn page_background(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        widgets::page_header(
            ui,
            "Background activity",
            "See what is running now and review what starts later.",
        );
        ui.horizontal_wrapped(|ui| {
            for (view, label) in [
                (BackgroundView::Processes, "Running processes"),
                (BackgroundView::Tasks, "Scheduled tasks"),
                (BackgroundView::Browsers, "Browsers"),
            ] {
                if ui
                    .add(widgets::chip(label, self.background.view == view))
                    .clicked()
                {
                    self.background.view = view;
                }
            }
        });
        ui.add_space(8.0);
        if self.background.showing_processes() {
            self.background_processes(ui);
            return;
        }
        let scheduled = self.background.view == BackgroundView::Tasks;
        if scheduled
            && !self.background.loaded
            && self.background.error.is_none()
            && !self.background.load.running()
        {
            self.refresh_background(&ctx);
        }
        let mut actions = Vec::new();
        if scheduled {
            self.render_tasks(ui, &mut actions);
        } else {
            Self::render_browsers(ui, &mut actions);
        }
        for action in actions {
            match action {
                UiAction::Refresh => self.refresh_background(&ctx),
                UiAction::Scheduler => {
                    let _ = std::process::Command::new("mmc.exe")
                        .arg("taskschd.msc")
                        .spawn();
                }
                UiAction::Browser(browser, page) => {
                    let result = browser_path(browser)
                        .ok_or("This browser was not found in its standard install locations.")
                        .and_then(|exe| {
                            std::process::Command::new(exe)
                                .arg(page)
                                .spawn()
                                .map(|_| ())
                                .map_err(|_| "The browser settings could not be opened.")
                        });
                    if let Err(e) = result {
                        self.toast(ToastKind::Info, e);
                    }
                }
                UiAction::Confirm(task, enabled, restore) => {
                    let verb = if restore {
                        "Restore original state"
                    } else if enabled {
                        "Enable"
                    } else {
                        "Disable"
                    };
                    self.modal = Some(Modal {
                        title: format!("{verb} {}?", task.name),
                        body: if restore {
                            "PCSweep will restore this task's saved original enabled state, after checking that its definition still matches.".into()
                        } else {
                            format!("Only this scheduled task will be {}. Its registration and files are kept. Only future launches are affected; already-running jobs continue. Disabling a task may pause app updates, backups or reminders; check its purpose first.", if enabled { "enabled" } else { "disabled" })
                        },
                        details: std::iter::once(task.path.clone()).chain(task.actions.iter().cloned()).collect(),
                        note: Some("Windows may require administrator rights. The original enabled state is saved before a change so you can undo it here.".into()),
                        confirm: verb.into(), danger: !enabled && !restore,
                        action: Action::Background(BackgroundAction::SetTask { task, enabled, restore }),
                    });
                }
            }
        }
    }

    fn render_browsers(ui: &mut Ui, actions: &mut Vec<UiAction>) {
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::strong("Browser background activity", 16.0));
                if ui
                    .add(widgets::small("Edge background & Startup Boost"))
                    .clicked()
                {
                    actions.push(UiAction::Browser("edge", "edge://settings/system"));
                }
            });
            ui.label(widgets::dim("Edge can stay running after its windows close. Review background extensions and Startup Boost in Edge settings. Organization-managed controls remain managed."));
            ui.horizontal_wrapped(|ui| {
                ui.label(widgets::faint("Review extensions:"));
                for (name, browser, page) in [
                    ("Edge", "edge", "edge://extensions"),
                    ("Chrome", "chrome", "chrome://extensions"),
                    ("Firefox", "firefox", "about:addons"),
                ] {
                    if ui.add(widgets::small(name)).clicked() {
                        actions.push(UiAction::Browser(browser, page));
                    }
                }
            });
        });
    }

    fn render_tasks(&mut self, ui: &mut Ui, actions: &mut Vec<UiAction>) {
        let busy = self.background.busy() || self.background.load.running();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!busy, widgets::small("Refresh tasks"))
                .clicked()
            {
                actions.push(UiAction::Refresh);
            }
            if ui.add(widgets::small("Open Task Scheduler")).clicked() {
                actions.push(UiAction::Scheduler);
            }
            widgets::search_box(
                ui,
                &mut self.background.filter,
                "Search tasks, publishers or commands",
                280.0,
            );
            ui.checkbox(&mut self.background.show_windows, "Include Windows tasks");
            if busy {
                ui.spinner();
            }
        });
        if let Some(error) = &self.background.error {
            ui.colored_label(WARN, error);
        }
        if self.background.skipped > 0 {
            ui.label(widgets::faint(format!(
                "{} tasks or folders could not be read. Other users' private tasks are excluded.",
                self.background.skipped
            )));
        }
        for warning in self.background.warnings.iter().take(2) {
            ui.colored_label(WARN, warning);
        }
        let query = self.background.filter.trim().to_lowercase();
        let visible: Vec<_> = self
            .background
            .tasks
            .iter()
            .filter(|t| t.matches(&query, !self.background.show_windows))
            .collect();
        ui.label(widgets::faint(format!(
            "{} tasks shown · Windows/security tasks and unverified actions are protected.",
            visible.len()
        )));
        if let Some(selected) = self
            .background
            .selected
            .as_ref()
            .and_then(|p| self.background.tasks.iter().find(|t| &t.path == p))
        {
            widgets::tight_frame().show(ui, |ui| {
                ui.label(widgets::strong(&selected.path, 14.0));
                ScrollArea::vertical()
                    .id_salt("background_details")
                    .max_height(110.0)
                    .show(ui, |ui| {
                        ui.label(widgets::faint(format!(
                            "Author: {} · Runs as: {}",
                            if selected.author.is_empty() {
                                "Not provided"
                            } else {
                                &selected.author
                            },
                            selected.principal
                        )));
                        ui.label(widgets::faint(if selected.signed { "A digital signer was found. A signature does not establish whether a task is necessary." } else { "No digital signer was found. This alone does not indicate harmful software." }));
                    if !selected.publishers.is_empty() {
                            ui.label(widgets::dim(format!(
                                "Signer: {}",
                                selected.publishers.join("; ")
                            )));
                        }
                        for command in &selected.actions {
                            ui.add(Label::new(widgets::dim(command)).wrap());
                        }
                        ui.label(widgets::faint(format!(
                            "Triggers: {}",
                            if selected.triggers.is_empty() {
                                "On demand / no trigger listed".into()
                            } else {
                                selected.triggers.join(", ")
                            }
                        )));
                        if selected.protected() {
                            ui.colored_label(WARN, selected.protection_reason());
                        }
                    });
            });
        }
        if visible.is_empty() && !busy {
            ui.add_space(12.0);
            ui.label(widgets::dim(if self.background.loaded {
                "No tasks match this view."
            } else {
                "Refresh to review scheduled tasks."
            }));
        }
        ScrollArea::vertical()
            .id_salt("background_tasks")
            .auto_shrink([false, false])
            .show_rows(ui, 48.0, visible.len(), |ui, range| {
                for task in &visible[range] {
                    widgets::row(ui, 48.0, |ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if task.protected() {
                                ui.add_enabled(false, widgets::small("Protected"))
                                    .on_disabled_hover_text(task.protection_reason());
                            } else {
                                if ui
                                    .add_enabled(
                                        !busy,
                                        widgets::small(if task.enabled {
                                            "Disable…"
                                        } else {
                                            "Enable…"
                                        }),
                                    )
                                    .clicked()
                                {
                                    actions.push(UiAction::Confirm(
                                        (*task).clone(),
                                        !task.enabled,
                                        false,
                                    ));
                                }
                                if let Some(original) =
                                    self.background.undo.get(&task.path.to_lowercase())
                                {
                                    if ui
                                        .add_enabled(
                                            !busy && original.identity == task.identity,
                                            widgets::small("Undo…"),
                                        )
                                        .on_disabled_hover_text("The task definition changed or another task operation is running. Refresh or review it in Task Scheduler.")
                                        .on_hover_text(format!(
                                            "Restore original state: {}",
                                            if original.original_enabled {
                                                "enabled"
                                            } else {
                                                "disabled"
                                            }
                                        ))
                                        .clicked()
                                    {
                                        actions.push(UiAction::Confirm(
                                            (*task).clone(),
                                            original.original_enabled,
                                            true,
                                        ));
                                    }
                                }
                            }
                            ui.label(
                                RichText::new(&task.state)
                                    .size(12.0)
                                    .color(if task.enabled { GOOD } else { TEXT_FAINT }),
                            );
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.vertical(|ui| {
                                    if ui
                                        .selectable_label(
                                            self.background.selected.as_ref() == Some(&task.path),
                                            &task.name,
                                        )
                                        .on_hover_text(&task.path)
                                        .clicked()
                                    {
                                        self.background.selected = Some(task.path.clone());
                                    }
                                    ui.add(
                                        Label::new(widgets::faint(if task.author.is_empty() {
                                            &task.path
                                        } else {
                                            &task.author
                                        }))
                                        .truncate(),
                                    );
                                });
                            });
                        });
                    });
                }
            });
    }
}

/// Representative local UI data; never queries Scheduler or changes the system.
#[cfg(test)]
pub(crate) fn preview_fixture() -> BackgroundState {
    let task = ScheduledTask {
        name: "Cloud backup nightly verification and maintenance".into(),
        path: r"\Example Vendor\Cloud backup nightly verification and maintenance".into(),
        author: "Example Vendor Software Corporation".into(),
        principal: "Current account".into(),
        actions: vec![
            r#"C:\Program Files\Example Vendor\Cloud Backup\backup.exe --verify --background"#
                .into(),
        ],
        triggers: vec!["Daily (2026-11-15T02:30:00)".into(), "Logon".into()],
        enabled: true,
        state: "Ready".into(),
        microsoft: false,
        signed: true,
        security: false,
        unknown_action: false,
        publishers: vec!["CN=Example Vendor Software Corporation, O=Example Vendor".into()],
        identity: "a".repeat(64),
    };
    let mut reminder = task.clone();
    reminder.name = "Example notes synchronization".into();
    reminder.path = r"\Example Vendor\Example notes synchronization".into();
    reminder.enabled = false;
    reminder.state = "Disabled".into();
    reminder.actions =
        vec![r#"C:\Users\Example\AppData\Local\Example Notes\notes-sync.exe --quiet"#.into()];
    let mut security = task.clone();
    security.name = "Endpoint protection maintenance".into();
    security.path = r"\Example Security\Endpoint protection maintenance".into();
    security.security = true;
    security.author = "Example Security".into();
    let mut undo = HashMap::new();
    undo.insert(
        reminder.path.to_lowercase(),
        UndoRecord {
            version: 1,
            path: reminder.path.clone(),
            identity: reminder.identity.clone(),
            original_enabled: true,
            last_applied: false,
        },
    );
    BackgroundState {
        selected: Some(task.path.clone()),
        tasks: vec![task, reminder, security],
        undo,
        loaded: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "pcsweep-background-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn example() -> ScheduledTask {
        ScheduledTask {
            name: "Update [review] 'one'".into(),
            path: "\\Vendor\\Update [review] 'one'".into(),
            author: "Example vendor".into(),
            principal: "Current user".into(),
            actions: vec![r"C:\Vendor\updater.exe --daily".into()],
            triggers: vec!["Daily".into()],
            enabled: true,
            state: "Ready".into(),
            microsoft: false,
            signed: false,
            security: false,
            unknown_action: false,
            publishers: vec![],
            identity: "a".repeat(64),
        }
    }
    #[test]
    fn parses_fake_task_report_and_filters_without_security_claims() {
        let task = example();
        let text = serde_json::json!({"tasks": [task], "skipped": 2}).to_string();
        let inventory = parse_inventory(&text).unwrap();
        assert_eq!(inventory.skipped, 2);
        assert!(inventory.tasks[0].matches("vendor", true));
        assert!(!inventory.tasks[0].matches("missing", true));
        let mut windows = inventory.tasks[0].clone();
        windows.microsoft = true;
        assert!(!windows.matches("", true));
        assert!(windows.matches("", false));
    }
    #[test]
    fn security_windows_and_unverified_actions_are_protected() {
        let dir = TestDir::new();
        for field in 0..3 {
            let mut task = example();
            match field {
                0 => task.security = true,
                1 => task.microsoft = true,
                _ => task.unknown_action = true,
            }
            assert!(
                change_with_backend(&task, false, false, &dir.0, |_, _| panic!(
                    "protected task reached backend"
                ))
                .is_err()
            );
        }
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    }
    #[test]
    fn saves_original_before_change_and_restores_it() {
        let dir = TestDir::new();
        let mut task = example();
        change_with_backend(&task, false, false, &dir.0, |t, enabled| {
            assert!(!enabled);
            assert!(read_undo(&dir.0, t).unwrap().unwrap().original_enabled);
            Ok(())
        })
        .unwrap();
        task.enabled = false;
        change_with_backend(&task, true, true, &dir.0, |_, enabled| {
            assert!(enabled);
            Ok(())
        })
        .unwrap();
        assert!(read_undo(&dir.0, &task).unwrap().is_none());
    }
    #[test]
    fn failed_backend_keeps_durable_undo_and_rejects_replaced_definition() {
        let dir = TestDir::new();
        let mut task = example();
        assert!(change_with_backend(&task, false, false, &dir.0, |_, _| Err(
            "permission denied".into()
        ))
        .is_err());
        assert!(read_undo(&dir.0, &task).unwrap().is_some());
        task.identity = "b".repeat(64);
        assert!(
            change_with_backend(&task, true, true, &dir.0, |_, _| panic!(
                "changed task reached backend"
            ))
            .is_err()
        );
    }
    #[test]
    fn repeated_changes_preserve_the_first_original_state() {
        let dir = TestDir::new();
        let mut task = example();
        change_with_backend(&task, false, false, &dir.0, |_, _| Ok(())).unwrap();
        task.enabled = false;
        change_with_backend(&task, true, false, &dir.0, |_, _| Ok(())).unwrap();
        assert!(read_undo(&dir.0, &task).unwrap().unwrap().original_enabled);
    }

    #[test]
    fn signed_third_party_task_can_be_reviewed_and_changed() {
        let dir = TestDir::new();
        let mut task = example();
        task.signed = true;
        task.publishers = vec!["CN=Example Vendor, O=Example Vendor".into()];
        assert!(!task.protected());
        change_with_backend(&task, false, false, &dir.0, |_, target| {
            assert!(!target);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn corrupt_or_unwritable_backup_prevents_any_mutation() {
        let dir = TestDir::new();
        let task = example();
        fs::write(undo_path(&dir.0, &task), b"incomplete backup").unwrap();
        assert!(
            change_with_backend(&task, false, false, &dir.0, |_, _| panic!(
                "corrupt backup reached backend"
            ))
            .is_err()
        );
        let not_directory = dir.0.join("not-a-directory");
        fs::write(&not_directory, b"occupied").unwrap();
        assert!(
            change_with_backend(&task, false, false, &not_directory, |_, _| panic!(
                "unwritable backup reached backend"
            ))
            .is_err()
        );
    }

    #[test]
    fn windows_scripts_parse_without_executing_any_task_changes() {
        for script in [
            inventory_script(),
            mutation_script(&example(), false).unwrap(),
        ] {
            let parser = format!("$tokens = $null; $parseErrors = $null; [void][System.Management.Automation.Language.Parser]::ParseInput({}, [ref]$tokens, [ref]$parseErrors); if ($parseErrors.Count -gt 0) {{ throw (($parseErrors | ForEach-Object {{ $_.Message }}) -join '; ') }}; 'syntax-ok'", ps_literal(&script));
            let output = crate::platform::powershell(&parser, Duration::from_secs(20))
                .expect("Internal scheduled-task PowerShell must parse");
            assert_eq!(output.trim(), "syntax-ok");
        }
    }

    #[test]
    fn windows_read_only_task_inventory_smoke() {
        match enumerate() {
            Ok(inventory) => {
                // The real report must pass the exact parser used by the UI,
                // including full paths and definition fingerprints.
                for task in inventory.tasks {
                    assert!(task.path.starts_with('\\'));
                    assert_eq!(task.identity.len(), 64);
                }
            }
            Err(error) => {
                // Service access can be denied on managed machines. Programming
                // errors, malformed JSON and timeouts must still fail this test.
                assert!(
                    error.contains("Access is denied")
                        || error.contains("The service cannot be started")
                        || error.contains("RPC server is unavailable"),
                    "Unexpected inventory failure: {error}"
                );
                eprintln!("Scheduled task inventory unavailable: {error}");
            }
        }
    }
    #[test]
    fn mutation_uses_literal_identity_and_expected_state() {
        let task = example();
        let script = mutation_script(&task, false).unwrap();
        assert!(script.contains("GetTask('Update [review] ''one''')"));
        assert!(script.contains("$row.enabled -ne $true"));
        assert!(script.contains("$task.Enabled = $false"));
        assert!(script.contains(&task.identity));
    }
}
