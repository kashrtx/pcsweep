//! Windows Defender status and explicit, reversible security actions.
//! All PowerShell runs off the UI thread. Successful command execution is never
//! treated as proof that a scan finished or that the computer is threat-free.
use crate::app::{Action, App, Modal, Page, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util;
use crate::widgets::{self, ic};
use chrono::{DateTime, Local, Utc};
use eframe::egui::{self, RichText, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const STATUS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$WarningPreference = 'SilentlyContinue'
function Stamp($v) { if ($null -ne $v -and $v.Year -gt 2000) { $v.ToUniversalTime().ToString('o') } else { $null } }
$result = [ordered]@{ available = $false; error = $null; mode = $null; service = $null; antivirus = $null; realtime = $null; tamper = $null; signatures_outdated = $null; signature_time = $null; signature_version = $null; quick_start = $null; quick_end = $null; full_start = $null; full_end = $null; reboot_required = $null; pua = $null; policy_managed = $false; products = @(); detections = @(); detection_error = $null }
try {
    $s = Get-MpComputerStatus -ErrorAction Stop
    $result.available = $true
    $result.mode = [string]$s.AMRunningMode
    $result.service = $s.AMServiceEnabled
    $result.antivirus = $s.AntivirusEnabled
    $result.realtime = $s.RealTimeProtectionEnabled
    $result.tamper = $s.IsTamperProtected
    $result.signatures_outdated = $s.DefenderSignaturesOutOfDate
    $result.signature_time = Stamp $s.AntivirusSignatureLastUpdated
    $result.signature_version = [string]$s.AntivirusSignatureVersion
    $result.quick_start = Stamp $s.QuickScanStartTime
    $result.quick_end = Stamp $s.QuickScanEndTime
    $result.full_start = Stamp $s.FullScanStartTime
    $result.full_end = Stamp $s.FullScanEndTime
    $result.reboot_required = $s.RebootRequired
    $result.pua = [int](Get-MpPreference -ErrorAction Stop).PUAProtection
} catch { $result.error = $_.Exception.Message }
try {
    $result.products = @(Get-CimInstance -Namespace root/SecurityCenter2 -ClassName AntivirusProduct -ErrorAction Stop | ForEach-Object { [string]$_.displayName })
} catch { }
foreach ($pair in @(@('HKLM:\SOFTWARE\Policies\Microsoft\Windows Defender', 'PUAProtection'), @('HKLM:\SOFTWARE\Policies\Microsoft\Windows Defender\MpEngine', 'MpEnablePus'))) {
    $v = Get-ItemProperty -LiteralPath $pair[0] -Name $pair[1] -ErrorAction SilentlyContinue
    if ($null -ne $v) { $result.policy_managed = $true }
}
if ($result.available) {
    try {
        $names = @{}
        Get-MpThreat -ErrorAction Stop | ForEach-Object { $names[[string]$_.ThreatID] = [string]$_.ThreatName }
        $result.detections = @(Get-MpThreatDetection -ErrorAction Stop | Sort-Object InitialDetectionTime -Descending | Select-Object -First 20 | ForEach-Object {
            $label = $names[[string]$_.ThreatID]
            if ([string]::IsNullOrWhiteSpace($label)) { $label = 'Threat ID ' + $_.ThreatID }
            [ordered]@{ name = $label; when = (Stamp $_.InitialDetectionTime); action_success = $_.ActionSuccess; status_id = [int]$_.ThreatStatusID; resources = @($_.Resources | Where-Object { $null -ne $_ } | Select-Object -First 3 | ForEach-Object { [string]$_ }) }
        })
    } catch { $result.detection_error = $_.Exception.Message }
}
$result | ConvertTo-Json -Depth 6 -Compress
"#;

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Detection {
    name: String,
    when: Option<String>,
    action_success: Option<bool>,
    status_id: u32,
    resources: Vec<String>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Snapshot {
    available: bool,
    error: Option<String>,
    mode: Option<String>,
    service: Option<bool>,
    antivirus: Option<bool>,
    realtime: Option<bool>,
    tamper: Option<bool>,
    signatures_outdated: Option<bool>,
    signature_time: Option<String>,
    signature_version: Option<String>,
    quick_start: Option<String>,
    quick_end: Option<String>,
    full_start: Option<String>,
    full_end: Option<String>,
    reboot_required: Option<bool>,
    pua: Option<u32>,
    policy_managed: bool,
    products: Vec<String>,
    detections: Vec<Detection>,
    detection_error: Option<String>,
    #[serde(skip)]
    backup: Option<PuaBackup>,
    #[serde(skip)]
    backup_error: Option<String>,
}

impl Snapshot {
    fn primary(&self) -> bool {
        self.available
            && self
                .mode
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case("Normal"))
            && self.antivirus == Some(true)
            && self.service == Some(true)
    }
    fn scan_times(&self, kind: ScanKind) -> (&Option<String>, &Option<String>) {
        match kind {
            ScanKind::Quick => (&self.quick_start, &self.quick_end),
            ScanKind::Full => (&self.full_start, &self.full_end),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanKind {
    Quick,
    Full,
}
impl ScanKind {
    fn label(self) -> &'static str {
        match self {
            Self::Quick => "Quick scan",
            Self::Full => "Full scan",
        }
    }
    fn argument(self) -> &'static str {
        match self {
            Self::Quick => "QuickScan",
            Self::Full => "FullScan",
        }
    }
}

#[derive(Clone, Debug)]
pub enum SecurityAction {
    UpdateSignatures,
    QuickScan,
    FullScan,
    EnablePua,
    UndoPua,
    OpenOfflineScan,
}

#[derive(Clone, Deserialize, Serialize)]
struct PuaBackup {
    version: u32,
    original: u32,
}

fn backup_path() -> Result<PathBuf, String> {
    util::env("LOCALAPPDATA")
        .map(|p| p.join("PCSweep").join("security-pua-backup.json"))
        .ok_or_else(|| "Windows did not provide a local settings folder.".into())
}
fn read_backup() -> Result<Option<PuaBackup>, String> {
    let path = backup_path()?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot inspect the PUA undo record: {e}")),
    };
    if !metadata.is_file() || util::is_reparse(&metadata) || metadata.len() > 4096 {
        return Err("PUA undo record is invalid; no setting was changed.".into());
    }
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Cannot read the PUA undo record: {e}")),
    };
    if data.len() > 4096 {
        return Err("PUA undo record is invalid; no setting was changed.".into());
    }
    let record: PuaBackup = serde_json::from_slice(&data)
        .map_err(|_| "PUA undo record is invalid; no setting was changed.")?;
    if record.version != 1 || !matches!(record.original, 0 | 2) {
        return Err("PUA undo record is invalid; no setting was changed.".into());
    }
    Ok(Some(record))
}
fn save_backup(record: &PuaBackup) -> Result<(), String> {
    let path = backup_path()?;
    fs::create_dir_all(path.parent().ok_or("Invalid settings folder")?)
        .map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| format!("Cannot create the PUA undo record; nothing was changed: {e}"))?;
    if let Err(e) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(format!(
            "Cannot save the PUA undo record; nothing was changed: {e}"
        ));
    }
    Ok(())
}

