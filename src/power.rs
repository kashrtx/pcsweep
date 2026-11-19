//! Supported Windows power preferences, reversible per setting. Modern APIs are
//! resolved at runtime so older Windows builds still start normally.
use crate::{
    app::{Action, App, Modal, Page},
    dns, platform,
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::core::GUID;
use winreg::{enums::*, RegKey};

const UNDO_KEY: &str = r"Software\PCSweep\PowerUndo";
const EFFICIENCY: &str = "961cc777-2547-4f9d-8174-7d86181b8a7a";
const BALANCED: &str = "00000000-0000-0000-0000-000000000000";
const PERFORMANCE: &str = "ded574b5-45a0-4f42-8737-46345c09c238";
const BALANCED_PLAN: &str = "381b4222-f694-41f0-9685-ff5bb260df2e";
const PERFORMANCE_PLAN: &str = "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c";
const SAVER_PLAN: &str = "a1841308-3541-4fab-bc81-f71556f20b4a";

#[derive(Clone, Copy, Debug, Default)]
struct Status {
    ac: Option<bool>,
    battery: Option<bool>,
    percent: Option<u8>,
    charging: bool,
    saver: Option<bool>,
    seconds: Option<u32>,
}
fn status() -> Status {
    let mut s: windows_sys::Win32::System::Power::SYSTEM_POWER_STATUS =
        unsafe { std::mem::zeroed() };
    if unsafe { windows_sys::Win32::System::Power::GetSystemPowerStatus(&mut s) } == 0 {
        return Status::default();
    }
    let battery = if s.BatteryFlag == 255 {
        None
    } else {
        Some(s.BatteryFlag & 128 == 0)
    };
    Status {
        ac: match s.ACLineStatus {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        battery,
        percent: if battery == Some(true) && s.BatteryLifePercent <= 100 {
            Some(s.BatteryLifePercent)
        } else {
            None
        },
        charging: s.BatteryFlag != 255 && s.BatteryFlag & 8 != 0,
        saver: match s.SystemStatusFlag {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        seconds: if s.BatteryLifeTime > 0 && s.BatteryLifeTime < u32::MAX {
            Some(s.BatteryLifeTime)
        } else {
            None
        },
    }
}
fn guid_text(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2..]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
type GetMode = unsafe extern "system" fn(*mut GUID) -> u32;
type SetMode = unsafe extern "system" fn(*const GUID) -> u32;
struct Modes {
    get_ac: GetMode,
    get_dc: GetMode,
    set_ac: SetMode,
    set_dc: SetMode,
}
fn modes() -> Option<Modes> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleHandleW, GetProcAddress, LoadLibraryExW,
    };
    let dll: Vec<u16> = "powrprof.dll".encode_utf16().chain(Some(0)).collect();
    let mut module = unsafe { GetModuleHandleW(dll.as_ptr()) };
    if module == 0 {
        module = unsafe { LoadLibraryExW(dll.as_ptr(), 0, 0x800) }
    }
    if module == 0 {
        return None;
    }
    let get = |name: &[u8]| {
        unsafe { GetProcAddress(module, name.as_ptr()) }.map(|p| unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, GetMode>(p)
        })
    };
    let set = |name: &[u8]| {
        unsafe { GetProcAddress(module, name.as_ptr()) }.map(|p| unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, SetMode>(p)
        })
    };
    Some(Modes {
        get_ac: get(b"PowerGetUserConfiguredACPowerMode\0")?,
        get_dc: get(b"PowerGetUserConfiguredDCPowerMode\0")?,
        set_ac: set(b"PowerSetUserConfiguredACPowerMode\0")?,
        set_dc: set(b"PowerSetUserConfiguredDCPowerMode\0")?,
    })
}
fn active_plan() -> Result<String, String> {
    let mut p = std::ptr::null_mut();
    let result = unsafe { windows_sys::Win32::System::Power::PowerGetActiveScheme(0, &mut p) };
    if result != 0 || p.is_null() {
        return Err(format!(
            "Windows did not report its active plan (code {result})."
        ));
    }
    let text = guid_text(unsafe { &*p });
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(p as *mut std::ffi::c_void);
    }
    Ok(text)
}
fn get_setting(setting: &str) -> Result<String, String> {
    if setting == "plan" {
        return active_plan();
    }
    let api=modes().ok_or("Modern power mode controls are unavailable on this Windows build. Use Power & battery settings.")?;
    let mut g: GUID = unsafe { std::mem::zeroed() };
    let result = unsafe {
        match setting {
            "ac" => (api.get_ac)(&mut g),
            "dc" => (api.get_dc)(&mut g),
            _ => return Err("Invalid power setting".into()),
        }
    };
    if result != 0 {
        return Err(format!(
            "Windows did not report this configured power mode (code {result})."
        ));
    }
    Ok(guid_text(&g))
}
fn mode_name(guid: &str) -> Option<&'static str> {
    match guid {
        EFFICIENCY => Some("Best power efficiency"),
        BALANCED => Some("Balanced"),
        PERFORMANCE => Some("Best performance"),
        _ => None,
    }
}
fn set_setting(setting: &str, value: &str) -> Result<(), String> {
    let guid = dns::parse_guid(value)?;
    let result = if setting == "plan" {
        unsafe { windows_sys::Win32::System::Power::PowerSetActiveScheme(0, &guid) }
    } else {
        if mode_name(value).is_none() {
            return Err("The power mode is not recognized. Use Windows Settings.".into());
        }
        let api = modes().ok_or("Modern power modes are unavailable")?;
        unsafe {
            match setting {
                "ac" => (api.set_ac)(&guid),
                "dc" => (api.set_dc)(&guid),
                _ => return Err("Invalid power setting".into()),
            }
        }
    };
    if result != 0 {
        return Err(format!(
            "Windows refused this power setting (code {result}). Managed/OEM settings were kept."
        ));
    }
    Ok(())
}

