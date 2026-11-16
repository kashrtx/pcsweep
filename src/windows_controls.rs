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
    "defer_quality",
    "defer_feature",
    "pause_quality",
    "pause_feature",
    "client_animation",
    "window_animation",
];
const PAUSE_IDS: &[&str] = &[
    "defer_quality",
    "defer_feature",
    "pause_quality",
    "pause_feature",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Value {
    kind: u32,
    bytes: Vec<u8>,
}
impl Value {
    fn text(value: &str) -> Self {
        Self {
            kind: REG_SZ as u32,
            bytes: value
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect(),
        }
    }
    fn string(&self) -> Option<String> {
        if self.kind != REG_SZ as u32
            || self.bytes.len() < 2
            || self.bytes.len() > 128
            || self.bytes.len() % 2 != 0
        {
            return None;
        }
        let mut units: Vec<_> = self
            .bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        if units.pop() != Some(0) || units.contains(&0) {
            return None;
        }
        String::from_utf16(&units).ok()
    }
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
        if !((self.kind == REG_DWORD as u32 && self.bytes.len() == 4) || self.string().is_some()) {
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
    #[serde(default)]
    applied_at: Option<i64>,
}
impl Entry {
    fn owns(&self, current: &Option<Value>) -> bool {
        current == &self.original
            || current == &self.applied
            || self.pending.as_ref() == Some(current)
    }
}
fn machine(id: &str) -> bool {
    matches!(
        id,
        "no_auto"
            | "download_mode"
            | "drivers"
            | "defer_quality"
            | "defer_feature"
            | "pause_quality"
            | "pause_feature"
    )
}
fn target(id: &str) -> Result<(&'static str, &'static str), String> {
    match id {
        "no_auto" => Ok((AU, "NoAutoUpdate")),
        "download_mode" => Ok((AU, "AUOptions")),
        "drivers" => Ok((UPDATE, "ExcludeWUDriversInQualityUpdate")),
        "defer_quality" => Ok((UPDATE, "DeferQualityUpdates")),
        "defer_feature" => Ok((UPDATE, "DeferFeatureUpdates")),
        "pause_quality" => Ok((UPDATE, "PauseQualityUpdatesStartTime")),
        "pause_feature" => Ok((UPDATE, "PauseFeatureUpdatesStartTime")),
        _ => Err("Unknown Windows policy target".into()),
    }
}
fn validate_target(id: &str, value: &Value) -> Result<(), String> {
    value.validate()?;
    if matches!(id, "pause_quality" | "pause_feature") {
        let date = value
            .string()
            .ok_or("The pause date has an unsupported type; it was preserved.")?;
        if date.is_empty()
            || (date.len() == 10 && chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok())
        {
            return Ok(());
        }
    } else if let Some(number) = value.number() {
        let recognized = if id == "download_mode" {
            (2..=5).contains(&number)
        } else {
            number <= 1
        };
        if recognized {
            return Ok(());
        }
    }
    Err("An unrecognized Windows preference was preserved. No change was applied.".into())
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
                validate_target(&e.id, value)?;
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
                vtype: if v.kind == REG_SZ as u32 {
                    REG_SZ
                } else {
                    REG_DWORD
                },
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
    if id == "pause" {
        return pause_values(chrono::Utc::now().date_naive());
    }
    let pairs: &[(&str, u32)] = match id {
        "manual" => &[("no_auto", 1)],
        "notify" => &[("no_auto", 0), ("download_mode", 2)],
        "automatic" => &[("no_auto", 0), ("download_mode", 4)],
        "exclude_drivers" => &[("drivers", 1)],
        "include_drivers" => &[("drivers", 0)],
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
fn pause_values(start: chrono::NaiveDate) -> Result<Vec<(String, Option<Value>)>, String> {
    let date = start.format("%Y-%m-%d").to_string();
    Ok(vec![
        ("defer_quality".into(), Some(Value::dword(1))),
        ("defer_feature".into(), Some(Value::dword(1))),
        ("pause_quality".into(), Some(Value::text(&date))),
        ("pause_feature".into(), Some(Value::text(&date))),
    ])
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
    for (id, value) in &current {
        if let Some(value) = value {
            validate_target(id, value)?;
        }
    }
    for (id, value) in changes {
        if let Some(value) = value {
            validate_target(id, value)?;
        }
    }
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
            entries.push(Entry {
                id: id.clone(),
                original: current.clone(),
                applied: current.clone(),
                pending: Some(value.clone()),
                applied_at: None,
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
        if e.applied != *value {
            e.applied_at = Some(chrono::Utc::now().timestamp());
        }
        e.applied = value.clone();
        e.pending = None;
    }
    backend.save(&entries)?;
    Ok("Configured values saved and read back. Undo is available. Windows policy refresh and other system signals determine effective behavior.".into())
}
fn undo_transaction(backend: &mut impl Backend) -> Result<String, String> {
    undo_group(backend, None)
}
fn undo_group(backend: &mut impl Backend, ids: Option<&[&str]>) -> Result<String, String> {
    let mut entries = backend.load()?;
    let mut failures = Vec::new();
    for entry in entries.clone() {
        if ids.is_some_and(|ids| !ids.contains(&entry.id.as_str())) {
            continue;
        }
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

#[derive(Clone, Debug, Default, Serialize)]
struct ObservedPause {
    quality: Option<u32>,
    feature: Option<u32>,
    quality_date: Option<String>,
    feature_date: Option<String>,
}
fn observed_pause() -> Result<ObservedPause, String> {
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\WindowsUpdate\UpdatePolicy\Settings")
    {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ObservedPause::default()),
        Err(e) => return Err(format!("Windows pause status is unavailable: {e}")),
    };
    let number = |name| -> Result<Option<u32>, String> {
        match key.get_value::<u32, _>(name) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Windows did not report a recognized {name}: {e}")),
        }
    };
    // Dates are context only. Never invent them from configured policy when
    // Windows has not provided a recognized observed date.
    Ok(ObservedPause {
        quality: number("PausedQualityStatus")?,
        feature: number("PausedFeatureStatus")?,
        quality_date: key.get_value("PausedQualityDate").ok(),
        feature_date: key.get_value("PausedFeatureDate").ok(),
    })
}
fn pause_status(value: Option<u32>) -> &'static str {
    match value {
        Some(0) => "Not paused",
        Some(1) => "Paused",
        Some(2) => "Automatically resumed",
        _ => "Not reported",
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct UpdateAge {
    installed: Option<String>,
    title: Option<String>,
    identity: Option<String>,
    warning: Option<String>,
}
const UPDATE_AGE_SCRIPT: &str = r#"
$r=[ordered]@{installed=$null;title=$null;identity=$null;warning=$null}
try {
 $session=New-Object -ComObject Microsoft.Update.Session
 $searcher=$session.CreateUpdateSearcher();$searcher.Online=$false
 $count=[Math]::Min(2000,$searcher.GetTotalHistoryCount());$seen=@{}
 if($count -gt 0){
  foreach($h in @($searcher.QueryHistory(0,$count)|Sort-Object Date -Descending)){
   if([int]$h.ResultCode -ne 2){continue}
   $id=[string]$h.UpdateIdentity.UpdateID
   if([string]::IsNullOrWhiteSpace($id) -or $seen.ContainsKey($id)){continue};$seen[$id]=$true
   if([int]$h.Operation -ne 1){continue}
   $title=[string]$h.Title
   if($title -match '(?i)(?:Cumulative|Security) Update(?:\s+\([^)]*\))?\s+for\s+(?:Microsoft\s+)?Windows\b' -and $title -notmatch '(?i)\.NET|Defender|Intelligence|Malicious Software'){
    $r.installed=$h.Date.ToUniversalTime().ToString('o');$r.title=$title;$r.identity=$id;break
   }
  }
 }
 if($null -eq $r.installed){$r.warning='No recognized successful Windows OS quality/security installation was found in the latest 2,000 local history entries. Localized or removed history may be unavailable.'}
} catch {$r.warning=$_.Exception.Message}
[pscustomobject]$r|ConvertTo-Json -Compress
"#;
fn update_age() -> UpdateAge {
    let result = platform::powershell(UPDATE_AGE_SCRIPT, Duration::from_secs(20))
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
        .unwrap_or_else(|e| UpdateAge {
            warning: Some(format!("Local installation age is unavailable: {e}")),
            ..Default::default()
        });
    if result
        .installed
        .as_deref()
        .and_then(|s| days_since(s, chrono::Utc::now()))
        .is_some()
    {
        if let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(REMINDERS) {
            if let Ok(text) = serde_json::to_string(&result) {
                let _ = key.set_value("LastRecordedInstallV1", &text);
            }
        }
        return result;
    }
    // WUA history may be unavailable while its service is disabled. Preserve
    // the last successfully observed installation, explicitly labelled cached.
    let cached = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(REMINDERS)
        .and_then(|k| k.get_value::<String, _>("LastRecordedInstallV1"))
        .ok()
        .filter(|s| s.len() <= 16384)
        .and_then(|s| serde_json::from_str::<UpdateAge>(&s).ok())
        .filter(|age| {
            age.installed
                .as_deref()
                .and_then(|s| days_since(s, chrono::Utc::now()))
                .is_some()
        });
    if let Some(mut cached) = cached {
        cached.warning = Some(format!("Showing the last cached installation record because current Windows update history is unavailable. {}", result.warning.unwrap_or_default()));
        cached
    } else {
        result
    }
}
fn days_since(stamp: &str, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()?
        .with_timezone(&chrono::Utc);
    (then <= now).then(|| now.signed_duration_since(then).num_days())
}
const REMINDERS: &str = r"Software\PCSweep\WindowsUpdateReminders";
#[derive(Clone, Debug, Default)]
struct ReminderPrefs {
    off: bool,
    dismissed: String,
    later: Option<i64>,
}
fn load_reminders() -> ReminderPrefs {
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(REMINDERS) else {
        return ReminderPrefs::default();
    };
    ReminderPrefs {
        off: key.get_value::<u32, _>("Off").ok() == Some(1),
        dismissed: key
            .get_value::<String, _>("Dismissed")
            .ok()
            .filter(|s| s.len() <= 128)
            .unwrap_or_default(),
        later: key
            .get_value::<u64, _>("Later")
            .ok()
            .and_then(|t| i64::try_from(t).ok()),
    }
}
fn save_reminders(token: &str, off: bool, later: Option<i64>) -> Result<String, String> {
    if token.len() > 128 || later.is_some_and(|v| v < 0) {
        return Err("Invalid reminder preference.".into());
    }
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(REMINDERS)
        .map_err(|e| e.to_string())?;
    key.set_value("Off", &u32::from(off))
        .map_err(|e| e.to_string())?;
    key.set_value("Dismissed", &token)
        .map_err(|e| e.to_string())?;
    key.set_value("Later", &(later.unwrap_or(0) as u64))
        .map_err(|e| e.to_string())?;
    Ok("Update reminder preference saved.".into())
}
fn reminder_hidden(prefs: &ReminderPrefs, token: &str, now: i64) -> bool {
    prefs.off || (prefs.dismissed == token && (prefs.later.is_none_or(|t| t == 0 || t > now)))
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
fn management_guard(environment: &Environment) -> Result<(), String> {
    if environment.build < 19045 || environment.edition.starts_with("Server") {
        return Err(
            "Direct update controls require a supported Windows 10/11 client installation.".into(),
        );
    }
    if environment.domain != Some(false)
        || present_key(r"SOFTWARE\Microsoft\Provisioning\OMADM\Accounts")?
    {
        return Err(
            "Windows may be organization-managed. PCSweep preserves administrator update settings."
                .into(),
        );
    }
    for (path, names) in [
        (
            UPDATE,
            &["WUServer", "WUStatusServer", "UpdateServiceUrlAlternate"][..],
        ),
        (AU, &["UseWUServer"][..]),
    ] {
        let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) {
            Ok(key) => key,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        for name in names {
            match key.get_raw_value(name) {
                Ok(raw) => {
                    if *name == "UseWUServer"
                        && raw.vtype == REG_DWORD
                        && raw.bytes == 0u32.to_le_bytes()
                    {
                        continue;
                    }
                    if *name != "UseWUServer" && raw.vtype == REG_SZ {
                        let value: String = key.get_value(name).map_err(|e| e.to_string())?;
                        if value.trim().is_empty() {
                            continue;
                        }
                    }
                    return Err("A managed update source is configured. Its administrator's update controls are preserved.".into());
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Ok(())
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
        return Err("Windows does not support these policy controls on this edition/build. The advanced service control below is a separate direct option.".into());
    }
    management_guard(environment)
}
struct Snapshot {
    environment: Environment,
    policy_guard: Result<(), String>,
    service_guard: Result<(), String>,
    values: Vec<(String, Result<Option<Value>, String>)>,
    machine_undo: Result<Vec<Entry>, String>,
    user_undo: Result<Vec<Entry>, String>,
    paused: Result<ObservedPause, String>,
    service: Result<crate::update_control::Snapshot, String>,
    update_age: UpdateAge,
    reminders: ReminderPrefs,
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
                    validate_target(id, v)?;
                }
                Ok((id.clone(), raw))
            })
            .collect()
    }
    fn reminder_context(&self) -> Option<(String, String, String)> {
        let manual = self
            .values
            .iter()
            .find(|(id, _)| id == "no_auto")
            .and_then(|(_, v)| v.as_ref().ok())
            .and_then(|v| v.as_ref())
            .and_then(Value::number)
            == Some(1);
        let disabled = self.service.as_ref().is_ok_and(|s| s.disabled);
        let paused = self
            .paused
            .as_ref()
            .is_ok_and(|s| s.quality == Some(1) || s.feature == Some(1));
        if !manual && !disabled && !paused {
            return None;
        }
        let since = self
            .service
            .as_ref()
            .ok()
            .filter(|s| s.disabled)
            .and_then(|s| s.since_disabled.as_deref())
            .and_then(|t| days_since(t, chrono::Utc::now()))
            .or_else(|| {
                self.machine_undo
                    .as_ref()
                    .ok()
                    .and_then(|entries| {
                        entries.iter().find(|e| {
                            e.id == "no_auto"
                                && e.applied.as_ref().and_then(Value::number) == Some(1)
                        })
                    })
                    .and_then(|e| e.applied_at)
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .filter(|t| *t <= chrono::Utc::now())
                    .map(|t| chrono::Utc::now().signed_duration_since(t).num_days())
            });
        let status = if disabled {
            "Windows Update service is disabled"
        } else if manual {
            "Automatic updates are set to manual"
        } else {
            "Windows reports updates paused"
        };
        let heading = match since {
            Some(n) => format!("{status} · {n} days since PCSweep's change"),
            None => status.into(),
        };
        let age = match self
            .update_age
            .installed
            .as_deref()
            .and_then(|t| days_since(t, chrono::Utc::now()))
        {
            Some(n) => format!(
                "Last recorded successful Windows OS quality/security installation: {n} days ago."
            ),
            None => "Last successful Windows OS quality/security installation age: unknown.".into(),
        };
        let service = self
            .service
            .as_ref()
            .ok()
            .map(|s| (&s.start_mode, &s.since_disabled));
        let values: Vec<_> = self
            .values
            .iter()
            .filter(|(id, _)| machine(id))
            .map(|(id, value)| (id, value.as_ref().ok()))
            .collect();
        let token = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    service,
                    values,
                    self.paused.as_ref().ok(),
                    &self.update_age.identity,
                    &self.update_age.installed
                ))
                .ok()?
            )
        );
        Some((token, heading, age))
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
        service_guard: management_guard(&environment),
        environment,
        values: IDS
            .iter()
            .map(|id| (id.to_string(), read_value(id)))
            .collect(),
        machine_undo: load_entries(true),
        user_undo: load_entries(false),
        paused: observed_pause(),
        service: crate::update_control::query(),
        update_age: update_age(),
        reminders: load_reminders(),
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
    Set {
        control: String,
        expected: String,
    },
    Undo {
        system: bool,
    },
    Discard {
        system: bool,
        expected: String,
    },
    Resume {
        expected: String,
    },
    DisableService {
        expected: String,
    },
    EnableService {
        expected: String,
    },
    RestoreService {
        expected: String,
        restores_disabled: bool,
    },
    ForgetService {
        expected: String,
    },
    Reminder {
        token: String,
        off: bool,
        later: Option<i64>,
    },
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
    if let WindowsControlsAction::Reminder { token, off, later } = &action {
        return save_reminders(token, *off, *later);
    }
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
        WindowsControlsAction::Resume { expected } => {
            policy_guard(&environment()?)?;
            let mut backend = WindowsBackend(true);
            let current = PAUSE_IDS
                .iter()
                .map(|id| backend.read(id).map(|v| ((*id).to_string(), v)))
                .collect::<Result<Vec<_>, _>>()?;
            if fingerprint(&current)? != expected {
                return Err(
                    "The pause policy changed after review. Refresh before resuming.".into(),
                );
            }
            if !backend
                .load()?
                .iter()
                .any(|e| PAUSE_IDS.contains(&e.id.as_str()))
            {
                return Err("There is no saved PCSweep pause to resume.".into());
            }
            undo_group(&mut backend, Some(PAUSE_IDS))
        }
        WindowsControlsAction::DisableService { expected } => {
            crate::update_control::disable(expected)
        }
        WindowsControlsAction::EnableService { expected } => {
            crate::update_control::enable(expected)
        }
        WindowsControlsAction::RestoreService { expected, .. } => {
            crate::update_control::restore(expected)
        }
        WindowsControlsAction::ForgetService { expected } => {
            crate::update_control::forget(expected)
        }
        WindowsControlsAction::Reminder { token, off, later } => save_reminders(&token, off, later),
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
    last_refresh: Option<std::time::Instant>,
}
impl WindowsControlsState {
    pub fn busy(&self) -> bool {
        self.change.running()
    }
}

fn policy_button(
    ui: &mut Ui,
    snapshot: &Snapshot,
    control: &str,
    label: &str,
    enabled: bool,
    action: &mut Option<WindowsControlsAction>,
) {
    let reviewed = desired(control)
        .and_then(|changes| snapshot.values_for(&changes))
        .and_then(|values| fingerprint(&values));
    let response = ui.add_enabled(enabled && reviewed.is_ok(), widgets::small(label));
    if response.clicked() {
        if let Ok(expected) = reviewed {
            *action = Some(WindowsControlsAction::Set {
                control: control.into(),
                expected,
            });
        }
    } else if let Err(reason) = reviewed {
        response.on_disabled_hover_text(reason);
    }
}

fn render_update_controls(
    ui: &mut Ui,
    snapshot: &Snapshot,
    busy: bool,
    admin: bool,
    action: &mut Option<WindowsControlsAction>,
) {
    widgets::card(ui, |ui| {
        ui.label(widgets::strong(
            format!(
                "Windows {} · build {}",
                snapshot.environment.edition, snapshot.environment.build
            ),
            17.0,
        ));
        if snapshot.environment.pending_restart {
            ui.colored_label(WARN, "Windows reports an update or servicing restart pending. A new pause does not undo work already installing.");
        }
        if let Some((token, heading, age)) = snapshot.reminder_context() {
            if !reminder_hidden(&snapshot.reminders, &token, chrono::Utc::now().timestamp()) {
                ui.colored_label(WARN, heading);
                ui.label(widgets::dim(age));
                ui.label(widgets::faint("Installation age is not a check for newly available updates. You choose when to resume; PCSweep does not turn updates back on for you."));
                ui.horizontal_wrapped(|ui| {
                    for (label, later, off) in [
                        ("Dismiss this reminder", None, false),
                        (
                            "Remind me in 7 days",
                            Some(chrono::Utc::now().timestamp() + 7 * 86400),
                            false,
                        ),
                        ("Don't show update reminders", None, true),
                    ] {
                        if ui.add_enabled(!busy, widgets::small(label)).clicked() {
                            *action = Some(WindowsControlsAction::Reminder {
                                token: token.clone(),
                                off,
                                later,
                            });
                        }
                    }
                });
            }
        }
        if let Some(title) = &snapshot.update_age.title {
            ui.label(widgets::faint(format!(
                "Last identified installation: {title}"
            )));
        }
        if let Some(warning) = &snapshot.update_age.warning {
            egui::CollapsingHeader::new("Installation record details").show(ui, |ui| {
                ui.label(widgets::faint(warning));
            });
        }
    });
    if let Err(reason) = &snapshot.policy_guard {
        ui.label(widgets::dim(reason));
    } else {
        let enabled = !busy && admin && snapshot.machine_undo.is_ok();
        widgets::section(ui, "Pause and resume");
        widgets::card(ui, |ui| {
            match &snapshot.paused {
                Ok(status) => {
                    ui.label(widgets::strong(
                        format!(
                            "Windows reports · Quality: {} · Features: {}",
                            pause_status(status.quality),
                            pause_status(status.feature)
                        ),
                        14.0,
                    ));
                    if let Some(date) = &status.quality_date {
                        ui.label(widgets::faint(format!(
                            "Quality pause date reported by Windows: {date}"
                        )));
                    }
                }
                Err(reason) => {
                    ui.label(widgets::dim(reason));
                }
            }
            for id in ["pause_quality", "pause_feature"] {
                if let Some(start) = snapshot
                    .values
                    .iter()
                    .find(|(key, _)| key == id)
                    .and_then(|(_, value)| value.as_ref().ok())
                    .and_then(|v| v.as_ref())
                    .and_then(Value::string)
                    .and_then(|v| chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d").ok())
                {
                    if let Some(end) = start.checked_add_days(chrono::Days::new(35)) {
                        ui.label(widgets::faint(format!(
                            "{} policy requested through {end}",
                            if id == "pause_quality" {
                                "Quality update"
                            } else {
                                "Feature update"
                            }
                        )));
                    }
                }
            }
            ui.horizontal_wrapped(|ui| {
                policy_button(ui, snapshot, "pause", "Pause for up to 35 days…", enabled, action);
                let own_pause = snapshot.machine_undo.as_ref().is_ok_and(|entries| entries.iter().any(|e| PAUSE_IDS.contains(&e.id.as_str())));
                let reviewed = desired("pause").and_then(|changes| snapshot.values_for(&changes)).and_then(|v| fingerprint(&v));
                if ui.add_enabled(enabled && own_pause && reviewed.is_ok(), widgets::small("Resume / restore saved pause…")).on_disabled_hover_text("Available after PCSweep applies a pause; restores the original pause values without changing other update controls.").clicked() {
                    if let Ok(expected) = reviewed { *action = Some(WindowsControlsAction::Resume { expected }); }
                }
            });
            ui.label(widgets::faint("Changes apply directly through Windows policy. Observed Windows status may take time to refresh. Pausing is temporary and does not cancel an installation already in progress."));
        });
        widgets::section(ui, "Automatic update behavior");
        let value = |id: &str| {
            snapshot
                .values
                .iter()
                .find(|(key, _)| key == id)
                .and_then(|(_, value)| value.as_ref().ok())
                .and_then(|v| v.as_ref())
                .and_then(Value::number)
        };
        ui.label(widgets::dim(format!(
            "Configured behavior: {}",
            match (value("no_auto"), value("download_mode")) {
                (Some(1), _) => "Manual — automatic updates turned off",
                (_, Some(2)) => "Notify before downloading",
                (Some(0), Some(4)) => "Automatic download and scheduled installation",
                _ => "Windows defaults or another recognized configuration",
            }
        )));
        ui.horizontal_wrapped(|ui| {
            for (control, label) in [
                ("automatic", "Automatic…"),
                ("notify", "Notify before download…"),
                ("manual", "Manual — turn automatic updates off…"),
            ] {
                policy_button(ui, snapshot, control, label, enabled, action);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (control, label) in [
                ("exclude_drivers", "Exclude driver updates…"),
                ("include_drivers", "Allow driver updates…"),
            ] {
                policy_button(ui, snapshot, control, label, enabled, action);
            }
        });
        ui.label(widgets::faint("Manual mode leaves Windows Update available for checks you initiate. Other update channels and updates already in progress are separate. Original reviewed values are saved for Undo."));
        undo_controls(ui, &snapshot.machine_undo, !busy && admin, true, action);
    }
    egui::CollapsingHeader::new("Advanced · turn the Windows Update service off")
        .default_open(snapshot.policy_guard.is_err())
        .show(ui, |ui| {
            ui.label(widgets::dim("A stronger direct control for Home and Pro. Disabling this service can prevent Windows Update checks and other updates that depend on it. Windows may repair or re-enable it later; this is not a permanent enforcement service."));
            if let Err(reason) = &snapshot.service_guard { ui.colored_label(WARN, reason); }
            match &snapshot.service {
                Ok(service) => {
                    ui.label(widgets::strong(format!("Windows Update service: {} · {}{}", service.start_mode, service.state, if service.delayed_auto { " · delayed auto-start flag" } else { "" }), 14.0));
                    if let Some(reason) = &service.warning { ui.colored_label(WARN, reason); }
                    if let Ok(expected) = service.fingerprint() {
                        let enabled = !busy && admin && snapshot.service_guard.is_ok();
                        ui.horizontal_wrapped(|ui| {
                            if ui.add_enabled(enabled && service.disable_available, widgets::small("Turn Windows Update service off…")).clicked() { *action = Some(WindowsControlsAction::DisableService { expected: expected.clone() }); }
                            if ui.add_enabled(enabled && service.enable_available, widgets::small("Enable service with Manual start…")).clicked() { *action = Some(WindowsControlsAction::EnableService { expected: expected.clone() }); }
                            if ui.add_enabled(enabled && service.restore_available, widgets::small(if service.undo_restores_disabled { "Undo service enabling — turn off…" } else { "Restore Windows Update service…" })).clicked() { *action = Some(WindowsControlsAction::RestoreService { expected: expected.clone(), restores_disabled: service.undo_restores_disabled }); }
                            if service.undo_available && !service.restore_available && ui.add_enabled(!busy && admin, widgets::small("Forget stale service Undo…")).clicked() { *action = Some(WindowsControlsAction::ForgetService { expected }); }
                        });
                    }
                }
                Err(reason) => { ui.colored_label(WARN, reason); }
            }
        });
    egui::CollapsingHeader::new("Update reminder preferences").show(ui, |ui| {
        ui.label(widgets::faint("Reminders stay inside this page. No popups, sounds, or forced re-enabling. Dismissal lasts until the update state or identified installation changes."));
        let mut on = !snapshot.reminders.off;
        if ui.add_enabled(!busy, egui::Checkbox::new(&mut on, "Show quiet update reminders")).changed() { *action = Some(WindowsControlsAction::Reminder { token: String::new(), off: !on, later: None }); }
    });
}

impl App {
    pub(crate) fn refresh_windows_controls(&mut self, ctx: &egui::Context) {
        if !self.windows_controls.load.running() && !self.windows_controls.change.running() {
            self.windows_controls.load = Task::spawn(ctx, query);
            self.windows_controls.last_refresh = Some(std::time::Instant::now());
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
                Ok(snapshot) => {
                    if let Some(motion) = snapshot
                        .values
                        .iter()
                        .find(|(id, _)| id == "client_animation")
                        .and_then(|(_, v)| v.as_ref().ok())
                        .and_then(|v| v.as_ref())
                        .and_then(Value::number)
                    {
                        crate::theme::set_motion(ctx, motion != 0);
                    }
                    self.windows_controls.snapshot = Some(snapshot);
                }
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
        if self.windows_controls.loaded
            && self
                .windows_controls
                .last_refresh
                .is_some_and(|t| t.elapsed() > Duration::from_secs(300))
        {
            self.refresh_windows_controls(ctx);
        }
    }
    pub fn page_windows_controls(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.windows_controls.loaded && !self.windows_controls.load.running() {
            self.refresh_windows_controls(&ctx);
        }
        let mut refresh = false;
        let mut action = None;
        let mut elevate = false;
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
                    if !self.is_admin && ui.add_enabled(!busy, widgets::small("Restart as administrator to change updates")).clicked() { elevate = true; }
                });
                if let Some(e) = &self.windows_controls.error {
                    ui.colored_label(WARN, e);
                }
                if let Some(m) = &self.windows_controls.message {
                    ui.label(widgets::dim(m));
                }
                if let Some(snapshot) = &self.windows_controls.snapshot {
                    render_update_controls(ui, snapshot, busy, self.is_admin, &mut action);
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
                }
                self.render_direct_preferences(ui, crate::preferences::Section::Performance);
                if ui.add(widgets::small("Manage startup apps in PCSweep")).clicked() { self.page = crate::app::Page::Startup; }
                egui::CollapsingHeader::new("Advanced Windows update and indexing details").show(ui, |ui| {
                    ui.label(widgets::faint("Optional Windows interfaces for detailed update installation, active hours, and individual indexed folders."));
                    ui.horizontal_wrapped(|ui| {
                        for (label, uri) in [("Windows Update details", "ms-settings:windowsupdate"), ("Active hours", "ms-settings:windowsupdate-options"), ("Index individual folders", "ms-settings:search")] {
                            if ui.add(widgets::small(label)).clicked() { util::open_uri(uri); }
                        }
                    });
                });
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
            self.refresh_preferences(&ctx);
        }
        if elevate {
            self.relaunch_admin(&ctx);
        }
        if let Some(action) = action {
            if matches!(action, WindowsControlsAction::Reminder { .. }) {
                self.execute_windows_controls(action, &ctx);
                return;
            }
            let (title, body, danger) = match &action {
                WindowsControlsAction::Set { control, .. } if control == "manual" => {
                    ("Use manual Windows updates?", "Turn off automatic Windows Update through policy. You will need to check and install security updates manually. This changes the current reviewed local policy values and saves them exactly for Undo. It does not stop an installation already in progress or control every update channel.", true)
                }
                WindowsControlsAction::Set { control, .. } if control == "notify" => {
                    ("Notify before downloading updates?", "Set the reviewed local policy to ask before downloading. Existing recognized values are saved exactly for Undo. Windows processes policy during its normal refresh. This does not cancel updates already in progress.", false)
                }
                WindowsControlsAction::Set {control,..} if control=="automatic" => ("Enable automatic Windows updates?","Allow automatic downloads and scheduled installation using Windows Update policy. Existing reviewed values are saved exactly for Undo. A separate service block must be restored before Windows can run updates.",false),
                WindowsControlsAction::Set {control,..} if control=="pause" => ("Pause Windows updates for up to 35 days?","Apply the supported feature and quality update pause policies starting today. Windows resumes after 35 days unless you resume earlier. Current recognized policy values are saved exactly for Undo. Updates already installing and other update channels may continue. The app will show Windows' observed pause status separately from the requested policy.",false),
                WindowsControlsAction::Resume {..} => ("Resume the PCSweep update pause?","Restore the exact pause settings saved before PCSweep's change. Other update modes and service controls stay as they are. If a pause was already present before PCSweep, restoring it may keep that earlier pause active; the observed Windows status is shown after refresh.",false),
                WindowsControlsAction::DisableService {..} => ("Disable the Windows Update service?","Set the Windows Update service (wuauserv) to Disabled and ask it to stop. This is a stronger advanced control, available on Home as well as Pro. Windows Update checks and updates that depend on this service will stop working while it is disabled. Save ongoing work first; an installation already in progress may continue. PCSweep saves the exact current service mode and running state before changing anything. Windows can repair or re-enable the service later. No protected services, tasks, permissions or update endpoints are changed.",true),
                WindowsControlsAction::EnableService {..} => ("Enable the previously disabled Windows Update service?", "Change this stopped service from Disabled to Manual/trigger start, so Windows can start it when needed. PCSweep does not force it to start or change update policies or pauses. The current disabled state and delayed-start flag are saved exactly for Undo. This is an explicit choice for a pre-existing block, not a guessed restoration of Windows defaults.", false),
                WindowsControlsAction::RestoreService { restores_disabled: true, .. } => ("Undo service enabling and turn it off again?", "Restore the Disabled and Stopped state saved before you explicitly enabled this service. Windows Update checks will stop working again. Update policies and pauses stay unchanged; an outside service configuration change is preserved.", true),
                WindowsControlsAction::RestoreService {..} => ("Restore the Windows Update service?","Restore only the service start mode, delayed-start flag and running state saved before PCSweep's service change. Existing update policies and pauses stay unchanged. An outside change to the service configuration is preserved.",false),
                WindowsControlsAction::ForgetService {..} => ("Forget the stale service Undo?","Keep Windows' current service configuration and discard only the saved PCSweep service Undo that no longer matches it. This does not start, stop or configure a service.",true),
                WindowsControlsAction::Set { control, .. } if control == "exclude_drivers" => {
                    ("Exclude drivers from quality updates?", "This requests Windows' supported driver-exclusion policy. It does not block every driver installation channel or update already in progress. Keep manufacturer driver updates under review. Undo restores the saved policy value.", false)
                }
                WindowsControlsAction::Set { control, .. } if control == "include_drivers" => ("Allow drivers through Windows Update?", "Allow Windows Update to include driver updates in its normal quality-update checks. The previous reviewed policy value is saved for Undo.", false),
                WindowsControlsAction::Set { .. } => {
                    ("Change this animation preference?", "Windows will update the reviewed animation preference for this account using its supported system API. Existing values are saved for Undo. This changes visual transitions, not application throughput.", false)
                }
                WindowsControlsAction::Discard { .. } => ("Dismiss saved Windows Undo?", "Keep the current Windows values and dismiss the saved original preferences. They will no longer be offered as Undo. This does not change Windows settings.", true),
                WindowsControlsAction::Undo { .. } => {
                    ("Restore saved Windows preferences?", "Restore only the values PCSweep changed. Changes made outside PCSweep are kept for review, and unrelated preferences stay as they are.", false)
                }
                WindowsControlsAction::Reminder {..} => unreachable!(),
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
    WindowsControlsState { loaded: true, last_refresh: Some(std::time::Instant::now()), snapshot: Some(Snapshot {
        environment: Environment { edition: "Professional".into(), build: 26100, domain: Some(false), pending_restart: false, update_api_level: Some(4) },
        policy_guard: Ok(()), service_guard: Ok(()),
        values: IDS.iter().map(|id| (id.to_string(), Ok(if machine(id) { None } else { Some(Value::dword(1)) }))).collect(),
        machine_undo: Ok(Vec::new()), user_undo: Ok(Vec::new()),
        paused: Ok(ObservedPause { quality: Some(0), feature: Some(0), ..Default::default() }),
        service: Ok(crate::update_control::preview_fixture()),
        update_age: UpdateAge { installed: Some("2026-11-01T10:30:00Z".into()), title: Some("Example cumulative Windows OS update".into()), identity: Some("preview-update".into()), warning: None },
        reminders: Default::default(),
    }), timeline: Some(Timeline { events: vec![Event { time: "2026-11-15 10:30:00".into(), source: "Windows Update".into(), description: "Example recorded cumulative update. History is local and does not establish the cause of a later issue.".into() }], warnings: Vec::new() }), ..Default::default() }
}

#[cfg(test)]
pub(crate) fn preview_fixture_variant(
    home: bool,
    managed: bool,
    disabled: bool,
) -> WindowsControlsState {
    let mut state = preview_fixture();
    let s = state.snapshot.as_mut().unwrap();
    if home {
        s.environment.edition = "Core".into();
        s.policy_guard = Err("Policy controls require Pro; the advanced service control below works directly on Home.".into());
    }
    if managed {
        s.environment.domain = Some(true);
        s.policy_guard = Err("Organization-managed updates are preserved.".into());
        s.service_guard = Err("Organization-managed update services are preserved.".into());
    }
    if disabled {
        s.service = Ok(crate::update_control::preview_disabled_fixture());
    }
    state
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
    fn reviewed_existing_policies_are_reversible_and_outside_changes_are_kept() {
        let mut fake = Fake::default();
        let changes = desired("manual").unwrap();
        let reviewed = expected(&mut fake, &changes);
        fake.values.insert("no_auto".into(), Some(Value::dword(0)));
        assert!(transaction(&mut fake, &changes, &reviewed).is_err());
        let reviewed = expected(&mut fake, &changes);
        transaction(&mut fake, &changes, &reviewed).unwrap();
        assert_eq!(fake.entries[0].original, Some(Value::dword(0)));
        undo_transaction(&mut fake).unwrap();
        assert_eq!(fake.read("no_auto").unwrap(), Some(Value::dword(0)));
        let reviewed = expected(&mut fake, &changes);
        transaction(&mut fake, &changes, &reviewed).unwrap();
        fake.values.insert("no_auto".into(), Some(Value::dword(7)));
        assert!(undo_transaction(&mut fake).is_err());
        assert_eq!(fake.read("no_auto").unwrap(), Some(Value::dword(7)));
    }
    #[test]
    fn pause_has_required_parent_gates_and_scoped_resume_keeps_manual_mode() {
        let mut fake = Fake::default();
        let manual = desired("manual").unwrap();
        let fp = expected(&mut fake, &manual);
        transaction(&mut fake, &manual, &fp).unwrap();
        let start = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let pause = pause_values(start).unwrap();
        let fp = expected(&mut fake, &pause);
        transaction(&mut fake, &pause, &fp).unwrap();
        assert_eq!(fake.read("defer_quality").unwrap(), Some(Value::dword(1)));
        assert_eq!(fake.read("defer_feature").unwrap(), Some(Value::dword(1)));
        assert_eq!(
            fake.read("pause_quality")
                .unwrap()
                .unwrap()
                .string()
                .as_deref(),
            Some("2026-11-15")
        );
        assert_eq!(
            start
                .checked_add_days(chrono::Days::new(35))
                .unwrap()
                .to_string(),
            "2026-12-20"
        );
        undo_group(&mut fake, Some(PAUSE_IDS)).unwrap();
        for id in PAUSE_IDS {
            assert_eq!(fake.read(id).unwrap(), None);
        }
        assert_eq!(fake.read("no_auto").unwrap(), Some(Value::dword(1)));
        assert_eq!(fake.entries.len(), 1);
    }
    #[test]
    fn quiet_reminder_dismissal_is_stable_until_state_changes_and_can_be_disabled() {
        let now = 1_000_000;
        let mut prefs = ReminderPrefs {
            off: false,
            dismissed: "state-a".into(),
            later: None,
        };
        assert!(reminder_hidden(&prefs, "state-a", now));
        assert!(!reminder_hidden(&prefs, "state-b", now));
        prefs.later = Some(now + 7 * 86400);
        assert!(reminder_hidden(&prefs, "state-a", now));
        assert!(!reminder_hidden(&prefs, "state-a", now + 8 * 86400));
        prefs.off = true;
        assert!(reminder_hidden(&prefs, "any-new-state", now));
        assert!(days_since("not-a-date", chrono::Utc::now()).is_none());
        assert!(days_since("9999-01-01T00:00:00Z", chrono::Utc::now()).is_none());
    }
    #[test]
    fn journal_scope_and_animation_values_are_strict() {
        let entries = vec![Entry {
            id: "arbitrary".into(),
            original: None,
            applied: None,
            pending: None,
            applied_at: None,
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
        for script in [ENVIRONMENT_SCRIPT, TIMELINE_SCRIPT, UPDATE_AGE_SCRIPT] {
            let check = format!("$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors);if($errors.Count){{throw ($errors|Out-String)}}", platform::ps_quote(script));
            platform::powershell(&check, Duration::from_secs(15)).unwrap();
        }
        let _ = animation("client_animation", None);
        let _ = animation("window_animation", None);
        let env = environment().unwrap();
        assert!(env.build > 0);
    }
}
