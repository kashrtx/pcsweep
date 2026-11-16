//! Direct account preferences with fixed registry targets and durable exact Undo.
//!
//! Mappings: Microsoft Windows settings reference (settings-common and
//! settings-windows-11), Experience/Search/System Policy CSP. The Game DVR
//! background-recording preference is also read by OBS's Windows diagnostics.
//! Writing a preference is distinct from creating missing hardware, app support
//! or account consent; the UI explains those limits without silently redirecting.
use crate::{
    app::{Action, App, Modal, ToastKind},
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, Ui};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use winreg::{enums::*, RegKey, RegValue};

const USER_UNDO: &str = r"Software\PCSweep\DirectPreferencesUndo";
const MACHINE_UNDO: &str = r"SOFTWARE\PCSweep\DirectPreferencesUndo";
const SEARCH: &str = r"Software\Microsoft\Windows\CurrentVersion\SearchSettings";
const EXPLORER: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
const CLOUD_POLICY: &str = r"Software\Policies\Microsoft\Windows\CloudContent";
const SEARCH_POLICY: &str = r"Software\Policies\Microsoft\Windows\Windows Search";
const SYSTEM_POLICY: &str = r"Software\Policies\Microsoft\Windows\System";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Privacy,
    Performance,
}
struct Spec {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    key: &'static str,
    value: &'static str,
    section: Section,
    minimum: u32,
    machine: bool,
    // A version-dependent, undocumented setting is writable only if Windows
    // already published its recognized value. Known documented keys may be set
    // explicitly even when absent; absence is preserved by Undo.
    existing_only: bool,
    off: u32,
    on: u32,
    policy: &'static str,
    policy_values: &'static [&'static str],
    provider_category: &'static str,
    provider_values: &'static [&'static str],
}
const SPECS: &[Spec] = &[
    Spec {
        id: "transparency", name: "Transparency effects",
        description: "Use translucent Windows surfaces. Turning this off can make the interface feel simpler; apps may need to reopen to pick up the change.",
        key: r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", value: "EnableTransparency",
        section: Section::Performance, minimum: 19045, machine: false, existing_only: false, off: 0, on: 1,
        policy: r"Software\Microsoft\Windows\CurrentVersion\Policies\System", policy_values: &["NoDispAppearancePage"], provider_category: "", provider_values: &[],
    },
    Spec {
        id: "game_mode", name: "Game Mode",
        description: "Let Windows prioritize supported games. This changes the Game Mode preference, without promising higher frame rates or changing drivers.",
        key: r"Software\Microsoft\GameBar", value: "AutoGameModeEnabled",
        section: Section::Performance, minimum: 19045, machine: false, existing_only: false, off: 0, on: 1,
        policy: r"Software\Policies\Microsoft\Windows\GameDVR", policy_values: &[], provider_category: "", provider_values: &[],
    },
    Spec {
        id: "background_recording", name: "Background game recording",
        description: "Control Game Bar's Record what happened preference. Turning it off stops future background capture for this account; existing clips are kept. Game Bar may need to reopen. Turning it on still requires supported capture hardware and Game Bar permissions.",
        key: r"Software\Microsoft\Windows\CurrentVersion\GameDVR", value: "HistoricalCaptureEnabled",
        section: Section::Performance, minimum: 19045, machine: false, existing_only: false, off: 0, on: 1,
        policy: r"Software\Policies\Microsoft\Windows\GameDVR", policy_values: &["AllowGameDVR"], provider_category: "ApplicationManagement", provider_values: &["AllowGameDVR"],
    },
    Spec {
        id: "tailored_offers", name: "Personalized offers",
        description: "Allow Windows to use diagnostic data for personalized tips, recommendations and offers. Turning this off reduces personalization; separate advertising and suggestion controls still apply.",
        key: r"Software\Microsoft\Windows\CurrentVersion\Privacy", value: "TailoredExperiencesWithDiagnosticDataEnabled",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: false, off: 0, on: 1,
        policy: CLOUD_POLICY, policy_values: &["DisableTailoredExperiencesWithDiagnosticData", "DisableWindowsSpotlightFeatures"], provider_category: "Experience", provider_values: &["AllowTailoredExperiencesWithDiagnosticData", "AllowWindowsSpotlight"],
    },
    Spec {
        id: "clipboard_history", name: "Clipboard history",
        description: "Keep multiple copied items for Win+V. Turning this off disables history for this account. This does not erase pinned or cloud items; clearing the current clipboard is separate.",
        key: r"Software\Microsoft\Clipboard", value: "EnableClipboardHistory",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: false, off: 0, on: 1,
        policy: SYSTEM_POLICY, policy_values: &["AllowClipboardHistory"], provider_category: "Experience", provider_values: &["AllowClipboardHistory"],
    },
    Spec {
        id: "clipboard_sync", name: "Clipboard sync across devices",
        description: "Allow clipboard synchronization through your Microsoft account. Turning this off prevents future sync; it does not remove items already sent. Turning it on still requires account sign-in and Windows consent.",
        key: r"Software\Microsoft\Clipboard", value: "EnableCloudClipboard",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: false, off: 0, on: 1,
        policy: SYSTEM_POLICY, policy_values: &["AllowCrossDeviceClipboard"], provider_category: "Experience", provider_values: &["AllowCrossDeviceClipboard"],
    },
    Spec {
        id: "cloud_search_personal", name: "Search Microsoft account content",
        description: "Include this account's OneDrive and other Microsoft cloud content in Windows Search. This does not control web results. Available when Windows has published the account-search preference.",
        key: SEARCH, value: "IsMSACloudSearchEnabled",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: true, off: 0, on: 1,
        policy: SEARCH_POLICY, policy_values: &["AllowCloudSearch"], provider_category: "Search", provider_values: &["AllowCloudSearch"],
    },
    Spec {
        id: "cloud_search_work", name: "Search work or school content",
        description: "Include connected work or school cloud content in Windows Search. This never changes your organization's policy or signs an account in or out.",
        key: SEARCH, value: "IsAADCloudSearchEnabled",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: true, off: 0, on: 1,
        policy: SEARCH_POLICY, policy_values: &["AllowCloudSearch"], provider_category: "Search", provider_values: &["AllowCloudSearch"],
    },
    Spec {
        id: "search_history", name: "Search history on this device",
        description: "Let Windows save search history on this device. Turning this off changes future collection; it does not delete the existing search history or cloud account history.",
        key: SEARCH, value: "IsDeviceSearchHistoryEnabled",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: true, off: 0, on: 1,
        policy: SEARCH_POLICY, policy_values: &["DisableSearchHistory"], provider_category: "Search", provider_values: &[],
    },
    Spec {
        id: "search_highlights", name: "Search highlights and suggestions",
        description: "Show changing highlights in Windows Search. Turning this off reduces that content while retaining local file and app search. Windows Search may need to reopen.",
        key: SEARCH, value: "IsDynamicSearchBoxEnabled",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: true, off: 0, on: 1,
        policy: SEARCH_POLICY, policy_values: &["EnableDynamicContentInWSB"], provider_category: "Search", provider_values: &["AllowSearchHighlights"],
    },
    Spec {
        id: "start_recommendations", name: "Start tips and app recommendations",
        description: "Show recommendations for tips, shortcuts, new apps and more in supported Windows 11 Start menus. This does not remove your pinned apps or installed applications.",
        key: EXPLORER, value: "Start_IrisRecommendations",
        section: Section::Privacy, minimum: 26100, machine: false, existing_only: false, off: 0, on: 1,
        policy: EXPLORER, policy_values: &[], provider_category: "Start", provider_values: &["HideRecommendedSection"],
    },
    Spec {
        id: "recent_items", name: "Recent files in Start and Jump Lists",
        description: "Show recently opened files in Start, File Explorer and Jump Lists. Turning this off changes Windows' recent-item preference; it does not delete your files.",
        key: EXPLORER, value: "Start_TrackDocs",
        section: Section::Privacy, minimum: 19045, machine: false, existing_only: false, off: 0, on: 1,
        policy: r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer", policy_values: &["NoRecentDocsHistory", "ClearRecentDocsOnExit"], provider_category: "Start", provider_values: &["HideRecentJumplists"],
    },
    Spec {
        id: "optional_diagnostics", name: "Optional Windows diagnostic data",
        description: "Choose required-only or optional Windows diagnostic data on supported editions. Required diagnostic data remains enabled. This is a system-wide policy; apps outside Windows can have their own telemetry settings.",
        key: r"Software\Policies\Microsoft\Windows\DataCollection", value: "AllowTelemetry",
        section: Section::Privacy, minimum: 19045, machine: true, existing_only: false, off: 1, on: 3,
        policy: r"Software\Policies\Microsoft\Windows\DataCollection", policy_values: &["AllowTelemetry", "DisableTelemetryOptInSettingsUx", "LimitDiagnosticLogCollection", "LimitDumpCollection"], provider_category: "System", provider_values: &["AllowTelemetry"],
    },
    Spec {
        id: "recall_snapshots", name: "Recall snapshot saving",
        description: "Block saving Recall snapshots for this account on supported Windows 11 editions. Windows deletes this account's existing Recall snapshots when the block takes effect. Undo restores only the policy and cannot recover deleted snapshots. This does not inspect snapshots or enable Recall; enabling saving still requires Windows Hello and individual consent on supported Copilot+ hardware.",
        key: r"Software\Policies\Microsoft\Windows\WindowsAI", value: "DisableAIDataAnalysis",
        section: Section::Privacy, minimum: 26100, machine: false, existing_only: false, off: 1, on: 0,
        policy: r"Software\Policies\Microsoft\Windows\WindowsAI", policy_values: &["DisableAIDataAnalysis", "AllowRecallEnablement"], provider_category: "WindowsAI", provider_values: &["DisableAIDataAnalysis", "AllowRecallEnablement"],
    },
];
fn spec(id: &str) -> Result<&'static Spec, String> {
    SPECS
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| "Unknown Windows preference".into())
}
fn configured(s: &Spec, value: &Raw) -> Option<bool> {
    match value.number() {
        Some(n) if n == s.on => Some(true),
        Some(n) if n == s.off || (s.id == "optional_diagnostics" && n == 0) => Some(false),
        _ => None,
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Raw {
    kind: u32,
    bytes: Vec<u8>,
}
impl Raw {
    fn dword(n: u32) -> Self {
        Self {
            kind: REG_DWORD as u32,
            bytes: n.to_le_bytes().to_vec(),
        }
    }
    fn number(&self) -> Option<u32> {
        if self.kind != REG_DWORD as u32 {
            return None;
        }
        Some(u32::from_le_bytes(self.bytes.as_slice().try_into().ok()?))
    }
    fn registry(&self) -> Result<RegValue, String> {
        if self.bytes.len() > 4096 {
            return Err("Saved preference is too large.".into());
        }
        let vtype = match self.kind {
            0 => REG_NONE,
            1 => REG_SZ,
            2 => REG_EXPAND_SZ,
            3 => REG_BINARY,
            4 => REG_DWORD,
            5 => REG_DWORD_BIG_ENDIAN,
            6 => REG_LINK,
            7 => REG_MULTI_SZ,
            8 => REG_RESOURCE_LIST,
            9 => REG_FULL_RESOURCE_DESCRIPTOR,
            10 => REG_RESOURCE_REQUIREMENTS_LIST,
            11 => REG_QWORD,
            _ => return Err("Saved preference has an invalid registry type.".into()),
        };
        Ok(RegValue {
            vtype,
            bytes: self.bytes.clone(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    id: String,
    before: Option<Raw>,
    applied: Raw,
    pending: Option<Raw>,
}
impl Entry {
    fn owns(&self, value: &Option<Raw>) -> bool {
        value == &self.before
            || value.as_ref() == Some(&self.applied)
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| value.as_ref() == Some(pending))
    }
}
fn validate(entries: &[Entry], machine: bool) -> Result<(), String> {
    if entries.len() > SPECS.len() {
        return Err("The preference Undo record is invalid.".into());
    }
    let mut ids = HashSet::new();
    for entry in entries {
        let s = spec(&entry.id)?;
        if s.machine != machine || !ids.insert(&entry.id) {
            return Err("The preference Undo scope is invalid.".into());
        }
        if let Some(value) = &entry.before {
            value.registry()?;
        }
        for value in std::iter::once(&entry.applied).chain(entry.pending.iter()) {
            if ![Some(s.off), Some(s.on)].contains(&value.number()) {
                return Err("The saved preference operation is invalid.".into());
            }
        }
    }
    Ok(())
}
fn decode(text: &str, machine: bool) -> Result<Vec<Entry>, String> {
    if text.len() > 128 * 1024 {
        return Err("Preference Undo is too large; changes were stopped.".into());
    }
    let entries: Vec<Entry> = serde_json::from_str(text)
        .map_err(|_| "Preference Undo is damaged; changes were stopped.")?;
    validate(&entries, machine)?;
    Ok(entries)
}
fn root(machine: bool) -> RegKey {
    RegKey::predef(if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    })
}
fn read_at(root: &RegKey, path: &str, name: &str) -> Result<Option<Raw>, String> {
    let key = match root.open_subkey(path) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    match key.get_raw_value(name) {
        Ok(v) => Ok(Some(Raw {
            kind: v.vtype as u32,
            bytes: v.bytes,
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
fn flush(key: &RegKey) -> Result<(), String> {
    let code = unsafe { windows_sys::Win32::System::Registry::RegFlushKey(key.raw_handle()) };
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "Windows could not flush preference storage (code {code})."
        ))
    }
}
fn secure(key: &RegKey) -> Result<(), String> {
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
    let text: Vec<u16> = "D:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;BU)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err("Could not secure system preference Undo.".into());
    }
    let result = unsafe { RegSetKeySecurity(key.raw_handle(), 4 | 0x8000_0000, descriptor) };
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(descriptor);
    }
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "Could not secure system preference Undo (code {result})."
        ))
    }
}
trait Store {
    fn read(&self, id: &str) -> Result<Option<Raw>, String>;
    fn write(&mut self, id: &str, value: Option<&Raw>) -> Result<(), String>;
    fn load(&self) -> Result<Vec<Entry>, String>;
    fn save(&mut self, entries: &[Entry]) -> Result<(), String>;
}
struct WindowsStore(bool);
impl Store for WindowsStore {
    fn read(&self, id: &str) -> Result<Option<Raw>, String> {
        let s = spec(id)?;
        if s.machine != self.0 {
            return Err("Preference scope mismatch".into());
        }
        read_at(&root(s.machine), s.key, s.value)
    }
    fn write(&mut self, id: &str, value: Option<&Raw>) -> Result<(), String> {
        let s = spec(id)?;
        if s.machine != self.0 {
            return Err("Preference scope mismatch".into());
        }
        if s.machine && !crate::system::is_admin() {
            return Err("Administrator rights are required for this system preference.".into());
        }
        let (key, _) = root(s.machine)
            .create_subkey_with_flags(s.key, KEY_READ | KEY_WRITE)
            .map_err(|e| e.to_string())?;
        match value {
            Some(v) => key
                .set_raw_value(s.value, &v.registry()?)
                .map_err(|e| e.to_string())?,
            None => match key.delete_value(s.value) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            },
        }
        flush(&key)
    }
    fn load(&self) -> Result<Vec<Entry>, String> {
        let path = if self.0 { MACHINE_UNDO } else { USER_UNDO };
        let Some(raw) = read_at(&root(self.0), path, "EntriesV1")? else {
            return Ok(Vec::new());
        };
        if raw.kind != REG_SZ as u32 || raw.bytes.len() % 2 != 0 {
            return Err("Preference Undo has an invalid type.".into());
        }
        let text = String::from_utf16(
            &raw.bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|v| *v != 0)
                .collect::<Vec<_>>(),
        )
        .map_err(|_| "Preference Undo is not valid text.")?;
        decode(&text, self.0)
    }
    fn save(&mut self, entries: &[Entry]) -> Result<(), String> {
        validate(entries, self.0)?;
        if self.0 && !crate::system::is_admin() {
            return Err("Administrator rights are required to save system preference Undo.".into());
        }
        let (key, _) = root(self.0)
            .create_subkey_with_flags(
                if self.0 { MACHINE_UNDO } else { USER_UNDO },
                KEY_ALL_ACCESS,
            )
            .map_err(|e| e.to_string())?;
        if self.0 {
            secure(&key)?
        }
        key.set_value(
            "EntriesV1",
            &serde_json::to_string(entries).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        flush(&key)
    }
}
fn apply(
    store: &mut impl Store,
    id: &str,
    enabled: bool,
    expected: &Option<Raw>,
) -> Result<(), String> {
    let s = spec(id)?;
    if s.id == "recall_snapshots" && enabled {
        return Err("PCSweep does not enable Recall saving. Windows requires individual opt-in consent. Use Undo to restore only your original policy.".into());
    }
    if store.read(id)? != *expected {
        return Err("This preference changed after review. Refresh before trying again.".into());
    }
    if expected
        .as_ref()
        .is_some_and(|v| configured(s, v).is_none())
    {
        return Err("Windows published an unrecognized value. No preference was changed.".into());
    }
    if s.existing_only && expected.is_none() {
        return Err("Windows has not published this preference on this PC.".into());
    }
    let mut entries = store.load()?;
    if let Some(old) = entries.iter().find(|e| e.id == id) {
        if !old.owns(expected) {
            return Err("This preference was changed outside PCSweep. Keep that newer choice or forget the old Undo point before changing it here.".into());
        }
    }
    let desired = Raw::dword(if enabled { s.on } else { s.off });
    if expected.as_ref() == Some(&desired) {
        return Ok(());
    }
    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
        entry.pending = Some(desired.clone())
    } else {
        entries.push(Entry {
            id: id.into(),
            before: expected.clone(),
            applied: desired.clone(),
            pending: Some(desired.clone()),
        })
    }
    store.save(&entries)?;
    if store.read(id)? != *expected {
        return Err(
            "This preference changed while saving Undo. No setting was overwritten.".into(),
        );
    }
    store.write(id, Some(&desired))?;
    if store.read(id)?.as_ref() != Some(&desired) {
        return Err("Windows did not retain the preference. The saved Undo point was kept.".into());
    }
    let entry = entries
        .iter_mut()
        .find(|e| e.id == id)
        .ok_or("Saved Undo changed unexpectedly")?;
    entry.applied = desired;
    entry.pending = None;
    store.save(&entries)
}
fn undo(store: &mut impl Store, id: &str, expected: &Option<Raw>) -> Result<(), String> {
    let mut entries = store.load()?;
    let entry = entries
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .ok_or("No saved Undo point exists for this preference.")?;
    let current = store.read(id)?;
    if current != *expected || !entry.owns(&current) {
        return Err(
            "This preference changed outside PCSweep. That newer choice was preserved.".into(),
        );
    }
    if current != entry.before {
        store.write(id, entry.before.as_ref())?
    }
    if store.read(id)? != entry.before {
        return Err("Windows did not retain the restored preference. Undo was kept.".into());
    }
    entries.retain(|e| e.id != id);
    store.save(&entries)
}
fn entry_hash(entry: &Entry) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(entry).map_err(|e| e.to_string())?)
    ))
}
fn forget(store: &mut impl Store, id: &str, expected: &str) -> Result<(), String> {
    let mut entries = store.load()?;
    let entry = entries
        .iter()
        .find(|e| e.id == id)
        .ok_or("The saved Undo point no longer exists.")?;
    if entry_hash(entry)? != expected {
        return Err("The saved Undo point changed after review. Refresh first.".into());
    }
    entries.retain(|e| e.id != id);
    store.save(&entries)
}
#[derive(Clone, Default)]
struct Environment {
    build: u32,
    revision: u32,
    edition: String,
    managed: bool,
    user_sid: String,
}
fn user_sid() -> Result<String, String> {
    use windows_sys::Win32::{Foundation::*, Security::*, System::Threading::*};
    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertSidToStringSidW(sid: *mut std::ffi::c_void, text: *mut *mut u16) -> i32;
    }
    let mut token = 0;
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err("Windows did not report this account's identity.".into());
    }
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 64 * 1024 {
        unsafe {
            CloseHandle(token);
        }
        return Err("Windows reported an invalid account identity length.".into());
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    unsafe {
        CloseHandle(token);
    }
    if ok == 0 {
        return Err("Windows did not return this account's identity.".into());
    }
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 || text.is_null() {
        return Err("Windows could not encode this account's identity.".into());
    }
    let mut length = 0;
    while length < 256 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let result = if length < 256 {
        String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(|_| "Invalid Windows account identity".to_string())
    } else {
        Err("Windows account identity was too long.".into())
    };
    unsafe {
        LocalFree(text.cast());
    }
    result
}
fn environment() -> Result<Environment, String> {
    let key = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .map_err(|e| e.to_string())?;
    let build = key
        .get_value::<String, _>("CurrentBuildNumber")
        .map_err(|e| e.to_string())?
        .parse()
        .map_err(|_| "Windows build is unavailable")?;
    let edition = key
        .get_value::<String, _>("EditionID")
        .map_err(|e| e.to_string())?;
    #[link(name = "netapi32")]
    extern "system" {
        fn NetGetJoinInformation(server: *const u16, name: *mut *mut u16, status: *mut u32) -> u32;
        fn NetApiBufferFree(buffer: *mut std::ffi::c_void) -> u32;
    }
    let mut name = std::ptr::null_mut();
    let mut status = 0;
    let code = unsafe { NetGetJoinInformation(std::ptr::null(), &mut name, &mut status) };
    if !name.is_null() {
        unsafe {
            NetApiBufferFree(name.cast());
        }
    }
    if code != 0 {
        return Err(
            "Windows did not report management status. System preference changes are unavailable."
                .into(),
        );
    }
    let mdm = match root(true).open_subkey(r"SOFTWARE\Microsoft\Provisioning\OMADM\Accounts") {
        Ok(key) => key
            .enum_keys()
            .next()
            .transpose()
            .map_err(|e| e.to_string())?
            .is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.to_string()),
    };
    Ok(Environment {
        build,
        revision: key.get_value("UBR").unwrap_or(0),
        edition,
        managed: status == 3 || mdm,
        user_sid: user_sid()?,
    })
}
fn pro(edition: &str) -> bool {
    matches!(
        edition,
        "Professional"
            | "ProfessionalN"
            | "ProfessionalEducation"
            | "ProfessionalWorkstation"
            | "Enterprise"
            | "EnterpriseS"
            | "Education"
            | "EducationN"
            | "IoTEnterprise"
            | "IoTEnterpriseS"
    )
}
fn policy_guard(
    s: &Spec,
    env: &Environment,
    own: Option<&Entry>,
    current: &Option<Raw>,
) -> Result<(), String> {
    if env.build < s.minimum {
        return Err(format!(
            "This control requires Windows build {} or later.",
            s.minimum
        ));
    }
    if s.machine && (!pro(&env.edition) || env.managed) {
        return Err("This system policy requires a supported unmanaged Pro, Enterprise or Education edition.".into());
    }
    if s.id == "recall_snapshots"
        && (!pro(&env.edition) || env.managed || (env.build == 26100 && env.revision < 3915))
    {
        return Err("Recall blocking requires an unmanaged supported edition and Windows 11 build 26100.3915 or later. Recall itself also requires supported Copilot+ hardware.".into());
    }
    for machine in [false, true] {
        for name in s.policy_values {
            let value = read_at(&root(machine), s.policy, name)?;
            if value.is_some() {
                let is_target = s.machine == machine && s.policy == s.key && *name == s.value;
                // On an unmanaged PC the user may deliberately replace a local
                // setting they chose in another tool. Preserve its exact value
                // for Undo. Provider-owned policy is checked below separately.
                if is_target && (!env.managed || own.is_some_and(|entry| entry.owns(current))) {
                    continue;
                }
                return Err(
                    "Controlled by an existing Windows policy. PCSweep preserves it.".into(),
                );
            }
        }
    }
    for name in s.provider_values {
        for branch in [
            format!(
                r"SOFTWARE\Microsoft\PolicyManager\current\device\{}",
                s.provider_category
            ),
            format!(
                r"SOFTWARE\Microsoft\PolicyManager\current\user\{}",
                s.provider_category
            ),
            format!(
                r"SOFTWARE\Microsoft\PolicyManager\current\{}\{}",
                env.user_sid, s.provider_category
            ),
        ] {
            if read_at(&root(true), &branch, &format!("{name}_ProviderSet"))?
                .as_ref()
                .and_then(Raw::number)
                .is_some_and(|v| v != 0)
            {
                return Err("Controlled by a management provider. PCSweep preserves it.".into());
            }
        }
    }
    if s.id == "start_recommendations" {
        for hive in [false, true] {
            for name in ["HideRecommendedSection", "HideRecentlyAddedApps"] {
                if read_at(
                    &root(hive),
                    r"SOFTWARE\Policies\Microsoft\Windows\Explorer",
                    name,
                )?
                .is_some()
                {
                    return Err("Start recommendations are controlled by Windows policy.".into());
                }
            }
        }
    }
    Ok(())
}
struct Lock(isize);
impl Lock {
    fn take(machine: bool) -> Result<Self, String> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::*;
        let name = if machine {
            "Global\\PCSweep.DirectPreferences.Machine.v1".into()
        } else {
            format!("Global\\PCSweep.DirectPreferences.User.{}.v1", user_sid()?)
        };
        let n: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, n.as_ptr()) };
        if h == 0 {
            return Err("Could not protect preference Undo storage.".into());
        }
        match unsafe { WaitForSingleObject(h, 0) } {
            0 | 0x80 => Ok(Self(h)),
            _ => {
                unsafe {
                    CloseHandle(h);
                }
                Err(
                    "Another PCSweep copy is changing these preferences. Refresh in a moment."
                        .into(),
                )
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
#[derive(Clone)]
pub enum PreferencesAction {
    Set {
        id: String,
        enabled: bool,
        expected: Option<Raw>,
    },
    Undo {
        id: String,
        expected: Option<Raw>,
    },
    Forget {
        id: String,
        expected: String,
    },
}
fn execute(action: PreferencesAction) -> Result<String, String> {
    let id = match &action {
        PreferencesAction::Set { id, .. }
        | PreferencesAction::Undo { id, .. }
        | PreferencesAction::Forget { id, .. } => id,
    };
    let s = spec(id)?;
    let _lock = Lock::take(s.machine)?;
    if s.machine && !crate::system::is_admin() {
        return Err("Restart PCSweep as administrator for system-wide preferences.".into());
    }
    let mut store = WindowsStore(s.machine);
    if let PreferencesAction::Forget { id, expected } = action {
        forget(&mut store, &id, &expected)?;
        return Ok(format!(
            "Forgot the saved {} Undo point. Your Windows setting was kept.",
            s.name
        ));
    }
    let current = store.read(s.id)?;
    let entries = store.load()?;
    policy_guard(
        s,
        &environment()?,
        entries.iter().find(|e| e.id == s.id),
        &current,
    )?;
    let message = match action {
        PreferencesAction::Set {
            id,
            enabled,
            expected,
        } => {
            apply(&mut store, &id, enabled, &expected)?;
            if s.id == "recall_snapshots" {
                "Recall snapshot-saving block configured and read back. Windows applies the block and removes existing snapshots; Undo cannot recover them.".into()
            } else {
                format!("{} configured {} and read back from Windows. Reopen affected apps or sign out if they retain the previous preference.",s.name,if enabled{"on"}else{"off"})
            }
        }
        PreferencesAction::Undo { id, expected } => {
            undo(&mut store, &id, &expected)?;
            format!("Restored the exact original {} preference.", s.name)
        }
        PreferencesAction::Forget { .. } => unreachable!(),
    };
    broadcast(s);
    Ok(message)
}
fn broadcast(s: &Spec) {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let name: Vec<u16> = if s.machine {
        "Policy"
    } else {
        "ImmersiveColorSet"
    }
    .encode_utf16()
    .chain(Some(0))
    .collect();
    // Bounded notification: a hung third-party window cannot hold this worker.
    let mut result = 0;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            name.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            200,
            &mut result,
        );
    }
}
struct Item {
    id: String,
    value: Result<Option<Raw>, String>,
    guard: Result<(), String>,
    undo: Option<Entry>,
}
struct Inventory {
    items: Vec<Item>,
    user_error: Option<String>,
    machine_error: Option<String>,
}
fn inventory() -> Result<Inventory, String> {
    let env = environment()?;
    let user = WindowsStore(false).load();
    let machine = WindowsStore(true).load();
    let mut items = Vec::new();
    for s in SPECS {
        let value = WindowsStore(s.machine).read(s.id);
        let undo = if s.machine {
            machine.as_ref()
        } else {
            user.as_ref()
        }
        .ok()
        .and_then(|rows| rows.iter().find(|e| e.id == s.id))
        .cloned();
        let guard = match &value {
            Ok(v) => policy_guard(s, &env, undo.as_ref(), v),
            Err(e) => Err(e.clone()),
        };
        items.push(Item {
            id: s.id.into(),
            value,
            guard,
            undo,
        });
    }
    Ok(Inventory {
        items,
        user_error: user.err(),
        machine_error: machine.err(),
    })
}
#[derive(Default)]
pub struct PreferencesState {
    load: Task<Result<Inventory, String>>,
    change: Task<Result<String, String>>,
    inventory: Option<Inventory>,
    loaded: bool,
    error: Option<String>,
}
impl PreferencesState {
    pub fn busy(&self) -> bool {
        self.change.running()
    }
}
impl App {
    pub fn refresh_preferences(&mut self, ctx: &egui::Context) {
        if !self.preferences.load.running() && !self.preferences.change.running() {
            self.preferences.load = Task::spawn(ctx, inventory)
        }
    }
    pub fn execute_preferences(&mut self, action: PreferencesAction, ctx: &egui::Context) {
        if self.preferences.busy() {
            return;
        }
        self.preferences.error = None;
        self.preferences.change = Task::spawn(ctx, move || execute(action));
    }
    pub fn poll_preferences(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.preferences.load.poll() {
            self.preferences.loaded = true;
            match result.and_then(|r| r) {
                Ok(i) => self.preferences.inventory = Some(i),
                Err(e) => self.preferences.error = Some(e),
            }
        }
        if let Some(result) = self.preferences.change.poll() {
            match result.and_then(|r| r) {
                Ok(message) => {
                    util::log(&message);
                    self.toast(ToastKind::Success, message)
                }
                Err(e) => {
                    util::log(format!("Windows preference change stopped: {e}"));
                    self.preferences.error = Some(e.clone());
                    self.toast(ToastKind::Error, e)
                }
            }
            self.refresh_preferences(ctx);
        }
    }
    pub fn render_direct_preferences(&mut self, ui: &mut Ui, section: Section) {
        let ctx = ui.ctx().clone();
        if !self.preferences.loaded && !self.preferences.load.running() {
            self.refresh_preferences(&ctx)
        }
        let busy = self.mutations_running() || self.preferences.load.running();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!busy, widgets::small("Refresh preferences"))
                .clicked()
            {
                self.refresh_preferences(&ctx)
            }
            if self.preferences.load.running() || self.preferences.busy() {
                ui.spinner();
            }
            ui.label(widgets::faint(
                "Changes are applied here. Your exact original value is saved for Undo.",
            ));
        });
        if let Some(error) = &self.preferences.error {
            ui.colored_label(WARN, error);
        }
        let Some(inventory) = &self.preferences.inventory else {
            ui.label(widgets::dim(if self.preferences.load.running() {
                "Reading Windows preferences…"
            } else {
                "Preferences are unavailable. Refresh to try again."
            }));
            return;
        };
        let mut requested = None;
        for item in &inventory.items {
            let Ok(s) = spec(&item.id) else { continue };
            if s.section != section {
                continue;
            }
            let journal_error = if s.machine {
                &inventory.machine_error
            } else {
                &inventory.user_error
            };
            widgets::tight_frame().show(ui,|ui|{
                ui.horizontal_wrapped(|ui|{
                    ui.label(widgets::strong(s.name,15.0));
                    if s.machine{widgets::badge(ui,"System-wide · admin",TEXT_DIM);}
                });
                ui.label(widgets::dim(s.description));
                let current=item.value.as_ref().ok().and_then(|v|v.as_ref());
                let bit=current.and_then(Raw::number);
                let state=current.and_then(|value|configured(s,value));
                let recognized=current.is_none()||state.is_some();
                let own=item.undo.as_ref().is_none_or(|e|item.value.as_ref().is_ok_and(|v|e.owns(v)));
                let can_change=!busy&&item.value.is_ok()&&item.guard.is_ok()&&journal_error.is_none()&&recognized&&own&&(!s.machine||self.is_admin)&&(!s.existing_only||current.is_some());
                let status=match &item.value{
                    Err(e)=>format!("Unavailable: {e}"),
                    Ok(None)=>"Windows default · no explicit value saved".into(),
                    Ok(Some(_))if s.id=="optional_diagnostics"&&bit==Some(0)=>"Optional data off · a lower diagnostic level is configured".into(),
                    Ok(Some(_))if bit==Some(s.off)=>if s.id=="optional_diagnostics"{"Required data only · optional data off".into()}else if s.id=="recall_snapshots"{"Snapshot saving blocked by account policy".into()}else{"Off".into()},
                    Ok(Some(_))if bit==Some(s.on)=>if s.id=="recall_snapshots"{"Policy allows individual opt-in · saving state is separate".into()}else{"On".into()},
                    _=>"Windows published an unrecognized preference; kept unchanged".into(),
                };
                ui.label(widgets::faint(status));
                if let Err(e)=&item.guard{ui.colored_label(WARN,e);}
                if let Some(e)=journal_error{ui.colored_label(WARN,e);}
                if !own{ui.colored_label(WARN,"Changed outside PCSweep. Your newer choice is preserved.");}
                if s.existing_only&&current.is_none(){ui.label(widgets::faint("PCSweep could not confirm this preference for this account or Windows build."));}
                ui.horizontal_wrapped(|ui|{
                    for (enabled,label)in[(false,"Turn off…"),(true,"Turn on…")]{
                        if s.id=="recall_snapshots"&&enabled {continue}
                        let label=if s.id=="recall_snapshots"{"Block snapshot saving…"}else{label};
                        if ui.add_enabled(can_change&&state!=Some(enabled),widgets::small(label)).clicked(){
                            requested=Some((if s.id=="recall_snapshots"{"Block Recall and delete its saved snapshots?".into()}else{format!("Turn {} {}?",if enabled{"on"}else{"off"},s.name)},s.description.to_string(),PreferencesAction::Set{id:s.id.into(),enabled,expected:item.value.as_ref().ok().cloned().flatten()},s.id=="recall_snapshots"));
                        }
                    }
                    if let Some(entry)=&item.undo{
                        if ui.add_enabled(!busy&&own&&item.guard.is_ok()&&(!s.machine||self.is_admin),widgets::small("Undo PCSweep change…")).clicked(){requested=Some((format!("Restore {}?",s.name),"Restore the exact value or absence saved before PCSweep changed this preference. A choice made outside PCSweep is preserved.".into(),PreferencesAction::Undo{id:s.id.into(),expected:item.value.as_ref().ok().cloned().flatten()},false));}
                        if !own{
                            if let Ok(expected)=entry_hash(entry){if ui.add_enabled(!busy&&(!s.machine||self.is_admin),widgets::small("Forget old Undo point…")).clicked(){requested=Some((format!("Forget saved {} Undo?",s.name),"Keep the current Windows preference and remove only PCSweep's old saved Undo point. You can then review a fresh change.".into(),PreferencesAction::Forget{id:s.id.into(),expected},false));}}
                        }
                    }
                    if s.machine&&!self.is_admin{ui.label(widgets::dim("Use Restart as admin in the sidebar to change this system policy."));}
                });
            });
            ui.add_space(4.0);
        }
        if let Some((title, body, action, danger)) = requested {
            self.modal=Some(Modal{title,body,details:Vec::new(),note:Some("The saved preference is read back after applying. Existing organization policy is preserved. Reopen affected apps or sign out if they retain cached settings. Undo restores the original value, including an originally absent value.".into()),confirm:"Apply preference".into(),danger,action:Action::Preferences(action)});
        }
    }
}
#[cfg(test)]
pub(crate) fn preview_fixture() -> PreferencesState {
    PreferencesState {
        loaded: true,
        inventory: Some(Inventory {
            items: SPECS
                .iter()
                .enumerate()
                .map(|(i, s)| Item {
                    id: s.id.into(),
                    value: Ok(Some(Raw::dword(if i % 3 == 0 { s.off } else { s.on }))),
                    guard: Ok(()),
                    undo: None,
                })
                .collect(),
            user_error: None,
            machine_error: None,
        }),
        ..Default::default()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    #[derive(Default)]
    struct Fake {
        values: HashMap<String, Raw>,
        entries: Vec<Entry>,
        writes: usize,
        saves: usize,
        fail_save: Option<usize>,
        fail_write: bool,
        wrong_readback: bool,
    }
    impl Store for Fake {
        fn read(&self, id: &str) -> Result<Option<Raw>, String> {
            Ok(self.values.get(id).cloned())
        }
        fn write(&mut self, id: &str, value: Option<&Raw>) -> Result<(), String> {
            self.writes += 1;
            if self.fail_write {
                return Err("write failed".into());
            }
            if self.wrong_readback {
                return Ok(());
            }
            match value {
                Some(v) => {
                    self.values.insert(id.into(), v.clone());
                }
                None => {
                    self.values.remove(id);
                }
            }
            Ok(())
        }
        fn load(&self) -> Result<Vec<Entry>, String> {
            Ok(self.entries.clone())
        }
        fn save(&mut self, entries: &[Entry]) -> Result<(), String> {
            self.saves += 1;
            if self.fail_save == Some(self.saves) {
                return Err("flush failed".into());
            }
            self.entries = entries.to_vec();
            Ok(())
        }
    }
    #[test]
    fn preference_undo_restores_absence_and_preserves_first_value_across_toggles() {
        let mut f = Fake::default();
        apply(&mut f, "transparency", false, &None).unwrap();
        apply(&mut f, "transparency", true, &Some(Raw::dword(0))).unwrap();
        assert_eq!(f.entries[0].before, None);
        undo(&mut f, "transparency", &Some(Raw::dword(1))).unwrap();
        assert!(!f.values.contains_key("transparency"));
        assert!(f.entries.is_empty());
    }
    #[test]
    fn journal_failure_prevents_registry_writes() {
        let mut f = Fake {
            fail_save: Some(1),
            ..Default::default()
        };
        assert!(apply(&mut f, "game_mode", false, &None).is_err());
        assert_eq!(f.writes, 0);
    }
    #[test]
    fn post_write_flush_failure_keeps_recoverable_pending_intention() {
        let mut f = Fake {
            fail_save: Some(2),
            ..Default::default()
        };
        assert!(apply(&mut f, "transparency", false, &None).is_err());
        assert_eq!(f.values.get("transparency"), Some(&Raw::dword(0)));
        assert_eq!(f.entries[0].pending, Some(Raw::dword(0)));
        f.fail_save = None;
        undo(&mut f, "transparency", &Some(Raw::dword(0))).unwrap();
        assert!(f.entries.is_empty());
        assert!(!f.values.contains_key("transparency"));
    }
    #[test]
    fn changed_values_are_never_overwritten_on_apply_or_undo() {
        let mut f = Fake::default();
        f.values.insert("game_mode".into(), Raw::dword(1));
        assert!(apply(&mut f, "game_mode", false, &None).is_err());
        assert_eq!(f.writes, 0);
        apply(&mut f, "game_mode", false, &Some(Raw::dword(1))).unwrap();
        f.values.insert("game_mode".into(), Raw::dword(9));
        assert!(undo(&mut f, "game_mode", &Some(Raw::dword(9))).is_err());
        assert_eq!(f.values.get("game_mode"), Some(&Raw::dword(9)));
    }
    #[test]
    fn failed_write_or_readback_preserves_exact_undo() {
        for readback in [false, true] {
            let mut f = Fake {
                fail_write: !readback,
                wrong_readback: readback,
                ..Default::default()
            };
            assert!(apply(&mut f, "game_mode", true, &None).is_err());
            assert_eq!(f.entries.len(), 1);
            assert_eq!(f.entries[0].before, None);
        }
    }
    #[test]
    fn invalid_and_cross_scope_undo_records_are_rejected() {
        let bad = Entry {
            id: "transparency".into(),
            before: None,
            applied: Raw::dword(0),
            pending: None,
        };
        let text = serde_json::to_string(&vec![bad.clone()]).unwrap();
        assert!(decode(&text, true).is_err());
        assert_eq!(decode(&text, false).unwrap().len(), 1);
        let text = serde_json::to_string(&vec![bad.clone(), bad]).unwrap();
        assert!(decode(&text, false).is_err());
        assert!(decode("[]garbage", false).is_err());
        assert!(spec("HKLM\\evil").is_err());
    }
    #[test]
    fn version_dependent_unpublished_search_preferences_cannot_be_created() {
        let mut f = Fake::default();
        assert!(apply(&mut f, "cloud_search_personal", false, &None).is_err());
        assert_eq!(f.writes, 0);
    }
    #[test]
    fn forgetting_stale_undo_never_changes_windows_and_checks_review_identity() {
        let mut f = Fake::default();
        apply(&mut f, "transparency", false, &None).unwrap();
        let before = f.writes;
        assert!(forget(&mut f, "transparency", "stale hash").is_err());
        let hash = entry_hash(&f.entries[0]).unwrap();
        forget(&mut f, "transparency", &hash).unwrap();
        assert_eq!(f.writes, before);
        assert!(f.entries.is_empty());
    }
    #[test]
    fn explicit_machine_scope_gates_keep_home_and_managed_preferences_safe() {
        assert!(!pro("Core"));
        assert!(!pro("ServerStandard"));
        assert!(pro("Professional"));
        let s = spec("optional_diagnostics").unwrap();
        for env in [
            Environment {
                build: 26100,
                revision: 4000,
                edition: "Core".into(),
                managed: false,
                ..Default::default()
            },
            Environment {
                build: 26100,
                revision: 4000,
                edition: "Professional".into(),
                managed: true,
                ..Default::default()
            },
        ] {
            assert!(policy_guard(s, &env, None, &None).is_err())
        }
    }
    #[test]
    fn native_read_only_preference_inventory_handles_missing_values() {
        // Reads only. No real preference or Undo key is created by this test.
        let i = inventory().expect("read native Windows preferences without changing any setting");
        assert_eq!(i.items.len(), SPECS.len());
        for item in i.items {
            spec(&item.id).unwrap();
        }
    }
    #[test]
    fn deleting_an_owned_value_elsewhere_is_preserved_by_undo() {
        let mut f = Fake::default();
        f.values.insert("transparency".into(), Raw::dword(1));
        apply(&mut f, "transparency", false, &Some(Raw::dword(1))).unwrap();
        f.values.remove("transparency");
        let writes = f.writes;
        assert!(undo(&mut f, "transparency", &None).is_err());
        assert_eq!(f.writes, writes);
        assert!(!f.values.contains_key("transparency"));
    }
    #[test]
    fn recall_control_never_enables_snapshots_and_gates_the_first_supported_patch() {
        let mut f = Fake::default();
        assert!(apply(&mut f, "recall_snapshots", true, &None).is_err());
        assert_eq!(f.writes, 0);
        apply(&mut f, "recall_snapshots", false, &None).unwrap();
        assert_eq!(f.values.get("recall_snapshots"), Some(&Raw::dword(1)));
        let env = Environment {
            build: 26100,
            revision: 3914,
            edition: "Professional".into(),
            managed: false,
            ..Default::default()
        };
        assert!(policy_guard(spec("recall_snapshots").unwrap(), &env, None, &None).is_err());
    }
}
