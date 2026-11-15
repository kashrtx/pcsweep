//! Supported, reviewed Windows preferences and local change history.
//! Policy readback is deliberately not advertised as proof of enforcement.
use crate::{
    app::{Action, App, Modal},
    platform,
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use winreg::{enums::*, RegKey, RegValue};

const UPDATE: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate";
const AU: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU";
const USER_UNDO: &str = r"Software\PCSweep\WindowsPreferencesUndo";
const MACHINE_UNDO: &str = r"Software\PCSweep\WindowsUpdateUndo";
const IDS: &[&str] = &[
    "no_auto",
    "download_mode",
    "drivers",
    "client_animation",
    "window_animation",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Value {
    kind: u32,
    bytes: Vec<u8>,
}
impl Value {
    fn dword(value: u32) -> Self {
        Self {
            kind: REG_DWORD as u32,
            bytes: value.to_le_bytes().to_vec(),
        }
    }
    fn number(&self) -> Option<u32> {
        (self.kind == REG_DWORD as u32)
            .then(|| {
                self.bytes
                    .as_slice()
                    .try_into()
                    .ok()
                    .map(u32::from_le_bytes)
            })
            .flatten()
    }
    fn validate(&self) -> Result<(), String> {
        if self.kind != REG_DWORD as u32 || self.bytes.len() != 4 {
            return Err(
                "The saved preference has an unsupported type. No setting was changed.".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    id: String,
    original: Option<Value>,
    applied: Option<Value>,
    // Some(None) represents an in-flight deletion of a registry value.
    pending: Option<Option<Value>>,
}
impl Entry {
    fn owns(&self, current: &Option<Value>) -> bool {
        current == &self.original
            || current == &self.applied
            || self.pending.as_ref() == Some(current)
    }
}
fn machine(id: &str) -> bool {
    matches!(id, "no_auto" | "download_mode" | "drivers")
}
fn target(id: &str) -> Result<(&'static str, &'static str), String> {
    match id {
        "no_auto" => Ok((AU, "NoAutoUpdate")),
        "download_mode" => Ok((AU, "AUOptions")),
        "drivers" => Ok((UPDATE, "ExcludeWUDriversInQualityUpdate")),
        _ => Err("Unknown Windows policy target".into()),
    }
}
fn validate_entries(entries: &[Entry], system: bool) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    if entries.len() > IDS.len() {
        return Err("The Windows Undo record is invalid.".into());
    }
    for e in entries {
        if !IDS.contains(&e.id.as_str()) || machine(&e.id) != system || !seen.insert(&e.id) {
            return Err("The Windows Undo scope is invalid.".into());
        }
        for value in [&e.original, &e.applied]
            .into_iter()
            .chain(e.pending.iter())
        {
            if let Some(value) = value {
                value.validate()?;
            }
        }
    }
    Ok(())
}
fn raw_policy(id: &str) -> Result<Option<Value>, String> {
    let (path, name) = target(id)?;
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    match key.get_raw_value(name) {
        Ok(v) => Ok(Some(Value {
            kind: v.vtype as u32,
            bytes: v.bytes,
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn animation(id: &str, value: Option<bool>) -> Result<bool, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let mut enabled: i32 = 0;
    let mut info = ANIMATIONINFO {
        cbSize: std::mem::size_of::<ANIMATIONINFO>() as u32,
        iMinAnimate: 0,
    };
    let success = unsafe {
        match (id, value) {
            ("client_animation", None) => SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut enabled as *mut i32).cast(),
                0,
            ),
            ("client_animation", Some(v)) => SystemParametersInfoW(
                SPI_SETCLIENTAREAANIMATION,
                0,
                usize::from(v) as *mut std::ffi::c_void,
                SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
            ),
            ("window_animation", None) => SystemParametersInfoW(
                SPI_GETANIMATION,
                info.cbSize,
                (&mut info as *mut ANIMATIONINFO).cast(),
                0,
            ),
            ("window_animation", Some(v)) => {
                info.iMinAnimate = i32::from(v);
                SystemParametersInfoW(
                    SPI_SETANIMATION,
                    info.cbSize,
                    (&mut info as *mut ANIMATIONINFO).cast(),
                    SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
                )
            }
            _ => return Err("Unknown animation preference".into()),
        }
    };
    if success == 0 {
        return Err(format!(
            "Windows did not provide or accept this animation preference: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(value.unwrap_or(if id == "window_animation" {
        info.iMinAnimate != 0
    } else {
        enabled != 0
    }))
}
fn read_value(id: &str) -> Result<Option<Value>, String> {
    if machine(id) {
        raw_policy(id)
    } else {
        animation(id, None).map(|v| Some(Value::dword(u32::from(v))))
    }
}
fn write_value(id: &str, value: &Option<Value>) -> Result<(), String> {
    if !machine(id) {
        let v = value
            .as_ref()
            .and_then(Value::number)
            .filter(|v| *v <= 1)
            .ok_or("Invalid saved animation preference")?;
        animation(id, Some(v == 1))?;
        return Ok(());
    }
    if !crate::system::is_admin() {
        return Err("Restart as administrator to change Windows Update policy.".into());
    }
    let (path, name) = target(id)?;
    let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey_with_flags(path, KEY_READ | KEY_WRITE)
        .map_err(|e| e.to_string())?;
    if let Some(v) = value {
        v.validate()?;
        key.set_raw_value(
            name,
            &RegValue {
                vtype: REG_DWORD,
                bytes: v.bytes.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
    } else {
        match key.delete_value(name) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
fn journal_root(system: bool) -> RegKey {
    RegKey::predef(if system {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    })
}
fn journal_path(system: bool) -> &'static str {
    if system {
        MACHINE_UNDO
    } else {
        USER_UNDO
    }
}
fn load_entries(system: bool) -> Result<Vec<Entry>, String> {
    let key = match journal_root(system).open_subkey(journal_path(system)) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Windows Undo could not be read: {e}")),
    };
    let text: String = match key.get_value("EntriesV1") {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    if text.len() > 32768 {
        return Err("Windows Undo is too large. Changes are blocked.".into());
    }
    let entries = serde_json::from_str::<Vec<Entry>>(&text)
        .map_err(|_| "Windows Undo is damaged. Changes are blocked.")?;
    validate_entries(&entries, system)?;
    Ok(entries)
}
fn protect_machine_journal(key: &RegKey) -> Result<(), String> {
    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            version: u32,
            descriptor: *mut *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
        fn RegSetKeySecurity(
            key: isize,
            information: u32,
            descriptor: *const std::ffi::c_void,
        ) -> i32;
    }
    let s: Vec<u16> = "D:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;BU)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            s.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err("Could not protect Windows Update Undo.".into());
    }
    let code = unsafe { RegSetKeySecurity(key.raw_handle(), 4 | 0x8000_0000, descriptor) };
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(descriptor);
    }
    if code != 0 {
        return Err(format!(
            "Could not secure Windows Update Undo (code {code})."
        ));
    }
    Ok(())
}
fn save_entries(system: bool, entries: &[Entry]) -> Result<(), String> {
    validate_entries(entries, system)?;
    if system && !crate::system::is_admin() {
        return Err("Administrator rights are needed to save Windows Update Undo.".into());
    }
    let (key, _) = journal_root(system)
        .create_subkey_with_flags(journal_path(system), KEY_ALL_ACCESS)
        .map_err(|e| e.to_string())?;
    if system {
        protect_machine_journal(&key)?;
    }
    key.set_value(
        "EntriesV1",
        &serde_json::to_string(entries).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let code = unsafe { windows_sys::Win32::System::Registry::RegFlushKey(key.raw_handle()) };
    if code != 0 {
        return Err("Could not persist Windows Undo. No further changes were attempted.".into());
    }
    Ok(())
}
struct Lock(isize);
impl Lock {
    fn take() -> Result<Self, String> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{CreateMutexW, WaitForSingleObject},
        };
        let name: Vec<u16> = "Global\\PCSweep.WindowsControls.v1"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle == 0 {
            return Err("Windows preferences are in use or unavailable.".into());
        }
        match unsafe { WaitForSingleObject(handle, 0) } {
            0 | 0x80 => Ok(Self(handle)),
            _ => {
                unsafe { CloseHandle(handle) };
                Err("Another Windows preference operation is running.".into())
            }
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

trait Backend {
    fn read(&mut self, id: &str) -> Result<Option<Value>, String>;
    fn write(&mut self, id: &str, value: &Option<Value>) -> Result<(), String>;
    fn load(&mut self) -> Result<Vec<Entry>, String>;
    fn save(&mut self, entries: &[Entry]) -> Result<(), String>;
}
struct WindowsBackend(bool);
impl Backend for WindowsBackend {
    fn read(&mut self, id: &str) -> Result<Option<Value>, String> {
        read_value(id)
    }
    fn write(&mut self, id: &str, value: &Option<Value>) -> Result<(), String> {
        write_value(id, value)
    }
    fn load(&mut self) -> Result<Vec<Entry>, String> {
        load_entries(self.0)
    }
    fn save(&mut self, entries: &[Entry]) -> Result<(), String> {
        save_entries(self.0, entries)
    }
}
fn fingerprint(values: &[(String, Option<Value>)]) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(values).map_err(|e| e.to_string())?)
    ))
}
fn desired(id: &str) -> Result<Vec<(String, Option<Value>)>, String> {
    let pairs: &[(&str, u32)] = match id {
        "manual" => &[("no_auto", 1)],
        "notify" => &[("no_auto", 0), ("download_mode", 2)],
        "exclude_drivers" => &[("drivers", 1)],
        "client_off" => &[("client_animation", 0)],
        "client_on" => &[("client_animation", 1)],
        "window_off" => &[("window_animation", 0)],
        "window_on" => &[("window_animation", 1)],
        _ => return Err("Unknown Windows control".into()),
    };
    Ok(pairs
        .iter()
        .map(|(id, v)| (id.to_string(), Some(Value::dword(*v))))
        .collect())
}
fn transaction(
    backend: &mut impl Backend,
    changes: &[(String, Option<Value>)],
    expected: &str,
) -> Result<String, String> {
    let current = changes
        .iter()
        .map(|(id, _)| backend.read(id).map(|v| (id.clone(), v)))
        .collect::<Result<Vec<_>, _>>()?;
    if fingerprint(&current)? != expected {
        return Err("A preference changed after review. Refresh before changing it.".into());
    }
    let mut entries = backend.load()?;
    if entries.iter().any(|e| e.pending.is_some()) {
        return Err(
            "A previous operation was interrupted. Undo it before making another change.".into(),
        );
    }
    for ((id, value), (_, current)) in changes.iter().zip(&current) {
        if let Some(e) = entries.iter_mut().find(|e| &e.id == id) {
            if !e.owns(current) {
                return Err("A preference changed outside PCSweep. It was preserved.".into());
            }
            e.pending = Some(value.clone());
        } else {
            if machine(id) && current.is_some() {
                return Err("This update policy was already configured outside PCSweep. Review it in Windows or with your administrator.".into());
            }
            entries.push(Entry {
                id: id.clone(),
                original: current.clone(),
                applied: current.clone(),
                pending: Some(value.clone()),
            });
        }
    }
    backend.save(&entries)?;
    for ((id, value), (_, reviewed)) in changes.iter().zip(&current) {
        if backend.read(id)? != *reviewed {
            return Err("A preference changed while applying the reviewed group. Further writes were stopped; Undo remains available for the completed changes.".into());
        }
        backend.write(id, value)?;
        if backend.read(id)? != *value {
            return Err(
                "Windows did not confirm a preference. The original Undo record was kept.".into(),
            );
        }
    }
    for (id, value) in changes {
        let e = entries
            .iter_mut()
            .find(|e| &e.id == id)
            .ok_or("Windows Undo changed unexpectedly")?;
        e.applied = value.clone();
        e.pending = None;
    }
    backend.save(&entries)?;
    Ok("Configured values saved and read back. Undo is available. Windows policy refresh and other system signals determine effective behavior.".into())
}
fn undo_transaction(backend: &mut impl Backend) -> Result<String, String> {
    let mut entries = backend.load()?;
    let mut failures = Vec::new();
    for entry in entries.clone() {
        let result = (|| {
            let current = backend.read(&entry.id)?;
            if !entry.owns(&current) {
                return Err("changed outside PCSweep; preserved".to_string());
            }
            if current != entry.original {
                backend.write(&entry.id, &entry.original)?;
            }
            if backend.read(&entry.id)? != entry.original {
                return Err("restoration was not confirmed".into());
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                entries.retain(|e| e.id != entry.id);
                backend.save(&entries)?;
            }
            Err(e) => failures.push(format!("{}: {e}", entry.id)),
        }
    }
    if failures.is_empty() {
        Ok("Original Windows preferences restored. Unrelated settings were kept.".into())
    } else {
        Err(format!(
            "Some saved preferences need review: {}",
            failures.join("; ")
        ))
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Environment {
    edition: String,
    build: u32,
    domain: Option<bool>,
    pending_restart: bool,
    update_api_level: Option<u32>,
}
const ENVIRONMENT_SCRIPT: &str = r#"
$v=Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -ErrorAction Stop
$domain=$null;try{$domain=[bool](Get-CimInstance Win32_ComputerSystem -Property PartOfDomain -ErrorAction Stop).PartOfDomain}catch{}
$restart=(Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired') -or (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending')
$level=$null;try{$auto=New-Object -ComObject Microsoft.Update.AutoUpdate;$level=[uint32]$auto.Settings.NotificationLevel}catch{}
[pscustomobject]@{edition=[string]$v.EditionID;build=[uint32]$v.CurrentBuildNumber;domain=$domain;pending_restart=$restart;update_api_level=$level}|ConvertTo-Json -Compress
"#;
fn present_key(path: &str) -> Result<bool, String> {
    match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) {
        Ok(k) => {
            for item in k.enum_values() {
                item.map_err(|e| e.to_string())?;
                return Ok(true);
            }
            for item in k.enum_keys() {
                item.map_err(|e| e.to_string())?;
                return Ok(true);
            }
            Ok(false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Management state is unavailable: {e}")),
    }
}
fn policy_guard(environment: &Environment) -> Result<(), String> {
    if environment.build < 19045
        || !matches!(
            environment.edition.as_str(),
            "Professional"
                | "ProfessionalN"
                | "ProfessionalEducation"
                | "ProfessionalWorkstation"
                | "Enterprise"
                | "EnterpriseS"
                | "Education"
                | "EducationN"
        )
    {
        return Err("These policy controls are unavailable for this Windows edition/build. Use the native update controls below.".into());
    }
    if environment.domain != Some(false)
        || present_key(r"SOFTWARE\Microsoft\Provisioning\OMADM\Accounts")?
    {
        return Err(
            "Windows may be organization-managed. PCSweep keeps administrator policy unchanged."
                .into(),
        );
    }
    // Any unrelated update policy makes the context uncertain. Do not override
    // existing deployment/deadline/WSUS configuration or its parent policy.
    for (path, allow) in [
        (UPDATE, &["ExcludeWUDriversInQualityUpdate"][..]),
        (AU, &["NoAutoUpdate", "AUOptions"][..]),
    ] {
        match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) {
            Ok(key) => {
                for value in key.enum_values() {
                    let (name, _) = value.map_err(|e| e.to_string())?;
                    if !allow.iter().any(|v| name.eq_ignore_ascii_case(v)) {
                        return Err("Other Windows Update policy is configured. Review it in Windows; PCSweep will not override it.".into());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
struct Snapshot {
    environment: Environment,
    policy_guard: Result<(), String>,
    values: Vec<(String, Result<Option<Value>, String>)>,
    machine_undo: Result<Vec<Entry>, String>,
    user_undo: Result<Vec<Entry>, String>,
}
impl Snapshot {
    fn values_for(
        &self,
        changes: &[(String, Option<Value>)],
    ) -> Result<Vec<(String, Option<Value>)>, String> {
        changes
            .iter()
            .map(|(id, _)| {
                let value = self
                    .values
                    .iter()
                    .find(|(key, _)| key == id)
                    .ok_or("Preference is unavailable")?;
                let raw = value.1.clone()?;
                if let Some(v) = &raw {
                    v.validate()?;
                }
                Ok((id.clone(), raw))
            })
            .collect()
    }
}
fn environment() -> Result<Environment, String> {
    serde_json::from_str(&platform::powershell(
        ENVIRONMENT_SCRIPT,
        Duration::from_secs(20),
    )?)
    .map_err(|e| format!("Windows status could not be read: {e}"))
}
fn query() -> Result<Snapshot, String> {
    let environment = environment()?;
    Ok(Snapshot {
        policy_guard: policy_guard(&environment),
        environment,
        values: IDS
            .iter()
            .map(|id| (id.to_string(), read_value(id)))
            .collect(),
        machine_undo: load_entries(true),
        user_undo: load_entries(false),
    })
}
#[derive(Clone, Deserialize)]
struct Event {
    time: String,
    source: String,
    description: String,
}
#[derive(Default, Deserialize)]
struct Timeline {
    events: Vec<Event>,
    warnings: Vec<String>,
}
const TIMELINE_SCRIPT: &str = r#"
$events=@();$warnings=@();$outcomes=@{0='Not started';1='In progress';2='Succeeded';3='Succeeded with errors';4='Failed';5='Canceled'}
try {
 $session=New-Object -ComObject Microsoft.Update.Session
 $searcher=$session.CreateUpdateSearcher();$searcher.Online=$false
 $count=[Math]::Min(40,$searcher.GetTotalHistoryCount())
 if($count -gt 0){$events+=@($searcher.QueryHistory(0,$count)|ForEach-Object{[pscustomobject]@{time=$_.Date.ToString('yyyy-MM-dd HH:mm:ss');source='Windows Update';description=([string]$_.Title)+' | '+[string]$outcomes[[int]$_.ResultCode]}})}
}catch{$warnings+='Windows Update history: '+$_.Exception.Message}
try {
 $events+=@(Get-CimInstance Win32_ReliabilityRecords -Filter "TimeGenerated >= '$((Get-Date).ToUniversalTime().AddDays(-30).ToString('yyyyMMddHHmmss.000000+000'))'" -ErrorAction Stop|Sort-Object TimeGenerated -Descending|Select-Object -First 40|ForEach-Object{[pscustomobject]@{time=$_.TimeGenerated.ToString('yyyy-MM-dd HH:mm:ss');source=[string]$_.SourceName;description=([string]$_.ProductName)+' | '+[string]$_.Message}})
}catch{$warnings+='Windows reliability history: '+$_.Exception.Message}
[pscustomobject]@{events=@($events|Sort-Object time -Descending);warnings=@($warnings)}|ConvertTo-Json -Depth 4 -Compress
"#;
fn timeline() -> Result<Timeline, String> {
    serde_json::from_str(&platform::powershell(
        TIMELINE_SCRIPT,
        Duration::from_secs(30),
    )?)
    .map_err(|e| format!("Windows change history could not be read: {e}"))
}

#[derive(Clone)]
pub enum WindowsControlsAction {
    Set { control: String, expected: String },
    Undo { system: bool },
    Discard { system: bool, expected: String },
}
fn entries_fingerprint(entries: &[Entry]) -> Result<String, String> {
    if entries.is_empty() {
        return Err("There is no saved Windows Undo to dismiss.".into());
    }
    validate_entries(entries, machine(&entries[0].id))?;
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(entries).map_err(|e| e.to_string())?)
    ))
}
fn discard_transaction(backend: &mut impl Backend, expected: &str) -> Result<String, String> {
    let entries = backend.load()?;
    if entries_fingerprint(&entries)? != expected {
        return Err("Windows Undo changed after review. Refresh before dismissing it.".into());
    }
    backend.save(&[])?;
    Ok("Saved Windows Undo dismissed. No Windows preference was changed.".into())
}
fn execute(action: WindowsControlsAction) -> Result<String, String> {
    let _lock = Lock::take()?;
    match action {
        WindowsControlsAction::Set { control, expected } => {
            let changes = desired(&control)?;
            let system = changes.first().is_some_and(|(id, _)| machine(id));
            if system {
                policy_guard(&environment()?)?;
            }
            transaction(&mut WindowsBackend(system), &changes, &expected)
        }
        WindowsControlsAction::Discard { system, expected } => {
            discard_transaction(&mut WindowsBackend(system), &expected)
        }
        WindowsControlsAction::Undo { system } => {
            if system {
                policy_guard(&environment()?)?;
            }
            undo_transaction(&mut WindowsBackend(system))
        }
    }
}
#[derive(Default)]
pub struct WindowsControlsState {
    load: Task<Result<Snapshot, String>>,
    change: Task<Result<String, String>>,
    history: Task<Result<Timeline, String>>,
    snapshot: Option<Snapshot>,
    timeline: Option<Timeline>,
    loaded: bool,
    error: Option<String>,
    message: Option<String>,
    history_filter: String,
}
impl WindowsControlsState {
    pub fn busy(&self) -> bool {
        self.change.running()
    }
}
impl App {
    fn refresh_windows_controls(&mut self, ctx: &egui::Context) {
        if !self.windows_controls.load.running() && !self.windows_controls.change.running() {
            self.windows_controls.load = Task::spawn(ctx, query);
        }
    }
    pub fn execute_windows_controls(&mut self, action: WindowsControlsAction, ctx: &egui::Context) {
        if self.windows_controls.busy() {
            return;
        }
        self.windows_controls.error = None;
        self.windows_controls.change = Task::spawn(ctx, move || execute(action));
    }
    pub fn poll_windows_controls(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.windows_controls.load.poll() {
            self.windows_controls.loaded = true;
            match result.and_then(|r| r) {
                Ok(snapshot) => self.windows_controls.snapshot = Some(snapshot),
                Err(e) => self.windows_controls.error = Some(e),
            }
        }
        if let Some(result) = self.windows_controls.change.poll() {
            match result.and_then(|r| r) {
                Ok(message) => {
                    util::log(&message);
                    self.windows_controls.message = Some(message);
                }
                Err(e) => {
                    util::log(format!("Windows preferences: {e}"));
                    self.windows_controls.error = Some(e);
                }
            }
            self.refresh_windows_controls(ctx);
        }
        if let Some(result) = self.windows_controls.history.poll() {
            match result.and_then(|r| r) {
                Ok(history) => self.windows_controls.timeline = Some(history),
                Err(e) => self.windows_controls.error = Some(e),
            }
        }
    }
    pub fn page_windows_controls(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.windows_controls.loaded && !self.windows_controls.load.running() {
            self.refresh_windows_controls(&ctx);
        }
        let mut refresh = false;
        let mut action = None;
        let busy = self.mutations_running();
        ScrollArea::vertical()
            .id_salt("windows_preferences")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                widgets::page_header(ui, "Windows controls", "Review update policy, reduce animation overhead, and investigate changes using local Windows history.");
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.windows_controls.load.running() && !self.windows_controls.busy(),
                            widgets::small("Refresh status"),
                        )
                        .clicked()
                    {
                        refresh = true;
                    }
                    if self.windows_controls.load.running() || self.windows_controls.busy() {
                        ui.spinner();
                    }
                    if ui.add(widgets::small("Windows Update settings")).clicked() {
                        util::open_uri("ms-settings:windowsupdate");
                    }
                });
                if let Some(e) = &self.windows_controls.error {
                    ui.colored_label(WARN, e);
                }
                if let Some(m) = &self.windows_controls.message {
                    ui.label(widgets::dim(m));
                }
                if let Some(snapshot) = &self.windows_controls.snapshot {
                    widgets::card(ui, |ui| {
                        ui.label(widgets::strong(
                            format!(
                                "Windows {} · build {}",
                                snapshot.environment.edition, snapshot.environment.build
                            ),
                            17.0,
                        ));
                        ui.label(widgets::dim(format!(
                            "Domain membership: {}",
                            match snapshot.environment.domain {
                                Some(true) => "joined",
                                Some(false) => "not joined",
                                None => "not reported",
                            }
                        )));
                        ui.label(widgets::dim(format!(
                            "Windows Update API reports: {}",
                            match snapshot.environment.update_api_level {
                                Some(0) => "not configured",
                                Some(1) => "automatic updates disabled",
                                Some(2) => "notify before download",
                                Some(3) => "notify before installation",
                                Some(4) => "scheduled installation",
                                _ => "unavailable or unrecognized",
                            }
                        )));
                        ui.label(widgets::faint("This is a read-only report from the legacy Windows Update API. It does not verify modern Windows Update orchestration or policy enforcement."));
                        ui.label(widgets::dim(if snapshot.environment.pending_restart {
                            "Windows reports a pending update or servicing restart. Save work and review the restart in Windows Settings."
                        } else {
                            "The checked update/servicing indicators do not currently report a pending restart. This does not cover every installer."
                        }));
                        ui.horizontal_wrapped(|ui| {
                            for (label, uri) in [
                                ("Pause / resume updates", "ms-settings:windowsupdate"),
                                ("Active hours and restart preferences", "ms-settings:windowsupdate-options"),
                                ("Installed update history", "ms-settings:windowsupdate-history"),
                            ] {
                                if ui.add(widgets::small(label)).clicked() {
                                    util::open_uri(uri);
                                }
                            }
                        });
                        ui.label(widgets::faint("Pause duration and available controls depend on your Windows build. These buttons open native controls; PCSweep does not claim a pause was applied."));
                    });
                    widgets::section(ui, "Configured Windows Update policy");
                    ui.label(widgets::dim("Policy values below are read from Windows. Effective enforcement is not independently verified; Windows processes policies during its normal refresh. Existing organization policy is protected."));
                    if let Err(e) = &snapshot.policy_guard {
                        ui.colored_label(WARN, e);
                    }
                    if !self.is_admin {
                        ui.label(widgets::dim("Restart as administrator to apply or restore supported update policies."));
                    }
                    for (id, label) in [
                        ("no_auto", "Automatic update policy"),
                        ("download_mode", "Download policy"),
                        ("drivers", "Exclude drivers from quality updates"),
                    ] {
                        if let Some((_, value)) = snapshot.values.iter().find(|(key, _)| key == id)
                        {
                            let text = match value {
                                Ok(None) => "Not configured".into(),
                                Ok(Some(v)) => match v.number() {
                                    Some(n) => format!(
                                        "Configured value {n}{}",
                                        match (id, n) {
                                            ("no_auto", 1) => " · manual updates",
                                            ("no_auto", 0) => " · automatic updates allowed",
                                            ("download_mode", 2) => " · notify before download",
                                            ("drivers", 1) => " · driver exclusion requested",
                                            _ => "",
                                        }
                                    ),
                                    None => "Unrecognized value · review in Windows".into(),
                                },
                                Err(e) => format!("Unavailable: {e}"),
                            };
                            ui.label(widgets::dim(format!("{label}: {text}")));
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        for (control, label) in [
                            ("manual", "Review manual update mode…"),
                            ("notify", "Review notify before download…"),
                            ("exclude_drivers", "Review driver exclusion…"),
                        ] {
                            if let Ok(changes) = desired(control) {
                                if let Ok(values) = snapshot.values_for(&changes) {
                                    if let Ok(expected) = fingerprint(&values) {
                                        if ui
                                            .add_enabled(
                                                !busy
                                                    && self.is_admin
                                                    && snapshot.policy_guard.is_ok()
                                                    && snapshot.machine_undo.is_ok(),
                                                widgets::small(label),
                                            )
                                            .clicked()
                                        {
                                            action = Some(WindowsControlsAction::Set {
                                                control: control.into(),
                                                expected,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    });
                    ui.label(widgets::faint("Manual mode stops automatic Windows Update downloads/installations on supported unmanaged editions; you must check manually for security updates. Other update channels may behave differently. No services, recovery tasks or update endpoints are disabled."));
                    undo_controls(
                        ui,
                        &snapshot.machine_undo,
                        !busy && self.is_admin,
                        true,
                        &mut action,
                    );
                    widgets::section(ui, "Responsiveness preferences");
                    ui.label(widgets::dim("Animation preferences can make transitions feel quicker. They do not guarantee faster application throughput. Font smoothing, security and accessibility settings stay available."));
                    for (id, label, off, on) in [
                        (
                            "client_animation",
                            "Client-area animations",
                            "client_off",
                            "client_on",
                        ),
                        (
                            "window_animation",
                            "Minimize / maximize animations",
                            "window_off",
                            "window_on",
                        ),
                    ] {
                        widgets::card(ui, |ui| {
                            ui.label(widgets::strong(label, 15.0));
                            let current = snapshot
                                .values
                                .iter()
                                .find(|(key, _)| key == id)
                                .and_then(|(_, v)| v.as_ref().ok())
                                .and_then(|v| v.as_ref())
                                .and_then(Value::number);
                            ui.label(widgets::dim(match current {
                                Some(0) => "Windows reports disabled",
                                Some(1) => "Windows reports enabled",
                                _ => "Windows did not report a recognized state",
                            }));
                            ui.horizontal_wrapped(|ui| {
                                for (control, label, wanted) in [
                                    (off, "Turn off…", 0),
                                    (on, "Turn on…", 1),
                                ] {
                                    if let Ok(changes) = desired(control) {
                                        if let Ok(values) = snapshot.values_for(&changes) {
                                            if let Ok(expected) = fingerprint(&values) {
                                                if ui
                                                    .add_enabled(
                                                        !busy
                                                            && current.is_some_and(|n| n != wanted)
                                                            && snapshot.user_undo.is_ok(),
                                                        widgets::small(label),
                                                    )
                                                    .clicked()
                                                {
                                                    action = Some(WindowsControlsAction::Set {
                                                        control: control.into(),
                                                        expected,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                        });
                    }
                    undo_controls(ui, &snapshot.user_undo, !busy, false, &mut action);
                    ui.horizontal_wrapped(|ui| {
                        for (label, uri) in [
                            ("Transparency and visual effects", "ms-settings:easeofaccess-visualeffects"),
                            ("Search indexing scope", "ms-settings:search"),
                            ("Startup apps", "ms-settings:startupapps"),
                            ("Game Mode", "ms-settings:gaming-gamemode"),
                            ("Recording and captures", "ms-settings:gaming-gamedvr"),
                        ] {
                            if ui.add(widgets::small(label)).clicked() {
                                util::open_uri(uri);
                            }
                        }
                    });
                    ui.label(widgets::faint("Review generated build/cache folders in Search settings if indexing is busy. Settings availability varies by Windows version. PCSweep does not apply a blanket service, timer, RAM-purge or security-disable preset."));
                }
                widgets::section(ui, "What changed on my PC?");
                ui.label(widgets::dim("Read local Windows update and reliability records. Timing can help an investigation; it does not prove an update caused a crash. Sources may have gaps or be unavailable."));
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.windows_controls.history.running(),
                            widgets::small("Read local change history"),
                        )
                        .clicked()
                    {
                        self.windows_controls.history = Task::spawn(&ctx, timeline);
                    }
                    if self.windows_controls.history.running() {
                        ui.spinner();
                    }
                    widgets::search_box(
                        ui,
                        &mut self.windows_controls.history_filter,
                        "Search recorded changes",
                        250.0,
                    );
                });
                if let Some(history) = &self.windows_controls.timeline {
                    for warning in &history.warnings {
                        ui.colored_label(WARN, warning);
                    }
                    let query = self.windows_controls.history_filter.to_lowercase();
                    let rows: Vec<_> = history
                        .events
                        .iter()
                        .filter(|e| {
                            query.is_empty()
                                || format!("{} {} {}", e.time, e.source, e.description)
                                    .to_lowercase()
                                    .contains(&query)
                        })
                        .collect();
                    ui.label(widgets::faint(format!("{} records shown · latest 40 update records and up to 40 reliability records within 30 days", rows.len())));
                    for event in rows {
                        widgets::tight_frame().show(ui, |ui| {
                            ui.label(widgets::strong(
                                format!("{} · {}", event.time, event.source),
                                14.0,
                            ));
                            ui.add(egui::Label::new(widgets::dim(&event.description)).wrap());
                        });
                    }
                    if history.events.is_empty() {
                        ui.label(widgets::dim("No records were returned by the available sources. This does not prove that nothing changed."));
                    }
                }
            });
        if refresh {
            self.windows_controls.error = None;
            self.refresh_windows_controls(&ctx);
        }
        if let Some(action) = action {
            let (title, body, danger) = match &action {
                WindowsControlsAction::Set { control, .. } if control == "manual" => {
                    ("Use manual Windows updates?", "This requests disabled automatic Windows Update through documented policy. You will need to check and install security updates manually. Other update channels are not guaranteed to follow this policy. The original values are saved for Undo.", true)
                }
                WindowsControlsAction::Set { control, .. } if control == "notify" => {
                    ("Notify before downloading updates?", "This requests the documented notification-before-download policy and allows automatic updates to use that mode. Windows policy refresh determines effective behavior; existing unowned policy will be preserved. The reviewed original values are saved for Undo.", false)
                }
                WindowsControlsAction::Set { control, .. } if control == "exclude_drivers" => {
                    ("Exclude drivers from quality updates?", "This requests Windows' supported driver-exclusion policy. It does not block every driver installation channel or update already in progress. Keep manufacturer driver updates under review. Undo restores the saved policy value.", false)
                }
                WindowsControlsAction::Set { .. } => {
                    ("Change this animation preference?", "Windows will update the reviewed animation preference for this account using its supported system API. Existing values are saved for Undo. This changes visual transitions, not application throughput.", false)
                }
                WindowsControlsAction::Discard { .. } => ("Dismiss saved Windows Undo?", "Keep the current Windows values and dismiss the saved original preferences. They will no longer be offered as Undo. This does not change Windows settings.", true),
                WindowsControlsAction::Undo { .. } => {
                    ("Restore saved Windows preferences?", "Restore only the values PCSweep changed. Changes made outside PCSweep are kept for review, and unrelated preferences stay as they are.", false)
                }
            };
            self.modal = Some(Modal {
                title: title.into(),
                body: body.into(),
                details: Vec::new(),
                note: Some("Configured policy readback does not prove immediate effective enforcement. Managed or unsupported Windows configurations are protected.".into()),
                confirm: if matches!(action, WindowsControlsAction::Discard { .. }) { "Keep current and dismiss Undo".into() } else if matches!(action, WindowsControlsAction::Undo { .. }) {
                    "Restore preferences".into()
                } else {
                    "Apply reviewed change".into()
                },
                danger,
                action: Action::WindowsControls(action),
            });
        }
    }
}
fn undo_controls(
    ui: &mut Ui,
    entries: &Result<Vec<Entry>, String>,
    enabled: bool,
    system: bool,
    action: &mut Option<WindowsControlsAction>,
) {
    match entries {
        Ok(entries) if !entries.is_empty() => {
            if ui
                .add_enabled(
                    enabled,
                    widgets::ghost(if system {
                        "Undo PCSweep update policy"
                    } else {
                        "Undo animation changes"
                    }),
                )
                .clicked()
            {
                *action = Some(WindowsControlsAction::Undo { system });
            }
            if let Ok(expected) = entries_fingerprint(entries) {
                if ui
                    .add_enabled(enabled, widgets::small("Dismiss saved Undo…"))
                    .clicked()
                {
                    *action = Some(WindowsControlsAction::Discard { system, expected });
                }
            }
        }
        Err(e) => {
            ui.colored_label(WARN, e);
        }
        _ => {}
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> WindowsControlsState {
    WindowsControlsState { loaded: true, snapshot: Some(Snapshot { environment: Environment { edition: "Professional".into(), build: 26100, domain: Some(false), pending_restart: false, update_api_level: Some(4) }, policy_guard: Ok(()), values: IDS.iter().map(|id| (id.to_string(), Ok(if machine(id) { None } else { Some(Value::dword(1)) }))).collect(), machine_undo: Ok(Vec::new()), user_undo: Ok(Vec::new()) }), timeline: Some(Timeline { events: vec![Event { time: "2026-11-15 10:30:00".into(), source: "Windows Update".into(), description: "Example recorded cumulative update. History is local and does not establish the cause of a later issue.".into() }], warnings: Vec::new() }), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    #[derive(Default)]
    struct Fake {
        values: HashMap<String, Option<Value>>,
        entries: Vec<Entry>,
        fail_save: bool,
        fail_write: Option<String>,
        writes: usize,
    }
    impl Backend for Fake {
        fn read(&mut self, id: &str) -> Result<Option<Value>, String> {
            Ok(self.values.get(id).cloned().unwrap_or(None))
        }
        fn write(&mut self, id: &str, value: &Option<Value>) -> Result<(), String> {
            if self.fail_write.as_deref() == Some(id) {
                return Err("Injected Windows refusal".into());
            }
            self.writes += 1;
            self.values.insert(id.into(), value.clone());
            Ok(())
        }
        fn load(&mut self) -> Result<Vec<Entry>, String> {
            Ok(self.entries.clone())
        }
        fn save(&mut self, entries: &[Entry]) -> Result<(), String> {
            if self.fail_save {
                return Err("Injected persistence failure".into());
            }
            self.entries = entries.into();
            Ok(())
        }
    }
    fn expected(backend: &mut Fake, changes: &[(String, Option<Value>)]) -> String {
        fingerprint(
            &changes
                .iter()
                .map(|(id, _)| (id.clone(), backend.read(id).unwrap()))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }
    #[test]
    fn dismiss_undo_preserves_values_and_refuses_changed_record() {
        let mut fake = Fake::default();
        let changes = desired("manual").unwrap();
        let reviewed = expected(&mut fake, &changes);
        transaction(&mut fake, &changes, &reviewed).unwrap();
        let reviewed = entries_fingerprint(&fake.load().unwrap()).unwrap();
        let writes = fake.writes;
        assert!(discard_transaction(&mut fake, "wrong").is_err());
        discard_transaction(&mut fake, &reviewed).unwrap();
        assert!(fake.load().unwrap().is_empty());
        assert_eq!(fake.writes, writes);
        assert_eq!(fake.read("no_auto").unwrap(), Some(Value::dword(1)));
    }
    #[test]
    fn durable_undo_precedes_writes_and_partial_policy_change_is_recoverable() {
        let mut fake = Fake::default();
        let changes = desired("notify").unwrap();
        let expected = expected(&mut fake, &changes);
        fake.fail_save = true;
        assert!(transaction(&mut fake, &changes, &expected).is_err());
        assert_eq!(fake.writes, 0);
        fake.fail_save = false;
        fake.fail_write = Some("download_mode".into());
        assert!(transaction(&mut fake, &changes, &expected).is_err());
        assert_eq!(fake.writes, 1);
        assert_eq!(fake.entries.len(), 2);
        fake.fail_write = None;
        undo_transaction(&mut fake).unwrap();
        assert!(fake.entries.is_empty());
        assert_eq!(fake.read("no_auto").unwrap(), None);
    }
    #[test]
    fn outside_changes_and_preexisting_policies_are_kept() {
        let mut fake = Fake::default();
        let changes = desired("manual").unwrap();
        let reviewed = expected(&mut fake, &changes);
        fake.values.insert("no_auto".into(), Some(Value::dword(0)));
        assert!(transaction(&mut fake, &changes, &reviewed).is_err());
        let reviewed = expected(&mut fake, &changes);
        assert!(transaction(&mut fake, &changes, &reviewed).is_err());
        assert_eq!(fake.writes, 0);
        fake.values.clear();
        let reviewed = expected(&mut fake, &changes);
        transaction(&mut fake, &changes, &reviewed).unwrap();
        fake.values.insert("no_auto".into(), Some(Value::dword(7)));
        assert!(undo_transaction(&mut fake).is_err());
        assert_eq!(fake.read("no_auto").unwrap(), Some(Value::dword(7)));
    }
    #[test]
    fn journal_scope_and_animation_values_are_strict() {
        let entries = vec![Entry {
            id: "arbitrary".into(),
            original: None,
            applied: None,
            pending: None,
        }];
        assert!(validate_entries(&entries, true).is_err());
        assert!(desired("disable_defender").is_err());
        let mut fake = Fake::default();
        fake.values
            .insert("client_animation".into(), Some(Value::dword(1)));
        let off = desired("client_off").unwrap();
        let reviewed = expected(&mut fake, &off);
        transaction(&mut fake, &off, &reviewed).unwrap();
        undo_transaction(&mut fake).unwrap();
        assert_eq!(
            fake.read("client_animation").unwrap(),
            Some(Value::dword(1))
        );
    }
    #[test]
    fn scripts_parse_and_native_preference_reads_do_not_change_windows() {
        for script in [ENVIRONMENT_SCRIPT, TIMELINE_SCRIPT] {
            let check = format!("$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors);if($errors.Count){{throw ($errors|Out-String)}}", platform::ps_quote(script));
            platform::powershell(&check, Duration::from_secs(15)).unwrap();
        }
        let _ = animation("client_animation", None);
        let _ = animation("window_animation", None);
        let env = environment().unwrap();
        assert!(env.build > 0);
    }
}