#[derive(Clone, Deserialize, Default)]
#[serde(default)]
struct Plan {
    guid: String,
    name: String,
}
#[derive(Clone, Deserialize, Default)]
#[serde(default)]
struct Hardware {
    plans: Vec<Plan>,
    design: Option<u64>,
    full: Option<u64>,
    cycles: Option<u32>,
    warning: Option<String>,
}
const HARDWARE_SCRIPT: &str = r#"
$result=[ordered]@{plans=@();design=$null;full=$null;cycles=$null;warning=$null}
try {$result.plans=@(Get-CimInstance -Namespace root/cimv2/power -Class Win32_PowerPlan -ErrorAction Stop|ForEach-Object { [pscustomobject]@{guid=([string]$_.InstanceID -replace '^.*\{','' -replace '\}$','').ToLowerInvariant();name=[string]$_.ElementName} })} catch {$result.warning=$_.Exception.Message}
try {
    $d=@(Get-CimInstance -Namespace root/WMI -Class BatteryStaticData -ErrorAction Stop)
    $f=@(Get-CimInstance -Namespace root/WMI -Class BatteryFullChargedCapacity -ErrorAction Stop)
    $design=[uint64]0;$full=[uint64]0;$complete=($d.Count -gt 0 -and $d.Count -eq $f.Count)
    foreach($battery in $d){
        $matched=@($f | Where-Object {$_.InstanceName -ceq $battery.InstanceName})
        if([string]::IsNullOrEmpty($battery.InstanceName) -or $matched.Count -ne 1 -or $battery.DesignedCapacity -le 0 -or $matched[0].FullChargedCapacity -le 0){$complete=$false;break}
        $design+=[uint64]$battery.DesignedCapacity;$full+=[uint64]$matched[0].FullChargedCapacity
    }
    if($complete){$result.design=$design;$result.full=$full}
}catch{}
try {$c=@(Get-CimInstance -Namespace root/WMI -Class BatteryCycleCount -ErrorAction Stop);if($c.Count -eq 1){$result.cycles=[uint32]$c[0].CycleCount}}catch{}
$result|ConvertTo-Json -Depth 4 -Compress
"#;
struct Snapshot {
    hardware: Hardware,
    ac: Result<String, String>,
    dc: Result<String, String>,
    plan: Result<String, String>,
    undo: Result<Vec<Entry>, String>,
}
fn query() -> Result<Snapshot, String> {
    let hardware = match platform::powershell(HARDWARE_SCRIPT, Duration::from_secs(30)) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("Windows returned unreadable power details: {e}"))?,
        Err(e) => Hardware {
            warning: Some(e),
            ..Default::default()
        },
    };
    Ok(Snapshot {
        hardware,
        ac: get_setting("ac"),
        dc: get_setting("dc"),
        plan: active_plan(),
        undo: read_undo(),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    setting: String,
    original: String,
    applied: String,
    // Written before a setting call, cleared after successful readback. This
    // covers a crash or an API error during a second change of the same mode.
    #[serde(default)]
    previous: Option<String>,
}
impl Entry {
    fn owns(&self, current: &str) -> bool {
        current == self.applied
            || current == self.original
            || self.previous.as_deref() == Some(current)
    }
}
fn validate_entries(entries: &[Entry]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    if entries.len() > 3 {
        return Err("Invalid power undo record".into());
    }
    for e in entries {
        if !matches!(e.setting.as_str(), "ac" | "dc" | "plan") || !ids.insert(&e.setting) {
            return Err("Invalid power undo scope".into());
        }
        dns::parse_guid(&e.original)?;
        dns::parse_guid(&e.applied)?;
        if let Some(previous) = &e.previous {
            dns::parse_guid(previous)?;
            if e.setting != "plan" && mode_name(previous).is_none() {
                return Err("Unknown pending power mode".into());
            }
        }
        if e.setting != "plan"
            && (mode_name(&e.original).is_none() || mode_name(&e.applied).is_none())
        {
            return Err("Unknown saved power mode".into());
        }
    }
    Ok(())
}
fn read_undo() -> Result<Vec<Entry>, String> {
    let k = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(UNDO_KEY) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let s: String = match k.get_value("ValuesV1") {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    if s.len() > 8192 {
        return Err("Power undo record is too large".into());
    }
    let entries: Vec<Entry> = serde_json::from_str(&s)
        .map_err(|_| "Power undo could not be read; no setting was changed")?;
    validate_entries(&entries)?;
    Ok(entries)
}
fn save_undo(entries: &[Entry]) -> Result<(), String> {
    validate_entries(entries)?;
    let (k, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey_with_flags(UNDO_KEY, KEY_READ | KEY_WRITE)
        .map_err(|e| e.to_string())?;
    k.set_value(
        "ValuesV1",
        &serde_json::to_string(entries).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let code = unsafe { windows_sys::Win32::System::Registry::RegFlushKey(k.raw_handle()) };
    if code != 0 {
        return Err("Could not persist the original power setting. The change was stopped.".into());
    }
    Ok(())
}
fn prepare_entry(
    entries: &mut Vec<Entry>,
    setting: &str,
    current: &str,
    value: &str,
) -> Result<(), String> {
    if let Some(e) = entries.iter_mut().find(|e| e.setting == setting) {
        if !e.owns(current) {
            return Err(
                "This power setting changed outside PCSweep. Its current value was preserved."
                    .into(),
            );
        }
        e.applied = value.into();
        e.previous = Some(current.into());
    } else {
        entries.push(Entry {
            setting: setting.into(),
            original: current.into(),
            applied: value.into(),
            previous: Some(current.into()),
        });
    }
    validate_entries(entries)
}
trait Preferences {
    fn get(&mut self, setting: &str) -> Result<String, String>;
    fn set(&mut self, setting: &str, value: &str) -> Result<(), String>;
    fn load(&mut self) -> Result<Vec<Entry>, String>;
    fn save(&mut self, entries: &[Entry]) -> Result<(), String>;
}
struct WindowsPreferences;
impl Preferences for WindowsPreferences {
    fn get(&mut self, setting: &str) -> Result<String, String> {
        get_setting(setting)
    }
    fn set(&mut self, setting: &str, value: &str) -> Result<(), String> {
        set_setting(setting, value)
    }
    fn load(&mut self) -> Result<Vec<Entry>, String> {
        read_undo()
    }
    fn save(&mut self, entries: &[Entry]) -> Result<(), String> {
        save_undo(entries)
    }
}
struct PreferenceLock(isize);
impl PreferenceLock {
    fn take() -> Result<Self, String> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{CreateMutexW, WaitForSingleObject},
        };
        let name: Vec<u16> = "Global\\PCSweep.PowerPreferences.v1"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle == 0 {
            return Err("Power preferences are in use or unavailable.".into());
        }
        let result = unsafe { WaitForSingleObject(handle, 0) };
        if result == 0 || result == 0x80 {
            Ok(Self(handle))
        } else {
            unsafe { CloseHandle(handle) };
            Err("Another PCSweep power operation is running. Refresh shortly.".into())
        }
    }
}
impl Drop for PreferenceLock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
fn apply_transaction(
    backend: &mut impl Preferences,
    setting: &str,
    expected: &str,
    value: &str,
) -> Result<String, String> {
    let current = backend.get(setting)?;
    if current != expected {
        return Err("This power setting changed after review. Refresh first.".into());
    }
    if current == value {
        return Ok("This power preference is already selected.".into());
    }
    let mut entries = backend.load()?;
    let previous_entries = entries.clone();
    prepare_entry(&mut entries, setting, &current, value)?;
    backend.save(&entries)?;
    let result = backend.set(setting, value);
    let actual = backend.get(setting);
    if actual.as_deref() == Ok(current.as_str()) {
        // No change occurred. Restore the preceding journal, including its
        // original Undo target, rather than replacing it with a failed attempt.
        backend.save(&previous_entries)?;
    } else if actual.as_deref() == Ok(value) {
        if let Some(e) = entries.iter_mut().find(|e| e.setting == setting) {
            e.previous = None;
        }
        backend.save(&entries)?;
    }
    result?;
    if actual? != value {
        return Err(
            "Windows did not confirm that configured power preference. Undo remains available."
                .into(),
        );
    }
    Ok("Windows confirmed the configured power preference. Undo is available; system and manufacturer signals can still affect the effective mode.".into())
}
fn apply(setting: &str, expected: &str, value: &str) -> Result<String, String> {
    let _lock = PreferenceLock::take()?;
    if setting == "plan" {
        if value == PERFORMANCE_PLAN && status().ac != Some(true) {
            return Err(
                "Best performance is available only after you connect external power.".into(),
            );
        }
        let snapshot = query()?;
        if !snapshot.hardware.plans.iter().any(|p| p.guid == value) {
            return Err(
                "Windows no longer advertises this plan. Use Power & battery settings.".into(),
            );
        }
    }
    apply_transaction(&mut WindowsPreferences, setting, expected, value)
}
fn undo_transaction(backend: &mut impl Preferences) -> Result<String, String> {
    let mut entries = backend.load()?;
    let mut failures = Vec::new();
    for entry in entries.clone() {
        let result = (|| {
            let current = backend.get(&entry.setting)?;
            if !entry.owns(&current) {
                return Err("Changed outside PCSweep; current value preserved".into());
            }
            if current != entry.original {
                backend.set(&entry.setting, &entry.original)?;
            }
            if backend.get(&entry.setting)? != entry.original {
                return Err("Windows did not confirm restoration".into());
            }
            Ok::<_, String>(())
        })();
        match result {
            Ok(()) => {
                entries.retain(|e| e.setting != entry.setting);
                backend.save(&entries)?;
            }
            Err(e) => failures.push(format!("{}: {e}", entry.setting)),
        }
    }
    if failures.is_empty() {
        Ok("Saved power preferences restored. No unrelated AC/DC settings were changed.".into())
    } else {
        Err(format!(
            "Some preferences were kept for review: {}",
            failures.join("; ")
        ))
    }
}
fn undo() -> Result<String, String> {
    let _lock = PreferenceLock::take()?;
    undo_transaction(&mut WindowsPreferences)
}

fn undo_fingerprint(entries: &[Entry]) -> Result<String, String> {
    validate_entries(entries)?;
    let bytes = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn dismiss_undo_transaction(
    backend: &mut impl Preferences,
    expected: &str,
) -> Result<String, String> {
    let entries = backend.load()?;
    if entries.is_empty() || undo_fingerprint(&entries)? != expected {
        return Err(
            "The saved power Undo changed after review. Refresh before dismissing it.".into(),
        );
    }
    backend.save(&[])?;
    Ok("Saved power Undo dismissed. Current Windows power preferences were kept.".into())
}

fn dismiss_undo(expected: &str) -> Result<String, String> {
    let _lock = PreferenceLock::take()?;
    dismiss_undo_transaction(&mut WindowsPreferences, expected)
}
#[derive(Clone, Copy, Debug)]
pub enum ReportKind {
    Battery,
    Sleep,
    Requests,
    LastWake,
    WakeTimers,
}
impl ReportKind {
    fn label(self) -> &'static str {
        match self {
            Self::Battery => "Battery health report",
            Self::Sleep => "Sleep drain report",
            Self::Requests => "Sleep blockers",
            Self::LastWake => "Last wake event",
            Self::WakeTimers => "Wake timers",
        }
    }
}
#[derive(Clone)]
pub enum PowerAction {
    Set {
        setting: String,
        expected: String,
        value: String,
    },
    Undo,
    DismissUndo {
        expected: String,
    },
    Report(ReportKind),
}
enum Outcome {
    Message(String),
    Report(PathBuf),
}
fn report_script(kind: ReportKind, path: &Path) -> Result<String, String> {
    let exe = platform::system_directory()?.join("powercfg.exe");
    let command = format!(
        "& {}",
        platform::ps_quote(
            exe.to_str()
                .ok_or("Windows tool path could not be encoded")?
        )
    );
    let output = platform::ps_quote(path.to_str().ok_or("Report path could not be encoded")?);
    let script=match kind{ReportKind::Battery=>format!("{command} /batteryreport /output {output}; if($LASTEXITCODE -ne 0){{throw 'Windows could not create a battery report. A battery or permission may be unavailable.'}}"),ReportKind::Sleep=>format!("{command} /sleepstudy /duration 3 /output {output}; if($LASTEXITCODE -ne 0){{throw 'SleepStudy needs a supported Modern Standby device and may need administrator rights. No sleep mode was changed.'}}"),other=>{let arg=match other{ReportKind::Requests=>"/requests",ReportKind::LastWake=>"/lastwake",ReportKind::WakeTimers=>"/waketimers",_=>unreachable!()};format!("{command} {arg} | Out-File -LiteralPath {output} -Encoding utf8; if($LASTEXITCODE -ne 0){{throw 'Windows did not provide this diagnostic. Try the administrator session or native power tools.'}}")}};
    Ok(script)
}
fn make_report(kind: ReportKind) -> Result<PathBuf, String> {
    let dir = util::log_path().with_file_name("reports");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for p in dir.ancestors() {
        if util::is_reparse(&fs::symlink_metadata(p).map_err(|e| e.to_string())?) {
            return Err("Reports cannot be written through links or junctions.".into());
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let ext = if matches!(kind, ReportKind::Battery | ReportKind::Sleep) {
        "html"
    } else {
        "txt"
    };
    let path = dir.join(format!("power-{}-{stamp}.{ext}", std::process::id()));
    platform::powershell(&report_script(kind, &path)?, Duration::from_secs(90))?;
    if !path.is_file() {
        return Err("Windows did not create the requested report.".into());
    }
    Ok(path)
}
pub struct PowerState {
    load: Task<Result<Snapshot, String>>,
    change: Task<Result<Outcome, String>>,
    snapshot: Option<Snapshot>,
    loaded: bool,
    error: Option<String>,
    message: Option<String>,
    report: Option<PathBuf>,
    status: Status,
    ticked: Option<Instant>,
}
impl Default for PowerState {
    fn default() -> Self {
        Self {
            load: Task::default(),
            change: Task::default(),
            snapshot: None,
            loaded: false,
            error: None,
            message: None,
            report: None,
            status: Status::default(),
            ticked: None,
        }
    }
}
impl PowerState {
    pub fn busy(&self) -> bool {
        self.change.running()
    }
}
impl App {
    fn refresh_power(&mut self, ctx: &egui::Context) {
        if !self.power.load.running() && !self.power.change.running() {
            self.power.status = status();
            self.power.load = Task::spawn(ctx, query);
        }
    }
    pub fn poll_power(&mut self, ctx: &egui::Context) {
        if self.page == Page::Power
            && self
                .power
                .ticked
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.power.ticked = Some(Instant::now());
            self.power.status = status();
        }
        if let Some(r) = self.power.load.poll() {
            self.power.loaded = true;
            match r.and_then(|r| r) {
                Ok(s) => self.power.snapshot = Some(s),
                Err(e) => self.power.error = Some(e),
            }
        }
        if let Some(r) = self.power.change.poll() {
            match r.and_then(|r| r) {
                Ok(Outcome::Message(m)) => {
                    util::log(&m);
                    self.power.message = Some(m);
                }
                Ok(Outcome::Report(p)) => {
                    self.power.message = Some(
                        "Windows created your local report. Open it below to review its findings."
                            .into(),
                    );
                    self.power.report = Some(p);
                }
                Err(e) => self.power.error = Some(e),
            }
            self.refresh_power(ctx);
        }
    }
    pub fn execute_power(&mut self, action: PowerAction, ctx: &egui::Context) {
        if self.power.busy() {
            return;
        }
        self.power.error = None;
        self.power.change = Task::spawn(ctx, move || match action {
            PowerAction::Set {
                setting,
                expected,
                value,
            } => apply(&setting, &expected, &value).map(Outcome::Message),
            PowerAction::Undo => undo().map(Outcome::Message),
            PowerAction::DismissUndo { expected } => dismiss_undo(&expected).map(Outcome::Message),
            PowerAction::Report(kind) => make_report(kind).map(Outcome::Report),
        });
    }
    pub fn page_power(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();

        if !self.power.loaded && !self.power.load.running() {
            self.refresh_power(&ctx);
        }

        let mut refresh = false;

        let mut chosen = None;

        let s = self.power.status;

        ScrollArea::vertical()
            .id_salt("power_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                widgets::page_header(ui, "Battery and power", "Use the power you need, understand battery wear, and review sleep drain without changing firmware or sleep models.");

                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.power.load.running() && !self.power.busy(),
                            widgets::small("Refresh"),
                        )
                        .clicked()
                    {
                        refresh = true;
                    }
                    if self.power.load.running() || self.power.busy() {
                        ui.spinner();
                    }
                    if ui.add(widgets::small("Windows Power & battery")).clicked() {
                        util::open_uri("ms-settings:powersleep");
                    }
                });

                widgets::card(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(widgets::strong(
                            match s.ac {
                                Some(true) => "External power connected",
                                Some(false) => "Running on battery",
                                None => "Power source not reported",
                            },
                            20.0,
                        ));
                        if let Some(p) = s.percent {
                            widgets::badge(
                                ui,
                                &format!("{p}%{}", if s.charging { " · charging" } else { "" }),
                                color_success(),
                            );
                        }
                    });

                    ui.label(widgets::dim(match s.battery {
                        Some(false) => "Windows reports no system battery. Power preferences still apply to this PC.",
                        Some(true) => "Windows reports a battery. Runtime and capacity are estimates from the device.",
                        None => "Windows has not reported whether a battery is available.",
                    }));

                    ui.label(widgets::faint(format!(
                        "Windows saver state: {}",
                        match s.saver {
                            Some(true) => "On",
                            Some(false) => "Off",
                            None => "Not reported",
                        }
                    )));
                    if let Some(seconds) = s.seconds {
                        if s.ac != Some(true) {
                            ui.label(widgets::dim(format!(
                                "Windows runtime estimate: {}",
                                util::fmt_duration(seconds as u64)
                            )));
                        }
                    }
                });

                if let Some(e) = &self.power.error {
                    ui.colored_label(color_warning(), e);
                }
                if let Some(m) = &self.power.message {
                    ui.label(widgets::dim(m));
                }

                if let Some(snapshot) = &self.power.snapshot {
                    widgets::section(ui, "Configured power preferences");

                    ui.label(widgets::dim("Balanced is a good everyday choice. Performance can increase heat and fan noise; efficiency may reduce throughput. Windows and manufacturer signals can override a configured preference."));

                    let modern = snapshot.ac.as_ref().ok().zip(snapshot.dc.as_ref().ok());

                    if let Some((ac, dc)) = modern {
                        for (setting, label, current) in
                            [("ac", "Plugged in", ac), ("dc", "On battery", dc)]
                        {
                            widgets::card(ui, |ui| {
                                ui.label(widgets::strong(label, 16.0));
                                ui.label(widgets::dim(format!(
                                    "Configured mode: {}",
                                    mode_name(current).unwrap_or("Custom / unrecognized")
                                )));

                                ui.horizontal_wrapped(|ui| {
                                    for (mode, name) in [
                                        (BALANCED, "Balanced"),
                                        (EFFICIENCY, "Best power efficiency"),
                                        (PERFORMANCE, "Best performance"),
                                    ] {
                                        if setting == "dc" && mode == PERFORMANCE {
                                            continue;
                                        }
                                        if ui
                                            .add_enabled(
                                                !self.mutations_running()
                                                    && mode_name(current).is_some()
                                                    && current != mode,
                                                widgets::small(name),
                                            )
                                            .clicked()
                                        {
                                            chosen = Some(PowerAction::Set {
                                                setting: setting.into(),
                                                expected: current.clone(),
                                                value: mode.into(),
                                            });
                                        }
                                    }
                                });
                            });
                        }

                        ui.label(widgets::faint("Windows applies these separate AC/DC preferences when the power source changes. PCSweep does not need to stay open or install a service."));
                    } else {
                        ui.label(widgets::dim("Modern power-mode controls are unavailable on this build. Review the supported Windows power plans below or use Windows Settings."));

                        if let Ok(current) = &snapshot.plan {
                            for plan in &snapshot.hardware.plans {
                                if [BALANCED_PLAN, PERFORMANCE_PLAN, SAVER_PLAN]
                                    .contains(&plan.guid.as_str())
                                {
                                    let allowed =
                                        plan.guid != PERFORMANCE_PLAN || s.ac == Some(true);
                                    if ui
                                        .add_enabled(
                                            !self.mutations_running()
                                                && current != &plan.guid
                                                && allowed,
                                            widgets::small(format!(
                                                "Use {} (power plan)",
                                                plan.name
                                            )),
                                        )
                                        .clicked()
                                    {
                                        chosen = Some(PowerAction::Set {
                                            setting: "plan".into(),
                                            expected: current.clone(),
                                            value: plan.guid.clone(),
                                        });
                                    }
                                }
                            }
                        }

                        ui.label(widgets::faint("A legacy power plan is different from Windows 11 Power Mode. Hidden/OEM plans are not created or forced."));
                    }

                    match &snapshot.undo {
                        Ok(entries) if !entries.is_empty() => {
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .add_enabled(
                                        !self.mutations_running(),
                                        widgets::ghost("Undo PCSweep power changes"),
                                    )
                                    .clicked()
                                {
                                    chosen = Some(PowerAction::Undo);
                                }
                                if let Ok(expected) = undo_fingerprint(entries) {
                                    if ui
                                        .add_enabled(
                                            !self.mutations_running(),
                                            widgets::small("Dismiss saved power Undo…"),
                                        )
                                        .clicked()
                                    {
                                        chosen = Some(PowerAction::DismissUndo { expected });
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            ui.colored_label(color_warning(), e);
                        }
                        _ => {}
                    }

                    widgets::section(ui, "Battery health");
                    widgets::card(ui, |ui| {
                        match (snapshot.hardware.design, snapshot.hardware.full) {
                            (Some(design), Some(full)) if design > 0 => {
                                ui.label(widgets::strong(
                                    format!(
                                        "Reported full-charge capacity: {:.0}% of design",
                                        full as f64 / design as f64 * 100.0
                                    ),
                                    17.0,
                                ));
                                ui.label(widgets::dim(format!("Design: {design} mWh · Current full charge: {full} mWh")));
                            }
                            _ => {
                                ui.label(widgets::dim("The firmware did not provide comparable battery capacities. The Windows battery report may have more detail."));
                            }
                        }
                        if let Some(c) = snapshot.hardware.cycles {
                            ui.label(widgets::dim(format!("Reported cycle count: {c}")));
                        }
                        ui.label(widgets::faint("Capacity reports depend on device calibration and may exceed 100%. They are not a laboratory measurement or a guaranteed runtime."));
                    });

                    if let Some(w) = &snapshot.hardware.warning {
                        ui.label(widgets::faint(w));
                    }
                }

                widgets::section(ui, "Reports and sleep drain");
                ui.label(widgets::dim("Create local Windows reports. SleepStudy requires a supported Modern Standby device; some diagnostics require administrator rights. No sleep mode or timer is changed."));

                ui.horizontal_wrapped(|ui| {
                    for kind in [
                        ReportKind::Battery,
                        ReportKind::Sleep,
                        ReportKind::Requests,
                        ReportKind::LastWake,
                        ReportKind::WakeTimers,
                    ] {
                        if ui
                            .add_enabled(!self.mutations_running(), widgets::small(kind.label()))
                            .clicked()
                        {
                            chosen = Some(PowerAction::Report(kind));
                        }
                    }
                });

                if let Some(path) = &self.power.report {
                    ui.horizontal_wrapped(|ui| {
                        if ui.add(widgets::primary("Open last report")).clicked() {
                            util::open_path(path);
                        }
                        if ui.add(widgets::small("Show report folder")).clicked() {
                            util::show_in_explorer(path);
                        }
                    });
                }

                widgets::section(ui, "Charging and battery saver");
                ui.label(widgets::dim("Use Energy Saver / Battery Saver and screen recommendations in Windows. Charge caps and Smart Charging depend on the manufacturer; PCSweep does not promise a universal 80% switch or change firmware."));

                ui.horizontal_wrapped(|ui| {
                    if ui.add(widgets::small("Energy Saver settings")).clicked() {
                        util::open_uri("ms-settings:batterysaver");
                    }
                    if ui
                        .add(widgets::small("Windows battery care guidance"))
                        .clicked()
                    {
                        util::open_uri("https://support.microsoft.com/en-us/windows/experience/power-battery/caring-for-your-battery-in-windows");
                    }
                });
            });

        if refresh {
            self.power.error = None;
            self.refresh_power(&ctx);
        }

        if let Some(action) = chosen {
            let (title, body, confirm) = match &action {
                PowerAction::Set { setting, value, .. } => (
                    "Change this power preference?".into(),
                    format!(
                        "Apply {} for {}. PCSweep saves the original setting first. Other AC/DC settings, networking, firmware and sleep models are kept.",
                        mode_name(value).unwrap_or("the selected power plan"),
                        match setting.as_str() {
                            "ac" => "plugged-in use",
                            "dc" => "battery use",
                            _ => "this Windows session",
                        }
                    ),
                    "Apply preference",
                ),
                PowerAction::Undo => (
                    "Restore saved power preferences?".into(),
                    "Restore only the preferences PCSweep changed. Values changed outside PCSweep are preserved for review.".into(),
                    "Restore preferences",
                ),
                PowerAction::DismissUndo { .. } => (
                    "Dismiss saved power Undo?".into(),
                    "Keep the current Windows power preferences and remove PCSweep's saved Undo targets. The original preferences will no longer be offered for restoration. Future changes will save a new baseline.".into(),
                    "Keep current and dismiss Undo",
                ),
                PowerAction::Report(kind) => (
                    format!("Create {}?", kind.label()),
                    "Ask Windows to create a local diagnostic report in PCSweep's reports folder. The report can contain device and usage information; nothing is uploaded or changed in the power configuration.".into(),
                    "Create report",
                ),
            };

            self.modal = Some(Modal {
                title,
                body,
                details: Vec::new(),
                note: Some(if matches!(&action, PowerAction::DismissUndo { .. }) {
                    "This only removes the reviewed Undo record. No Windows power setting is changed.".into()
                } else {
                    "Windows may refuse organization-managed or unsupported settings. PCSweep verifies applied values and reports failures.".into()
                }),
                confirm: confirm.into(),
                danger: matches!(&action, PowerAction::DismissUndo { .. }),
                action: Action::Power(action),
            });
        }
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> PowerState {
    PowerState {
        loaded: true,
        status: Status {
            ac: Some(false),
            battery: Some(true),
            percent: Some(64),
            charging: false,
            saver: Some(true),
            seconds: Some(10800),
        },
        snapshot: Some(Snapshot {
            hardware: Hardware {
                design: Some(60000),
                full: Some(51000),
                cycles: Some(235),
                ..Default::default()
            },
            ac: Ok(BALANCED.into()),
            dc: Ok(EFFICIENCY.into()),
            plan: Ok(BALANCED_PLAN.into()),
            undo: Ok(Vec::new()),
        }),
        ..Default::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct FakePreferences {
        current: String,
        entries: Vec<Entry>,
        fail_save: bool,
        fail_set: bool,
        sets: usize,
    }
    impl FakePreferences {
        fn new() -> Self {
            Self {
                current: BALANCED.into(),
                entries: Vec::new(),
                fail_save: false,
                fail_set: false,
                sets: 0,
            }
        }
    }
    impl Preferences for FakePreferences {
        fn get(&mut self, _: &str) -> Result<String, String> {
            Ok(self.current.clone())
        }
        fn set(&mut self, _: &str, value: &str) -> Result<(), String> {
            self.sets += 1;
            if self.fail_set {
                return Err("injected Windows refusal".into());
            }
            self.current = value.into();
            Ok(())
        }
        fn load(&mut self) -> Result<Vec<Entry>, String> {
            Ok(self.entries.clone())
        }
        fn save(&mut self, entries: &[Entry]) -> Result<(), String> {
            if self.fail_save {
                return Err("injected persistence failure".into());
            }
            self.entries = entries.into();
            Ok(())
        }
    }
    #[test]
    fn journal_failure_stops_changes_and_failed_second_change_retains_undo() {
        let mut backend = FakePreferences::new();
        backend.fail_save = true;
        assert!(apply_transaction(&mut backend, "ac", BALANCED, PERFORMANCE).is_err());
        assert_eq!(backend.sets, 0);
        backend.fail_save = false;
        apply_transaction(&mut backend, "ac", BALANCED, PERFORMANCE).unwrap();
        assert!(backend.entries[0].previous.is_none());
        backend.fail_set = true;
        assert!(apply_transaction(&mut backend, "ac", PERFORMANCE, EFFICIENCY).is_err());
        assert_eq!(backend.entries[0].original, BALANCED);
        assert_eq!(backend.entries[0].applied, PERFORMANCE);
        backend.fail_set = false;
        undo_transaction(&mut backend).unwrap();
        assert_eq!(backend.current, BALANCED);
        assert!(backend.entries.is_empty());
    }
    #[test]
    fn changed_after_review_and_outside_undo_are_preserved() {
        let mut backend = FakePreferences::new();
        assert!(apply_transaction(&mut backend, "ac", PERFORMANCE, EFFICIENCY).is_err());
        assert_eq!(backend.sets, 0);
        apply_transaction(&mut backend, "ac", BALANCED, PERFORMANCE).unwrap();
        backend.current = EFFICIENCY.into();
        assert!(undo_transaction(&mut backend).is_err());
        assert_eq!(backend.current, EFFICIENCY);
        assert_eq!(backend.sets, 1);
        assert_eq!(backend.entries.len(), 1);
    }
    #[test]
    fn dismissing_reviewed_undo_never_changes_power_and_rejects_a_changed_record() {
        let mut backend = FakePreferences::new();
        prepare_entry(&mut backend.entries, "ac", BALANCED, PERFORMANCE).unwrap();
        let reviewed = undo_fingerprint(&backend.entries).unwrap();
        backend.entries[0].applied = EFFICIENCY.into();
        let changed = backend.entries.clone();
        assert!(dismiss_undo_transaction(&mut backend, &reviewed).is_err());
        assert_eq!(backend.entries, changed);
        assert_eq!(backend.sets, 0);

        let reviewed = undo_fingerprint(&backend.entries).unwrap();
        backend.fail_save = true;
        assert!(dismiss_undo_transaction(&mut backend, &reviewed).is_err());
        assert_eq!(backend.entries, changed);
        backend.fail_save = false;
        dismiss_undo_transaction(&mut backend, &reviewed).unwrap();
        assert!(backend.entries.is_empty());
        assert_eq!(backend.current, BALANCED);
        assert_eq!(backend.sets, 0);

        backend.entries = changed;
        backend.entries[0].setting = "corrupt".into();
        let invalid = backend.entries.clone();
        assert!(dismiss_undo_transaction(&mut backend, &reviewed).is_err());
        assert_eq!(backend.entries, invalid);
        assert_eq!(backend.sets, 0);
    }
    #[test]
    fn original_mode_survives_repeated_changes() {
        let mut entries = Vec::new();
        prepare_entry(&mut entries, "ac", BALANCED, PERFORMANCE).unwrap();
        prepare_entry(&mut entries, "ac", PERFORMANCE, EFFICIENCY).unwrap();
        assert_eq!(entries[0].original, BALANCED);
        assert_eq!(entries[0].applied, EFFICIENCY);
        assert!(prepare_entry(&mut entries, "ac", "custom", BALANCED).is_err());
    }
    #[test]
    fn undo_scope_rejects_unknown_modes_and_arbitrary_targets() {
        assert!(validate_entries(&[Entry {
            setting: "registry".into(),
            original: BALANCED.into(),
            applied: PERFORMANCE.into(),
            previous: None
        }])
        .is_err());
        assert!(validate_entries(&[Entry {
            setting: "dc".into(),
            original: "11111111-2222-3333-4444-555555555555".into(),
            applied: EFFICIENCY.into(),
            previous: None
        }])
        .is_err());
    }
    #[test]
    fn mode_guid_roundtrip_and_unknown_are_honest() {
        for id in [BALANCED, EFFICIENCY, PERFORMANCE] {
            assert_eq!(guid_text(&dns::parse_guid(id).unwrap()), id);
            assert!(mode_name(id).is_some());
        }
        assert!(mode_name("unknown").is_none());
    }
    #[test]
    fn native_status_and_queries_are_read_only() {
        let data = platform::powershell(HARDWARE_SCRIPT, Duration::from_secs(30)).unwrap();
        let hardware: Hardware = serde_json::from_str(&data).unwrap();
        assert_eq!(hardware.design.is_some(), hardware.full.is_some());
        let _ = status();
        let _ = active_plan();
        let _ = get_setting("ac");
        let _ = get_setting("dc");
    }
    #[test]
    fn report_scripts_parse_without_running_diagnostics() {
        let mut scripts = vec![HARDWARE_SCRIPT.to_owned()];
        for kind in [
            ReportKind::Battery,
            ReportKind::Sleep,
            ReportKind::Requests,
            ReportKind::LastWake,
            ReportKind::WakeTimers,
        ] {
            scripts.push(report_script(kind, Path::new(r"C:\Temp\PCSweep report.html")).unwrap());
        }
        for script in scripts {
            let check=format!("$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors);if($errors.Count){{throw ($errors|Out-String)}}",platform::ps_quote(&script));
            platform::powershell(&check, Duration::from_secs(15)).unwrap();
        }
    }
}