fn query() -> Result<Snapshot, String> {
    let json = crate::platform::powershell(STATUS_SCRIPT, Duration::from_secs(45))?;
    let mut value: Snapshot = serde_json::from_str(json.trim().trim_start_matches('\u{feff}'))
        .map_err(|e| format!("Windows returned an unreadable security status: {e}"))?;
    match read_backup() {
        Ok(v) => value.backup = v,
        Err(e) => value.backup_error = Some(e),
    }
    Ok(value)
}

fn pua_change(restore: bool) -> Result<String, String> {
    // Recheck immediately before changing anything. Never bypass policy or tamper protection.
    let before = query()?;
    if !before.primary() {
        return Err(
            "Defender is not the active antivirus. Open Windows Security to manage your provider."
                .into(),
        );
    }
    if before.policy_managed {
        return Err("PUA settings are managed by policy. Change them in Windows Security or contact your administrator; PCSweep will not bypass policy.".into());
    }
    let original = before
        .pua
        .ok_or("Windows did not report the current PUA setting.")?;
    let target = if restore {
        let backup = read_backup()?.ok_or("PCSweep has no saved PUA setting to restore.")?;
        if original == backup.original {
            fs::remove_file(backup_path()?).map_err(|e| format!("PUA already has its original setting, but its undo record could not be removed: {e}"))?;
            return Ok("PUA already has its saved original setting. The completed undo record was cleared.".into());
        }
        if original != 1 {
            return Err("PUA protection changed outside PCSweep. The saved setting was not applied; your current setting was kept.".into());
        }
        backup.original
    } else {
        if original == 1 {
            return Ok("Potentially unwanted app blocking is already on.".into());
        }
        if !matches!(original, 0 | 2) {
            return Err("Windows reported an unfamiliar PUA setting. Nothing was changed.".into());
        }
        if read_backup()?.is_some() {
            return Err("An earlier PUA undo record exists. Restore that change in PCSweep before changing it again.".into());
        }
        save_backup(&PuaBackup {
            version: 1,
            original,
        })?;
        1
    };
    // All script values come from checked numeric enums, never user or detection text.
    let script = format!("$ErrorActionPreference='Stop'; Set-MpPreference -PUAProtection {target} -ErrorAction Stop; $v=[int](Get-MpPreference -ErrorAction Stop).PUAProtection; if ($v -ne {target}) {{ throw 'Windows refused the PUA setting. It may be managed or tamper protected.' }}; 'PUA setting verified.'");
    crate::platform::powershell(&script, Duration::from_secs(45))?;
    if restore {
        fs::remove_file(backup_path()?).map_err(|e| format!("The original PUA setting was restored, but its undo record could not be removed: {e}"))?;
        Ok("Your original potentially unwanted app setting was restored.".into())
    } else {
        Ok(
            "Potentially unwanted app blocking is on. PCSweep saved the previous setting for Undo."
                .into(),
        )
    }
}

