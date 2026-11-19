//! Windows-reported disk health and media-aware maintenance. Inventory and
//! maintenance run on workers; a capacity bar is never presented as a SMART test.
use crate::{
    app::{Action, App, Modal, Page},
    platform,
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, Label, ScrollArea, Ui};
use serde::Deserialize;
use std::time::Duration;

const INVENTORY_SCRIPT: &str = r#"
$WarningPreference='SilentlyContinue'
$disks=@{}; $physical=@(); $diskError=$null
try {Get-Disk -ErrorAction Stop | ForEach-Object {$disks[[string]$_.Number]=$_}} catch {$diskError=$_.Exception.Message}
try {$physical=@(Get-PhysicalDisk -ErrorAction Stop)} catch {}
$rows=@(Get-Volume -ErrorAction Stop | Where-Object {$null -ne $_.DriveLetter -and [string]$_.DriveLetter -ne ''} | ForEach-Object {
    $v=$_; $d=$null; $p=$null; $c=$null; $mapping=$diskError
    try {
        $part=@(Get-Partition -DriveLetter $v.DriveLetter -ErrorAction Stop)
        $ids=@($part | Select-Object -ExpandProperty DiskNumber -Unique)
        if($ids.Count -eq 1){$d=$disks[[string]$ids[0]]}
        if($null -eq $d){$mapping='Windows did not resolve this volume to one disk. Pooled or virtual storage may need its own tools.'}
    } catch {$mapping=$_.Exception.Message}
    if($null -ne $d -and -not [string]::IsNullOrWhiteSpace([string]$d.UniqueId)){
        $matches=@($physical | Where-Object {-not [string]::IsNullOrWhiteSpace([string]$_.UniqueId) -and ([string]$_.UniqueId).Trim() -eq ([string]$d.UniqueId).Trim()})
        if($matches.Count -eq 1){$p=$matches[0];try {$c=$p | Get-StorageReliabilityCounter -ErrorAction Stop}catch{}}
    }
    $temperature=$null;$wear=$null;$hours=$null;$readErrors=$null;$writeErrors=$null
    if($null -ne $c){
        # Some drivers return zero-filled unsupported telemetry. Do not turn it
        # into a 100%-healthy badge or an invented lifetime estimate.
        $hasTelemetry=($c.Temperature -gt 0 -or $c.PowerOnHours -gt 0 -or $c.Wear -gt 0 -or $c.ReadErrorsUncorrected -gt 0 -or $c.WriteErrorsUncorrected -gt 0)
        if($hasTelemetry){
            if($c.Temperature -gt 0){$temperature=[double]$c.Temperature}
            if($null -ne $c.Wear){$wear=[uint64]$c.Wear}
            if($null -ne $c.PowerOnHours){$hours=[uint64]$c.PowerOnHours}
            if($null -ne $c.ReadErrorsUncorrected){$readErrors=[uint64]$c.ReadErrorsUncorrected}
            if($null -ne $c.WriteErrorsUncorrected){$writeErrors=[uint64]$c.WriteErrorsUncorrected}
        }
    }
    [pscustomobject]@{
        letter=[string]$v.DriveLetter; unique_id=[string]$v.UniqueId; label=[string]$v.FileSystemLabel;
        filesystem=[string]$v.FileSystemType; drive_type=[string]$v.DriveType; capacity=[uint64]$v.Size; free=[uint64]$v.SizeRemaining;
        volume_health=[string]$v.HealthStatus; disk_id=if($null -ne $d){[string]$d.UniqueId}else{''};
        disk_name=if($null -ne $d){[string]$d.FriendlyName}else{''}; bus=if($null -ne $d){[string]$d.BusType}else{''};
        media=if($null -ne $p){[string]$p.MediaType}else{'Not reported'};
        disk_health=if($null -ne $d){[string]$d.HealthStatus}else{'Unknown'};
        offline=if($null -ne $d){[bool]$d.IsOffline}else{$null}; readonly=if($null -ne $d){[bool]$d.IsReadOnly}else{$null};
        mapping_warning=$mapping; temperature=$temperature; wear=$wear; hours=$hours; read_errors=$readErrors; write_errors=$writeErrors
    }
})
$schedule=[ordered]@{enabled=$null;last_run=$null;next_run=$null;result=$null;warning=$null}
try {
    $task=Get-ScheduledTask -TaskPath '\Microsoft\Windows\Defrag\' -TaskName 'ScheduledDefrag' -ErrorAction Stop
    $info=$task | Get-ScheduledTaskInfo -ErrorAction Stop
    $schedule.enabled=([string]$task.State -ne 'Disabled' -and [bool]$task.Settings.Enabled)
    if($info.LastRunTime.Year -gt 2000){$schedule.last_run=$info.LastRunTime.ToString('o')}
    if($info.NextRunTime.Year -gt 2000){$schedule.next_run=$info.NextRunTime.ToString('o')}
    $schedule.result=[uint32]$info.LastTaskResult
} catch {$schedule.warning=$_.Exception.Message}
[pscustomobject]@{volumes=$rows;schedule=[pscustomobject]$schedule;observed=(Get-Date).ToString('o')} | ConvertTo-Json -Depth 6 -Compress
"#;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VolumeInfo {
    letter: String,
    unique_id: String,
    label: String,
    filesystem: String,
    drive_type: String,
    capacity: u64,
    free: u64,
    volume_health: String,
    disk_id: String,
    disk_name: String,
    bus: String,
    media: String,
    disk_health: String,
    offline: Option<bool>,
    readonly: Option<bool>,
    mapping_warning: Option<String>,
    temperature: Option<f64>,
    wear: Option<u64>,
    hours: Option<u64>,
    read_errors: Option<u64>,
    write_errors: Option<u64>,
}
impl VolumeInfo {
    fn normalize_telemetry(&mut self) {
        self.temperature = self.temperature.filter(|v| v.is_finite() && *v > 0.0);
        if self.temperature.is_none()
            && self.wear.unwrap_or(0) == 0
            && self.hours.unwrap_or(0) == 0
            && self.read_errors.unwrap_or(0) == 0
            && self.write_errors.unwrap_or(0) == 0
        {
            self.wear = None;
            self.hours = None;
            self.read_errors = None;
            self.write_errors = None;
        }
    }
    fn validate(&self) -> Result<(), String> {
        if self.letter.len() != 1
            || !self.letter.bytes().all(|v| v.is_ascii_alphabetic())
            || self.unique_id.is_empty()
            || self.unique_id.len() > 512
        {
            return Err("Windows did not provide a complete volume identity.".into());
        }
        Ok(())
    }
    fn warning(&self) -> bool {
        [self.disk_health.as_str(), self.volume_health.as_str()]
            .iter()
            .any(|v| {
                ["Warning", "Unhealthy", "Degraded"]
                    .iter()
                    .any(|bad| v.eq_ignore_ascii_case(bad))
            })
            || self.read_errors.is_some_and(|n| n > 0)
            || self.write_errors.is_some_and(|n| n > 0)
            || self.wear.is_some_and(|n| n >= 100)
    }
    fn maintenance_available(&self) -> bool {
        self.validate().is_ok()
            && !self.disk_id.is_empty()
            && self.mapping_warning.is_none()
            && self.offline == Some(false)
            && self.readonly == Some(false)
            && self.drive_type.eq_ignore_ascii_case("Fixed")
            && ["NTFS", "ReFS"]
                .iter()
                .any(|v| self.filesystem.eq_ignore_ascii_case(v))
            && !self.warning()
            && !["RAID", "Spaces", "Virtual", "File Backed Virtual"]
                .iter()
                .any(|v| self.bus.eq_ignore_ascii_case(v))
    }
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Schedule {
    enabled: Option<bool>,
    last_run: Option<String>,
    next_run: Option<String>,
    result: Option<u32>,
    warning: Option<String>,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Snapshot {
    volumes: Vec<VolumeInfo>,
    schedule: Schedule,
    observed: String,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TrimPolicy {
    ntfs: Option<bool>,
    refs: Option<bool>,
}
fn parse_trim_policy(text: &str) -> TrimPolicy {
    let mut result = TrimPolicy::default();
    for line in text.lines() {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let enabled = match value.split_whitespace().next() {
            Some("0") => Some(true),
            Some("1") => Some(false),
            _ => None,
        };
        if name.eq_ignore_ascii_case("NTFS DisableDeleteNotify") {
            result.ntfs = enabled;
        }
        if name.eq_ignore_ascii_case("ReFS DisableDeleteNotify") {
            result.refs = enabled;
        }
    }
    result
}
fn trim_policy() -> TrimPolicy {
    // This is the file-system policy only, not a device capability test or proof
    // that an Optimize-Volume retrim actually completed.
    let query = (|| {
        let exe = platform::system_directory()?.join("fsutil.exe");
        let script = format!("& {} behavior query DisableDeleteNotify; if($LASTEXITCODE -ne 0){{throw 'Windows did not report the TRIM policy'}}", platform::ps_quote(exe.to_str().ok_or("Windows tool path cannot be encoded")?));
        platform::powershell(&script, Duration::from_secs(10))
    })();
    query
        .map(|text| parse_trim_policy(&text))
        .unwrap_or_default()
}
fn query() -> Result<(Snapshot, TrimPolicy), String> {
    let text = platform::powershell(INVENTORY_SCRIPT, Duration::from_secs(45))?;
    Ok((parse_snapshot(&text)?, trim_policy()))
}
fn parse_snapshot(text: &str) -> Result<Snapshot, String> {
    let mut snapshot: Snapshot = serde_json::from_str(text)
        .map_err(|e| format!("Windows returned unreadable storage details: {e}"))?;
    for volume in &mut snapshot.volumes {
        if volume.letter.len() != 1 || !volume.letter.bytes().all(|v| v.is_ascii_alphabetic()) {
            return Err("Windows returned an unrecognized drive letter.".into());
        }
        // An empty card reader, optical drive or unusual provider must not hide
        // every other disk. Keep it visible but unavailable for maintenance.
        volume.normalize_telemetry();
    }
    snapshot
        .volumes
        .sort_by_key(|v| v.letter.to_ascii_uppercase());
    Ok(snapshot)
}

#[derive(Clone)]
pub enum StorageAction {
    Maintenance { volume: VolumeInfo, analyze: bool },
}
fn maintenance_script(volume: &VolumeInfo, analyze: bool) -> Result<String, String> {
    volume.validate()?;
    if !volume.maintenance_available() {
        return Err("This volume needs native or vendor review before maintenance. Its configuration was kept.".into());
    }
    let operation = if analyze { "-Analyze" } else { "" };
    Ok(format!(
        r#"
$letter={letter};$identity={identity};$diskId={disk_id}
$v=Get-Volume -DriveLetter $letter -ErrorAction Stop
if(-not [string]::Equals([string]$v.UniqueId,$identity,[StringComparison]::OrdinalIgnoreCase)){{throw 'The volume identity changed after review. No other volume will be optimized.'}}
$parts=@(Get-Partition -DriveLetter $letter -ErrorAction Stop)
$ids=@($parts | Select-Object -ExpandProperty DiskNumber -Unique)
if($ids.Count -ne 1){{throw 'The disk mapping changed. Use Windows storage tools.'}}
$d=Get-Disk -Number $ids[0] -ErrorAction Stop
if(-not [string]::Equals([string]$d.UniqueId,$diskId,[StringComparison]::OrdinalIgnoreCase)){{throw 'The physical disk changed after review.'}}
if($d.IsOffline -or $d.IsReadOnly -or [string]$v.DriveType -ne 'Fixed' -or [string]$v.HealthStatus -ne 'Healthy' -or [string]$d.HealthStatus -ne 'Healthy'){{throw 'Windows reports an unavailable, protected or unhealthy volume. Review disk health and back up important files first.'}}
if([string]$d.BusType -in @('RAID','Spaces','Virtual','File Backed Virtual')){{throw 'This storage configuration needs its native management tools.'}}
if([string]$v.FileSystemType -notin @('NTFS','ReFS')){{throw 'This file system is not eligible for this action.'}}
Optimize-Volume -DriveLetter $letter {operation} -ErrorAction Stop -Verbose 4>&1 | Out-String -Width 160
"#,
        letter = platform::ps_quote(&volume.letter),
        identity = platform::ps_quote(&volume.unique_id),
        disk_id = platform::ps_quote(&volume.disk_id),
    ))
}
struct MaintenanceLock(isize);
impl MaintenanceLock {
    fn take() -> Result<Self, String> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{CreateMutexW, WaitForSingleObject},
        };
        let name: Vec<u16> = "Global\\PCSweep.StorageMaintenance.v1"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle == 0 {
            return Err("Storage maintenance is unavailable or already in use.".into());
        }
        let result = unsafe { WaitForSingleObject(handle, 0) };
        if result == 0 || result == 0x80 {
            Ok(Self(handle))
        } else {
            unsafe { CloseHandle(handle) };
            Err("Another PCSweep storage maintenance operation is running.".into())
        }
    }
}
impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
fn run(action: StorageAction) -> Result<String, String> {
    if !crate::system::is_admin() {
        return Err("Restart as administrator to run disk analysis or optimization.".into());
    }
    let _lock = MaintenanceLock::take()?;
    match action {
        StorageAction::Maintenance { volume, analyze } => {
            let (current, _) = query()?;
            let fresh = current
                .volumes
                .into_iter()
                .find(|v| {
                    v.letter.eq_ignore_ascii_case(&volume.letter)
                        && v.unique_id.eq_ignore_ascii_case(&volume.unique_id)
                        && v.disk_id.eq_ignore_ascii_case(&volume.disk_id)
                })
                .ok_or("The reviewed volume or disk changed. Refresh before maintenance.")?;
            let script = maintenance_script(&fresh, analyze)?;
            let output = platform::powershell(&script, Duration::from_secs(30 * 60))?;
            let operation = if analyze {
                "analysis"
            } else {
                "media-aware optimization"
            };
            Ok(format!(
                "Windows completed the {operation} request for {}:.\n{}",
                volume.letter,
                output.trim()
            ))
        }
    }
}

pub struct StorageState {
    load: Task<Result<(Snapshot, TrimPolicy), String>>,
    change: Task<Result<String, String>>,
    snapshot: Option<Snapshot>,
    trim: TrimPolicy,
    loaded: bool,
    error: Option<String>,
    message: Option<String>,
}
impl Default for StorageState {
    fn default() -> Self {
        Self {
            load: Task::default(),
            change: Task::default(),
            snapshot: None,
            trim: TrimPolicy::default(),
            loaded: false,
            error: None,
            message: None,
        }
    }
}
impl StorageState {
    pub fn busy(&self) -> bool {
        self.change.running()
    }
    pub fn overview_summary(&self) -> String {
        if self.load.running() {
            return "Reading Windows storage reports…".into();
        }
        let Some(snapshot) = &self.snapshot else {
            return if self.error.is_some() {
                "Storage status unavailable — open Disk health for details.".into()
            } else {
                "Storage health not checked yet.".into()
            };
        };
        let warnings = snapshot.volumes.iter().filter(|v| v.warning()).count();
        if warnings > 0 {
            format!("{warnings} volume(s) need storage-health review. Back up important data before intensive maintenance.")
        } else {
            format!("{} mounted volume(s) reviewed. Open Disk health for reported telemetry and optimization status.", snapshot.volumes.len())
        }
    }
    pub fn has_warning(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|s| s.volumes.iter().any(VolumeInfo::warning))
    }
}
impl App {
    fn refresh_storage(&mut self, ctx: &egui::Context) {
        if !self.storage.load.running() && !self.storage.change.running() {
            self.storage.load = Task::spawn(ctx, query);
        }
    }
    pub fn poll_storage(&mut self, ctx: &egui::Context) {
        if !self.storage.loaded
            && !self.storage.load.running()
            && matches!(self.page, Page::Dashboard | Page::Storage)
        {
            self.refresh_storage(ctx);
        }
        if let Some(result) = self.storage.load.poll() {
            self.storage.loaded = true;
            match result.and_then(|r| r) {
                Ok((snapshot, trim)) => {
                    self.storage.snapshot = Some(snapshot);
                    self.storage.trim = trim;
                }
                Err(error) => self.storage.error = Some(error),
            }
        }
        if let Some(result) = self.storage.change.poll() {
            match result.and_then(|r| r) {
                Ok(message) => {
                    util::log(&message);
                    self.storage.message = Some(message);
                }
                Err(error) => {
                    util::log(format!("Storage maintenance: {error}"));
                    self.storage.error = Some(error);
                }
            }
            self.refresh_storage(ctx);
        }
    }
    pub fn execute_storage(&mut self, action: StorageAction, ctx: &egui::Context) {
        if self.storage.busy() {
            return;
        }
        self.storage.error = None;
        self.storage.message = None;
        self.storage.change = Task::spawn(ctx, move || run(action));
    }
    pub fn page_storage(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let busy = self.mutations_running();
        let mut refresh = false;
        let mut action = None;
        ScrollArea::vertical().id_salt("storage_page").auto_shrink([false, false]).show(ui, |ui| {
            widgets::page_header(ui, "Disk health and maintenance", "Review Windows-reported health and maintenance separately from free space. Unsupported telemetry stays unknown.");
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!self.storage.load.running() && !self.storage.busy(), widgets::small("Refresh storage reports")).clicked() { refresh = true; }
                if ui.add(widgets::small("Windows Optimize Drives")).clicked() { util::launch("dfrgui.exe"); }
                if ui.add(widgets::small("Windows Storage settings")).clicked() { util::open_uri("ms-settings:storagesense"); }
                if self.storage.load.running() { ui.spinner(); ui.label(widgets::dim("Reading disk reports in the background…")); }
            });
            if let Some(error) = &self.storage.error { ui.colored_label(color_warning(), error); }
            if self.storage.busy() { ui.horizontal_wrapped(|ui| { ui.spinner(); ui.label("Windows maintenance is running. You can navigate elsewhere or use Optimize Drives to review native progress."); }); }
            widgets::card(ui, |ui| {
                ui.label(widgets::strong("Health, capacity and optimization are different", 16.0));
                ui.label(widgets::dim("Free space shows capacity pressure. Windows status and reliability counters report available device information; neither guarantees a drive will not fail. TRIM/defrag does not repair failing hardware."));
                ui.label(widgets::faint("Windows' consumer health warnings cover NVMe SSDs. SATA, USB bridges, RAID and drivers can hide telemetry. Missing or zero-filled unsupported counters are not a clean bill of health."));
            });
            widgets::card(ui, |ui| {
                ui.label(widgets::strong("TRIM notification policy", 16.0));
                for (filesystem, value) in [("NTFS", self.storage.trim.ntfs), ("ReFS", self.storage.trim.refs)] {
                    ui.label(widgets::dim(format!("{filesystem} delete notifications: {}", match value {
                        Some(true) => "Enabled", Some(false) => "Disabled", None => "Not reported",
                    })));
                }
                ui.label(widgets::faint("This policy is separate from device/controller TRIM support and successful retrim. PCSweep does not infer last maintenance from this value or change this policy."));
            });
            if let Some(snapshot) = &self.storage.snapshot {
                ui.label(widgets::faint(format!("Windows reports read at {}", snapshot.observed)));
                widgets::section(ui, "Scheduled optimization");
                widgets::card(ui, |ui| {
                    ui.label(widgets::strong(match snapshot.schedule.enabled { Some(true) => "Windows scheduled optimization is enabled", Some(false) => "Windows scheduled optimization is disabled", None => "Windows optimization schedule is not reported" }, 16.0));
                    if let Some(last) = &snapshot.schedule.last_run { ui.label(widgets::dim(format!("Last task run: {last}"))); }
                    if let Some(next) = &snapshot.schedule.next_run { ui.label(widgets::dim(format!("Next task trigger: {next}"))); }
                    if let Some(result) = snapshot.schedule.result { ui.label(widgets::faint(format!("Last task result code: {result}"))); }
                    if let Some(warning) = &snapshot.schedule.warning { ui.label(widgets::faint(warning)); }
                    ui.label(widgets::faint("Task history is not proof every volume was optimized. Per-volume completion and due dates are not verified here; open Optimize Drives for Windows' latest analysis. PCSweep does not invent an overdue date or replace the Windows schedule."));
                });
                widgets::section(ui, "Mounted volumes and physical-disk reports");
                if snapshot.volumes.is_empty() { ui.label(widgets::dim("Windows did not report any mounted drive-letter volumes.")); }
                for volume in &snapshot.volumes {
                    widgets::card(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(widgets::strong(format!("{}:  {}", volume.letter, volume.label), 19.0));
                            widgets::badge(ui, if volume.warning() { "Needs review" } else { "Windows report" }, if volume.warning() { color_warning() } else { color_muted() });
                        });
                        ui.label(widgets::dim(format!("{} free of {} · {} · {}", util::fmt_bytes(volume.free), util::fmt_bytes(volume.capacity), volume.filesystem, volume.drive_type)));
                        if volume.capacity > 0 {
                            let used = 1.0 - (volume.free as f64 / volume.capacity as f64).clamp(0.0, 1.0);
                            widgets::bar(ui, used as f32, if used > 0.9 { color_warning() } else { color_accent() }, egui::vec2(ui.available_width(), 7.0));
                        }
                        ui.label(widgets::dim(format!("Volume status: {} · Disk status: {}", unknown(&volume.volume_health), unknown(&volume.disk_health))));
                        ui.add(Label::new(widgets::faint(format!("{} · {} · {}", unknown(&volume.disk_name), unknown(&volume.bus), unknown(&volume.media)))).wrap());
                        if let Some(warning) = &volume.mapping_warning { ui.label(widgets::faint(warning)); }
                        let temp = volume.temperature.map(|v| format!("{v:.0} °C")).unwrap_or_else(|| "Not reported".into());
                        let wear = volume.wear.map(|v| format!("{v}% used (reported estimate)")).unwrap_or_else(|| "Not reported".into());
                        let hours = volume.hours.map(|v| format!("{v} h")).unwrap_or_else(|| "Not reported".into());
                        ui.horizontal_wrapped(|ui| { ui.label(widgets::dim(format!("Temperature: {temp}"))); ui.label(widgets::dim(format!("Wear: {wear}"))); ui.label(widgets::dim(format!("Power-on: {hours}"))); });
                        ui.label(widgets::faint(format!("Reported uncorrected errors — read: {} · write: {}", optional_count(volume.read_errors), optional_count(volume.write_errors))));
                        if volume.warning() { ui.colored_label(color_warning(), "Review this warning and back up important files before maintenance. Native/vendor diagnostics can provide more detail."); }
                        ui.label(widgets::faint("Last successful per-volume optimization / next due: not verified. Use Windows Optimize Drives for its current status."));
                        ui.horizontal_wrapped(|ui| {
                            for (analyze, label) in [(true, "Analyze volume…"), (false, "Optimize using Windows…")] {
                                if ui.add_enabled(self.is_admin && !busy && volume.maintenance_available(), widgets::small(label)).clicked() {
                                    action = Some(StorageAction::Maintenance { volume: volume.clone(), analyze });
                                }
                            }
                        });
                        if !self.is_admin { ui.label(widgets::faint("Restart as administrator to request analysis or optimization.")); }
                        if !volume.maintenance_available() { ui.label(widgets::faint("Use native storage tools for protected, unhealthy, pooled, virtual or unmapped volumes.")); }
                    });
                }
            }
            if let Some(message) = &self.storage.message {
                widgets::section(ui, "Last maintenance result");
                widgets::card(ui, |ui| { ui.add(Label::new(widgets::dim(message)).wrap()); });
            }
        });
        if refresh {
            self.storage.error = None;
            self.refresh_storage(&ctx);
        }
        if let Some(action) = action {
            let StorageAction::Maintenance { volume, analyze } = &action;
            self.modal = Some(Modal {
                title: format!("{} {}:?", if *analyze { "Analyze" } else { "Optimize" }, volume.letter),
                body: if *analyze { "Ask Windows to analyze this reviewed volume. This checks maintenance needs; it is different from mapping files or checking physical drive health.".into() }
                    else { "Ask Windows to choose the supported optimization for this reviewed volume and media. This can include retrim or defragmentation and may take several minutes.".into() },
                details: vec![format!("Volume: {}: {}", volume.letter, volume.label), format!("Device: {}", volume.disk_name)],
                note: Some("Maintenance may increase disk activity. It is not an Undo-able setting and does not repair failing hardware. Windows rechecks the volume identity and health before starting.".into()),
                confirm: if *analyze { "Analyze volume" } else { "Optimize volume" }.into(), danger: false,
                action: Action::Storage(action),
            });
        }
    }
}
fn unknown(value: &str) -> &str {
    if value.trim().is_empty() {
        "Not reported"
    } else {
        value
    }
}
fn optional_count(value: Option<u64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "Not reported".into())
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> StorageState {
    StorageState {
        loaded: true,
        trim: TrimPolicy {
            ntfs: Some(true),
            refs: Some(true),
        },
        snapshot: Some(Snapshot {
            volumes: vec![VolumeInfo {
                letter: "C".into(),
                unique_id: r"\\?\Volume{11111111-2222-3333-4444-555555555555}\".into(),
                label: "Windows".into(),
                filesystem: "NTFS".into(),
                drive_type: "Fixed".into(),
                capacity: 512 * 1024u64.pow(3),
                free: 120 * 1024u64.pow(3),
                volume_health: "Healthy".into(),
                disk_id: "Preview disk identity".into(),
                disk_name: "Example NVMe SSD".into(),
                bus: "NVMe".into(),
                media: "SSD".into(),
                disk_health: "Healthy".into(),
                offline: Some(false),
                readonly: Some(false),
                temperature: Some(38.0),
                wear: Some(8),
                hours: Some(2400),
                read_errors: Some(0),
                write_errors: Some(0),
                ..Default::default()
            }],
            schedule: Schedule {
                enabled: Some(true),
                ..Default::default()
            },
            observed: "Preview — local Windows report".into(),
        }),
        ..Default::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trim_notification_policy_preserves_each_filesystem_and_unknown_values() {
        let parsed = parse_trim_policy(
            "NTFS DisableDeleteNotify = 0 (Disabled)\r\nReFS DisableDeleteNotify = 1 (Enabled)",
        );
        assert_eq!(parsed.ntfs, Some(true));
        assert_eq!(parsed.refs, Some(false));
        assert_eq!(
            parse_trim_policy("Unknown localized output"),
            TrimPolicy::default()
        );
        assert_eq!(parse_trim_policy("NTFS DisableDeleteNotify = 9").ntfs, None);
    }
    #[test]
    fn unknown_data_is_not_reported_as_healthy_or_invented_due_date() {
        let snapshot: Snapshot = serde_json::from_str(r#"{"volumes":[{"letter":"C","unique_id":"volume-id","capacity":100,"free":20}],"schedule":{}}"#).unwrap();
        let volume = &snapshot.volumes[0];
        assert_eq!(volume.temperature, None);
        assert_eq!(volume.wear, None);
        assert!(!volume.maintenance_available());
        assert_eq!(snapshot.schedule.enabled, None);
        assert_eq!(snapshot.schedule.last_run, None);
        assert_eq!(optional_count(None), "Not reported");
    }
    #[test]
    fn unavailable_media_does_not_hide_other_volumes_and_zero_counters_stay_unknown() {
        let snapshot = parse_snapshot(r#"{"volumes":[{"letter":"D","unique_id":"","drive_type":"CD-ROM"},{"letter":"C","unique_id":"volume-c","wear":0,"hours":0,"temperature":0,"read_errors":0,"write_errors":0}]}"#).unwrap();
        assert_eq!(snapshot.volumes.len(), 2);
        assert_eq!(snapshot.volumes[0].letter, "C");
        assert_eq!(snapshot.volumes[0].wear, None);
        assert_eq!(snapshot.volumes[0].hours, None);
        assert_eq!(snapshot.volumes[0].read_errors, None);
        assert!(!snapshot.volumes[1].maintenance_available());
    }
    #[test]
    fn reported_wear_is_not_converted_to_an_invented_life_percentage() {
        let mut volume = preview_fixture().snapshot.unwrap().volumes.remove(0);
        volume.wear = Some(108);
        volume.normalize_telemetry();
        assert_eq!(volume.wear, Some(108));
        assert!(volume.warning());
        assert!(!volume.maintenance_available());
        volume.wear = Some(0);
        volume.temperature = Some(f64::NAN);
        volume.hours = None;
        volume.read_errors = None;
        volume.write_errors = None;
        volume.normalize_telemetry();
        assert_eq!(volume.temperature, None);
        assert_eq!(volume.wear, None);
    }
    #[test]
    fn maintenance_requires_stable_identity_and_supported_healthy_mapping() {
        let mut volume = preview_fixture().snapshot.unwrap().volumes.remove(0);
        assert!(volume.maintenance_available());
        let script = maintenance_script(&volume, false).unwrap();
        assert!(!script.contains("-Defrag"));
        assert!(!script.contains("-ReTrim"));
        assert!(script.contains("OrdinalIgnoreCase"));
        volume.disk_health = "Warning".into();
        assert!(!volume.maintenance_available());
        assert!(maintenance_script(&volume, false).is_err());
        volume.disk_health = "Healthy".into();
        volume.mapping_warning = Some("Pooled".into());
        assert!(!volume.maintenance_available());
        volume.mapping_warning = None;
        volume.offline = None;
        assert!(!volume.maintenance_available());
    }
    #[test]
    fn fixed_targets_are_quoted_and_invalid_letters_are_rejected() {
        let mut volume = preview_fixture().snapshot.unwrap().volumes.remove(0);
        volume.letter = "C'; Remove-Item x".into();
        assert!(maintenance_script(&volume, true).is_err());
        volume.letter = "C".into();
        volume.disk_id = "disk's unique id".into();
        let script = maintenance_script(&volume, true).unwrap();
        assert!(script.contains("'disk''s unique id'"));
        assert!(script.contains("-Analyze"));
    }
    #[test]
    fn native_storage_scripts_parse_without_running_maintenance() {
        let volume = preview_fixture().snapshot.unwrap().volumes.remove(0);
        for script in [
            INVENTORY_SCRIPT.to_string(),
            maintenance_script(&volume, true).unwrap(),
            maintenance_script(&volume, false).unwrap(),
        ] {
            let check = format!("$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors);if($errors.Count){{throw ($errors|Out-String)}}", platform::ps_quote(&script));
            platform::powershell(&check, Duration::from_secs(15)).unwrap();
        }
    }
    #[test]
    fn native_storage_inventory_is_read_only_and_structured() {
        let (snapshot, _) = query().unwrap();
        for volume in snapshot.volumes {
            assert_eq!(volume.letter.len(), 1);
            if volume.unique_id.is_empty() {
                assert!(!volume.maintenance_available());
            }
        }
    }
}