struct OperationResult {
    message: String,
    scan: Option<(ScanKind, DateTime<Utc>)>,
}
struct ScanWatch {
    kind: ScanKind,
    requested: DateTime<Utc>,
    began: Instant,
    command_returned: bool,
    returned_at: Option<Instant>,
}
impl ScanWatch {
    fn expired(&self, now: Instant) -> bool {
        self.command_returned
            && self
                .returned_at
                .is_some_and(|t| now.saturating_duration_since(t) >= Duration::from_secs(120))
    }
    fn completed(&self, snapshot: &Snapshot) -> bool {
        let (start, end) = snapshot.scan_times(self.kind);
        let (Some(start), Some(end)) = (parse_time(start.as_deref()), parse_time(end.as_deref()))
        else {
            return false;
        };
        start >= self.requested - chrono::Duration::seconds(2)
            && end >= start
            && end >= self.requested
    }
    fn started(&self, snapshot: &Snapshot) -> bool {
        let (start, _) = snapshot.scan_times(self.kind);
        parse_time(start.as_deref())
            .is_some_and(|s| s >= self.requested - chrono::Duration::seconds(2))
    }
}

pub struct SecurityState {
    snapshot: Option<Snapshot>,
    read: Task<Result<Snapshot, String>>,
    operation: Task<Result<OperationResult, String>>,
    scan: Option<ScanWatch>,
    refreshed: Option<Instant>,
    read_error: Option<String>,
    result: Option<String>,
    operation_label: Option<&'static str>,
}
impl Default for SecurityState {
    fn default() -> Self {
        Self {
            snapshot: None,
            read: Task::default(),
            operation: Task::default(),
            scan: None,
            refreshed: None,
            read_error: None,
            result: None,
            operation_label: None,
        }
    }
}
impl SecurityState {
    pub fn busy(&self) -> bool {
        self.operation.running() || self.scan.is_some()
    }
}

fn parse_time(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}
fn show_time(value: Option<&str>) -> String {
    parse_time(value)
        .map(|t| {
            t.with_timezone(&Local)
                .format("%b %d, %Y %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Not reported".into())
}
fn clean_text(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(600)
        .collect()
}
fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "On",
        Some(false) => "Off",
        None => "Not reported",
    }
}
fn detection_status(value: &Detection) -> &'static str {
    if value.action_success == Some(false) {
        return "Action needs review in Protection history";
    }
    match value.status_id {
        1 => "Detected",
        2 => "Cleaned",
        3 => "Quarantined",
        4 => "Removed",
        5 => "Allowed",
        6 => "Blocked",
        _ => "See Protection history for current status",
    }
}

impl App {
    fn refresh_security(&mut self, ctx: &egui::Context) {
        if !self.security.read.running() {
            self.security.read = Task::spawn(ctx, query);
            self.security.refreshed = Some(Instant::now());
        }
    }

    pub fn poll_security(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.security.read.poll() {
            match result.and_then(|r| r) {
                Ok(snapshot) => {
                    self.security.snapshot = Some(snapshot);
                    self.security.read_error = None;
                }
                Err(e) => self.security.read_error = Some(clean_text(&e)),
            }
        }
        if let Some(result) = self.security.operation.poll() {
            self.security.operation_label = None;
            match result.and_then(|r| r) {
                Ok(result) => {
                    if let Some((kind, requested)) = result.scan {
                        if let Some(watch) = &mut self.security.scan {
                            watch.requested = requested;
                            watch.command_returned = true;
                            watch.returned_at = Some(Instant::now());
                        } else {
                            self.security.scan = Some(ScanWatch {
                                kind,
                                requested,
                                began: Instant::now(),
                                command_returned: true,
                                returned_at: Some(Instant::now()),
                            });
                        }
                    }
                    util::log(&result.message);
                    self.security.result = Some(result.message.clone());
                    self.toast(ToastKind::Info, result.message);
                }
                Err(e) => {
                    self.security.scan = None;
                    let e = clean_text(&e);
                    self.security.result = Some(e.clone());
                    self.toast(ToastKind::Error, e);
                }
            }
            self.refresh_security(ctx);
        }
        let finished = self
            .security
            .scan
            .as_ref()
            .zip(self.security.snapshot.as_ref())
            .is_some_and(|(watch, snapshot)| watch.command_returned && watch.completed(snapshot));
        if finished {
            let watch = self.security.scan.take().unwrap();
            let message = format!("Defender reports that {} finished. Review Protection history for findings and actions.", watch.kind.label().to_lowercase());
            util::log(&message);
            self.security.result = Some(message.clone());
            self.toast(ToastKind::Success, message);
        } else if self
            .security
            .scan
            .as_ref()
            .is_some_and(|watch| watch.expired(Instant::now()))
        {
            self.stop_security_monitoring();
        }
        let interval = if self.security.busy() { 5 } else { 60 };
        if (self.page == Page::Security || self.security.busy())
            && self
                .security
                .refreshed
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(interval))
        {
            self.refresh_security(ctx);
        }
    }

    fn stop_security_monitoring(&mut self) {
        if self.security.operation.running() {
            return;
        }
        self.security.scan = None;
        let message = "Scan monitoring ended without a verified completion. Defender may still be scanning, or the scan may have been cancelled. Review Windows Security for its current status.";
        self.security.result = Some(message.into());
        util::log(message);
        self.toast(ToastKind::Info, message);
    }

    pub fn execute_security(&mut self, action: SecurityAction, ctx: &egui::Context) {
        if matches!(action, SecurityAction::OpenOfflineScan) {
            util::open_uri("windowsdefender://threat");
            return;
        }
        if self.security.busy() {
            self.toast(ToastKind::Info, "A security operation is already running.");
            return;
        }
        if !self.is_admin {
            self.toast(
                ToastKind::Error,
                "Restart PCSweep as administrator to change Defender settings or run a scan.",
            );
            return;
        }
        if !self
            .security
            .snapshot
            .as_ref()
            .is_some_and(Snapshot::primary)
        {
            self.toast(ToastKind::Error, "Defender is not confirmed as the active antivirus. Refresh status or open Windows Security.");
            return;
        }
        let kind = match action {
            SecurityAction::QuickScan => Some(ScanKind::Quick),
            SecurityAction::FullScan => Some(ScanKind::Full),
            _ => None,
        };
        if let Some(kind) = kind {
            self.security.scan = Some(ScanWatch {
                kind,
                requested: Utc::now(),
                began: Instant::now(),
                command_returned: false,
                returned_at: None,
            });
        }
        self.security.operation_label = Some(match action {
            SecurityAction::QuickScan | SecurityAction::FullScan => {
                "Updating signatures, then scanning with Defender…"
            }
            SecurityAction::UpdateSignatures => "Updating Defender signatures…",
            SecurityAction::EnablePua | SecurityAction::UndoPua => {
                "Applying and verifying the PUA setting…"
            }
            SecurityAction::OpenOfflineScan => unreachable!(),
        });
        self.security.result = None;
        self.security.operation = Task::spawn(ctx, move || {
            if let Some(kind) = kind {
                crate::platform::powershell("$ErrorActionPreference='Stop'; Update-MpSignature -ErrorAction Stop; 'Signatures updated.'", Duration::from_secs(600))?;
                let requested = Utc::now();
                let script = format!("$ErrorActionPreference='Stop'; Start-MpScan -ScanType {} -ErrorAction Stop; 'Defender accepted the scan command.'", kind.argument());
                crate::platform::powershell(&script, Duration::from_secs(24 * 60 * 60))?;
                Ok(OperationResult { message: format!("{} command returned. Checking Defender's scan timestamps; this does not establish a clean result.", kind.label()), scan: Some((kind, requested)) })
            } else {
                let message = match action {
                    SecurityAction::EnablePua => pua_change(false)?,
                    SecurityAction::UndoPua => pua_change(true)?,
                    SecurityAction::UpdateSignatures => {
                        crate::platform::powershell("$ErrorActionPreference='Stop'; Update-MpSignature -ErrorAction Stop; 'Signatures updated.'", Duration::from_secs(600))?;
                        "Defender signature update finished. Refreshing the reported signature date.".into()
                    }
                    _ => unreachable!(),
                };
                Ok(OperationResult {
                    message,
                    scan: None,
                })
            }
        });
    }

    fn confirm_security(&mut self, action: SecurityAction) {
        let (title, body, note, confirm) = match action {
            SecurityAction::EnablePua => ("Block potentially unwanted apps?", "Defender will block potentially unwanted apps, including some bundled installers. PCSweep saves the current setting so you can undo this change.", "Managed or tamper-protected settings are respected. PCSweep does not add exclusions or weaken other protection.", "Enable blocking"),
            SecurityAction::UndoPua => ("Restore your previous PUA setting?", "Restore the exact setting PCSweep saved before enabling potentially unwanted app blocking.", "If another program changed the setting since then, PCSweep keeps the current setting and asks you to review Windows Security.", "Restore setting"),
            SecurityAction::UpdateSignatures => ("Update Defender signatures?", "Ask Microsoft Defender to download current security intelligence from its configured update source.", "Requires an internet connection or your organization's configured update server.", "Update signatures"),
            SecurityAction::QuickScan => ("Run a Defender quick scan?", "Update security intelligence, then scan common malware locations using Microsoft Defender.", "Defender applies your existing threat-action settings and may quarantine detections. Review Protection history afterward.", "Update and scan"),
            SecurityAction::FullScan => ("Run a Defender full scan?", "Update security intelligence, then scan files and running programs using Microsoft Defender. This can take hours and use CPU and disk bandwidth.", "Defender applies your existing threat-action settings and may quarantine detections. Keep your PC powered on and review Protection history afterward.", "Update and scan"),
            SecurityAction::OpenOfflineScan => ("Open Defender offline scan options?", "Open Virus & threat protection in Windows Security. Choose Scan options, then Microsoft Defender Offline scan if you want Windows to scan before normal startup.", "An offline scan restarts your PC. Save your work first. PCSweep only opens Windows Security; Windows asks for its own confirmation before restarting.", "Open scan options"),
        };
        self.modal = Some(Modal {
            title: title.into(),
            body: body.into(),
            details: Vec::new(),
            note: Some(note.into()),
            confirm: confirm.into(),
            danger: false,
            action: Action::Security(action),
        });
    }

    pub fn page_security(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut chosen = None;
        let mut refresh = false;
        let mut admin = false;
        let mut stop_monitoring = false;
        widgets::page_header(
            ui,
            "Security check",
            "Real protection status from Windows. Updates and scans run in the background.",
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.security.read.running(),
                    widgets::small("Refresh status"),
                )
                .clicked()
            {
                refresh = true;
            }
            if ui.add(widgets::small("Windows Security")).clicked() {
                util::open_uri("windowsdefender:");
            }
            if ui.add(widgets::small("Protection history")).clicked() {
                util::open_uri("windowsdefender://history");
            }
            if self.security.read.running() {
                ui.spinner();
                ui.label(widgets::dim("Reading Windows security status…"));
            }
        });
        if let Some(error) = &self.security.read_error {
            ui.colored_label(WARN, error);
        }
        if let Some(message) = &self.security.result {
            ui.label(widgets::dim(message));
        }
        let busy = self.security.busy();
        ScrollArea::vertical().id_salt("security_scroll").show(ui, |ui| {
            let Some(s) = &self.security.snapshot else {
                ui.label(widgets::dim("Defender status is being checked. You can open Windows Security at any time.")); return;
            };
            widgets::card(ui, |ui| {
                let primary = s.primary();
                ui.label(widgets::strong(if primary { "Microsoft Defender is active" } else if s.available { "Defender is not confirmed as your active antivirus" } else { "Defender status unavailable" }, 19.0));
                if let Some(mode) = &s.mode { ui.label(widgets::dim(format!("Reported mode: {}", clean_text(mode)))); }
                if !s.products.is_empty() { ui.label(widgets::dim(format!("Registered antivirus: {}", s.products.iter().map(|s| clean_text(s)).collect::<Vec<_>>().join(", ")))); }
                if let Some(error) = &s.error { ui.colored_label(WARN, clean_text(error)); }
                ui.horizontal_wrapped(|ui| {
                    widgets::badge(ui, &format!("Real-time protection: {}", yes_no(s.realtime)), if s.realtime == Some(true) { GOOD } else { WARN });
                    widgets::badge(ui, &format!("Tamper protection: {}", yes_no(s.tamper)), TEXT_DIM);
                });
                if !primary { ui.label(widgets::dim("Passive mode, another antivirus provider, or unavailable Defender services can limit these actions. Manage your active provider in Windows Security.")); }
                if s.reboot_required == Some(true) { ui.colored_label(WARN, "Windows reports that a restart is required to finish security changes."); }
            });
            widgets::section(ui, "Security intelligence and scans");
            let age = parse_time(s.signature_time.as_deref()).map(|t| Utc::now().signed_duration_since(t).num_hours().max(0));
            let stale = s.signatures_outdated == Some(true) || age.is_some_and(|h| h > 48);
            ui.label(RichText::new(format!("Signatures: {}{}", show_time(s.signature_time.as_deref()), age.map(|h| format!(" · {h} hours old")).unwrap_or_default())).color(if stale { WARN } else { TEXT }));
            if let Some(version) = &s.signature_version { ui.label(widgets::faint(format!("Reported version: {}", clean_text(version)))); }
            if stale { ui.label(widgets::dim("Security intelligence may be out of date. Update it before scanning.")); }
            ui.label(widgets::dim(format!("Last quick scan finished: {}", show_time(s.quick_end.as_deref()))));
            ui.label(widgets::dim(format!("Last full scan finished: {}", show_time(s.full_end.as_deref()))));
            if let Some(label) = self.security.operation_label { ui.horizontal(|ui| { ui.spinner(); ui.label(label); }); }
            if let Some(watch) = &self.security.scan {
                let status = if watch.completed(s) { "Defender reports a completed scan; collecting the command result" } else if watch.started(s) { "Defender reports scan started; waiting for a matching completion time" } else if !watch.command_returned { "Updating signatures or waiting for Defender to report scan start" } else { "Command accepted; waiting for Defender to report a new scan" };
                ui.label(widgets::dim(format!("{} · {}s · {}", watch.kind.label(), watch.began.elapsed().as_secs(), status)));
                ui.label(widgets::faint("Windows does not expose a reliable percentage here. No clean result is inferred from a successful command."));
                if watch.command_returned && ui.add(widgets::small("Stop monitoring this scan")).clicked() { stop_monitoring = true; }
            }
            let available = !busy && self.is_admin && s.primary();
            if !self.is_admin && ui.add(widgets::small(widgets::icon_text(ic::SHIELD, "Restart as admin for security actions", 13.0, WARN))).clicked() { admin = true; }
            ui.horizontal_wrapped(|ui| {
                for (label, action) in [("Update signatures", SecurityAction::UpdateSignatures), ("Quick scan", SecurityAction::QuickScan), ("Full scan", SecurityAction::FullScan)] {
                    if ui.add_enabled(available, widgets::small(label)).clicked() { chosen = Some(action); }
                }
                if ui.add(widgets::small("Offline scan options")).clicked() { chosen = Some(SecurityAction::OpenOfflineScan); }
            });
            widgets::section(ui, "Potentially unwanted app protection");
            let pua = match s.pua { Some(0) => "Off", Some(1) => "Blocking", Some(2) => "Audit only", _ => "Not reported" };
            ui.label(widgets::dim(format!("Defender PUA setting: {pua}")));
            ui.label(widgets::faint("Blocks unwanted bundled software using Defender's own policy. No custom blocklists, downloads, or antivirus exclusions."));
            let managed = s.policy_managed;
            if managed { ui.colored_label(WARN, "PUA changes are managed by policy. Use Windows Security to review the setting."); }
            else if s.tamper == Some(true) { ui.label(widgets::faint("Tamper protection is on. Windows may refuse a PUA change; PCSweep verifies the setting and never bypasses that refusal.")); }
            if let Some(error) = &s.backup_error { ui.colored_label(WARN, error); }
            ui.horizontal(|ui| {
                if ui.add_enabled(available && !managed && matches!(s.pua, Some(0 | 2)) && s.backup.is_none() && s.backup_error.is_none(), widgets::small("Enable blocking")).clicked() { chosen = Some(SecurityAction::EnablePua); }
                if ui.add_enabled(available && !managed && s.backup.is_some(), widgets::small("Undo PCSweep's PUA change")).clicked() { chosen = Some(SecurityAction::UndoPua); }
            });
            widgets::section(ui, "Recent Defender detections");
            ui.label(widgets::faint("Up to 20 records from Defender's history. These may be old or already resolved; Protection history has the current details."));
            if let Some(error) = &s.detection_error { ui.colored_label(WARN, format!("Detection history unavailable: {}", clean_text(error))); }
            else if s.detections.is_empty() { ui.label(widgets::dim("Defender returned no recent detection records. This does not establish that the PC is threat-free.")); }
            for detection in &s.detections {
                widgets::card(ui, |ui| {
                    ui.label(widgets::strong(clean_text(&detection.name), 14.0));
                    ui.label(widgets::dim(format!("{} · {}", show_time(detection.when.as_deref()), detection_status(detection))));
                    for resource in &detection.resources { ui.label(widgets::faint(clean_text(resource))); }
                });
            }
        });
        if refresh {
            self.refresh_security(&ctx);
        }
        if stop_monitoring {
            self.stop_security_monitoring();
        }
        if admin {
            self.relaunch_admin(&ctx);
        }
        if let Some(action) = chosen {
            self.confirm_security(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_security_status_probe_is_read_only_and_reports_unavailable_provider() {
        // The Windows CI runner may not have an active Defender provider. Both
        // a real status and an explicit unavailable-provider result are valid.
        // This invokes only status/history queries, never scans or settings writes.
        let snapshot =
            query().expect("the read-only Windows security probe should return valid JSON");
        if !snapshot.available {
            assert!(snapshot
                .error
                .as_deref()
                .is_some_and(|e| !e.trim().is_empty()));
        }
    }

    #[test]
    fn passive_and_disabled_defender_are_not_primary() {
        let mut snapshot = Snapshot {
            available: true,
            mode: Some("Normal".into()),
            service: Some(true),
            antivirus: Some(true),
            ..Snapshot::default()
        };
        assert!(snapshot.primary());
        snapshot.mode = Some("Passive Mode".into());
        assert!(!snapshot.primary());
        snapshot.mode = Some("Normal".into());
        snapshot.antivirus = Some(false);
        assert!(!snapshot.primary());
    }
    #[test]
    fn old_scan_completion_cannot_finish_new_request() {
        let requested = parse_time(Some("2026-11-15T12:00:00Z")).unwrap();
        let watch = ScanWatch {
            kind: ScanKind::Quick,
            requested,
            began: Instant::now(),
            command_returned: true,
            returned_at: Some(Instant::now()),
        };
        let mut snapshot = Snapshot {
            quick_start: Some("2026-11-14T12:00:00Z".into()),
            quick_end: Some("2026-11-14T12:05:00Z".into()),
            ..Snapshot::default()
        };
        assert!(!watch.completed(&snapshot));
        snapshot.quick_start = Some("2026-11-15T12:00:01Z".into());
        assert!(!watch.completed(&snapshot));
        snapshot.quick_end = Some("2026-11-15T12:04:00Z".into());
        assert!(watch.completed(&snapshot));
        snapshot.quick_end = None;
        assert!(!watch.completed(&snapshot));
    }
    #[test]
    fn missing_or_cancelled_scan_completion_cannot_block_operations_forever() {
        let now = Instant::now();
        let mut watch = ScanWatch {
            kind: ScanKind::Full,
            requested: Utc::now(),
            began: now,
            command_returned: false,
            returned_at: None,
        };
        assert!(!watch.expired(now + Duration::from_secs(3600)));
        watch.command_returned = true;
        watch.returned_at = Some(now);
        assert!(!watch.completed(&Snapshot::default()));
        assert!(!watch.expired(now + Duration::from_secs(119)));
        assert!(watch.expired(now + Duration::from_secs(120)));
    }

    #[test]
    fn detection_mapping_never_claims_unreported_actions() {
        let mut value = Detection::default();
        assert_eq!(
            detection_status(&value),
            "See Protection history for current status"
        );
        value.status_id = 3;
        assert_eq!(detection_status(&value), "Quarantined");
        value.action_success = Some(false);
        assert_eq!(
            detection_status(&value),
            "Action needs review in Protection history"
        );
        assert_eq!(clean_text("file:\r\nC:\\sample\u{1b}"), "file:C:\\sample");
    }
    #[test]
    fn default_or_incomplete_status_does_not_mean_protection_is_on() {
        let snapshot: Snapshot =
            serde_json::from_str(r#"{"available":true,"realtime":null}"#).unwrap();
        assert!(!snapshot.primary());
        assert_eq!(yes_no(snapshot.realtime), "Not reported");
    }
}

/// Deterministic sample inventory for headless UI previews; never queries Windows.
#[cfg(test)]
pub(crate) fn preview_fixture() -> SecurityState {
    SecurityState {
        snapshot: Some(Snapshot {
            available: true,
            mode: Some("Normal".into()),
            service: Some(true),
            antivirus: Some(true),
            realtime: Some(true),
            tamper: Some(true),
            signatures_outdated: Some(false),
            signature_time: Some((Utc::now() - chrono::Duration::hours(7)).to_rfc3339()),
            signature_version: Some("1.425.120.0".into()),
            quick_start: Some("2026-11-14T09:00:00Z".into()),
            quick_end: Some("2026-11-14T09:04:00Z".into()),
            full_start: Some("2026-11-10T08:00:00Z".into()),
            full_end: Some("2026-11-10T09:38:00Z".into()),
            pua: Some(0),
            products: vec!["Microsoft Defender Antivirus".into()],
            detections: vec![Detection {
                name: "PUA:Win32/ExampleInstaller (sample history)".into(),
                when: Some("2026-11-12T12:15:00Z".into()),
                action_success: Some(true),
                status_id: 3,
                resources: vec![r"file:C:\Users\Example\Desktop\sample-installer.exe".into()],
            }],
            ..Snapshot::default()
        }),
        ..SecurityState::default()
    }
}
