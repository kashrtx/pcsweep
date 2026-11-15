//! Account privacy preferences, with a write-ahead undo journal. All targets are
//! fixed in this module: journal contents can never name an arbitrary registry key.
use crate::app::{Action, App, Modal, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util;
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Layout, RichText, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use winreg::enums::*;
use winreg::{RegKey, RegValue};

const JOURNAL_KEY: &str = r"Software\PCSweep\PrivacyUndo";
const JOURNAL_VALUE: &str = "AccountPreferencesV1";
const CONTENT: &str = r"Software\Microsoft\Windows\CurrentVersion\ContentDeliveryManager";
const CLOUD_POLICY: &str = r"Software\Policies\Microsoft\Windows\CloudContent";

struct Preference {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    key: &'static str,
    value: &'static str,
    settings: &'static str,
    // None means Windows 10 and 11; Start's old preference is Windows 10 only.
    max_build: Option<u32>,
    policy_key: &'static str,
    policy_values: &'static [&'static str],
}

const PREFERENCES: &[Preference] = &[
    Preference {
        id: "advertising_id",
        name: "Advertising ID",
        description: "Stop Windows apps using this account's advertising identifier. This limits personalization; it does not block ads or website cookies.",
        key: r"Software\Microsoft\Windows\CurrentVersion\AdvertisingInfo",
        value: "Enabled",
        settings: "ms-settings:privacy-general",
        max_build: None,
        policy_key: r"Software\Policies\Microsoft\Windows\AdvertisingInfo",
        policy_values: &["DisabledByGroupPolicy"],
    },
    Preference {
        id: "windows_tips",
        name: "Windows tips and suggestions",
        description: "Reduce suggestions about using Windows. Security alerts and app notifications stay available.",
        key: CONTENT,
        value: "SubscribedContent-338389Enabled",
        settings: "ms-settings:notifications",
        max_build: None,
        policy_key: CLOUD_POLICY,
        policy_values: &["DisableSoftLanding", "DisableWindowsSpotlightFeatures"],
    },
    Preference {
        id: "welcome_experience",
        name: "Windows welcome suggestions",
        description: "Reduce the Windows welcome experience shown after updates and sign-in.",
        key: CONTENT,
        value: "SubscribedContent-310093Enabled",
        settings: "ms-settings:notifications",
        max_build: None,
        policy_key: CLOUD_POLICY,
        policy_values: &["DisableWindowsSpotlightWindowsWelcomeExperience", "DisableWindowsSpotlightFeatures"],
    },
    Preference {
        id: "start_suggestions",
        name: "Suggested apps in Start (Windows 10)",
        description: "Turn off occasional app suggestions in the Windows 10 Start menu. Windows 11 has different recommendation controls.",
        key: CONTENT,
        value: "SubscribedContent-338388Enabled",
        settings: "ms-settings:personalization-start",
        max_build: Some(19045),
        policy_key: CLOUD_POLICY,
        policy_values: &["DisableWindowsConsumerFeatures", "DisableWindowsSpotlightFeatures"],
    },
    Preference {
        id: "lock_screen_tips",
        name: "Lock-screen facts and tips",
        description: "Turn off the facts-and-tips preference used with picture or slideshow lock screens. Windows Spotlight has its own settings.",
        key: CONTENT,
        value: "RotatingLockScreenOverlayEnabled",
        settings: "ms-settings:lockscreen",
        max_build: None,
        policy_key: CLOUD_POLICY,
        policy_values: &["ConfigureWindowsSpotlight", "DisableWindowsSpotlightFeatures"],
    },
];

fn preference(id: &str) -> Result<&'static Preference, String> {
    PREFERENCES
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| "Unknown privacy preference".into())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawValue {
    kind: u32,
    bytes: Vec<u8>,
}

impl RawValue {
    fn dword(value: u32) -> Self {
        Self {
            kind: REG_DWORD as u32,
            bytes: value.to_le_bytes().to_vec(),
        }
    }
    fn as_dword(&self) -> Option<u32> {
        if self.kind != REG_DWORD as u32 {
            return None;
        }
        Some(u32::from_le_bytes(self.bytes.as_slice().try_into().ok()?))
    }
    fn from_registry(value: RegValue) -> Self {
        Self {
            kind: value.vtype as u32,
            bytes: value.bytes,
        }
    }
    fn to_registry(&self) -> Result<RegValue, String> {
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
            _ => return Err("The saved registry type is invalid".into()),
        };
        if self.bytes.len() > 4096 {
            return Err("The saved privacy value is too large".into());
        }
        Ok(RegValue {
            vtype,
            bytes: self.bytes.clone(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UndoEntry {
    id: String,
    before: Option<RawValue>,
    after: RawValue,
}

#[derive(Default, Serialize, Deserialize)]
struct Journal {
    entries: Vec<UndoEntry>,
}

impl Journal {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 64 * 1024 {
            return Err("The privacy undo journal is too large".into());
        }
        let journal: Self = serde_json::from_slice(bytes).map_err(|_| {
            "The privacy undo journal is damaged. Changes are blocked to preserve it."
        })?;
        let mut ids = HashSet::new();
        for entry in &journal.entries {
            preference(&entry.id)?;
            if !ids.insert(&entry.id) || entry.after != RawValue::dword(0) {
                return Err("The privacy undo journal contains an invalid operation".into());
            }
            if let Some(before) = &entry.before {
                before.to_registry()?;
            }
        }
        Ok(journal)
    }
}

trait Store {
    fn read(&self, id: &str) -> Result<Option<RawValue>, String>;
    fn write(&mut self, id: &str, value: Option<&RawValue>) -> Result<(), String>;
    fn journal(&self) -> Result<Journal, String>;
    fn save_journal(&mut self, journal: &Journal) -> Result<(), String>;
}

struct RegistryStore;

fn flush_key(key: &RegKey) -> Result<(), String> {
    #[link(name = "advapi32")]
    extern "system" {
        fn RegFlushKey(key: isize) -> i32;
    }
    let status = unsafe { RegFlushKey(key.raw_handle() as isize) };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(status).to_string())
    }
}

fn raw_value(root: &RegKey, path: &str, name: &str) -> Result<Option<RawValue>, String> {
    let key = match root.open_subkey_with_flags(path, KEY_READ) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    match key.get_raw_value(name) {
        Ok(value) => Ok(Some(RawValue::from_registry(value))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

impl Store for RegistryStore {
    fn read(&self, id: &str) -> Result<Option<RawValue>, String> {
        let p = preference(id)?;
        raw_value(&RegKey::predef(HKEY_CURRENT_USER), p.key, p.value)
    }
    fn write(&mut self, id: &str, value: Option<&RawValue>) -> Result<(), String> {
        let p = preference(id)?;
        let root = RegKey::predef(HKEY_CURRENT_USER);
        match value {
            Some(value) => {
                let key = root
                    .open_subkey_with_flags(p.key, KEY_SET_VALUE | KEY_QUERY_VALUE)
                    .map_err(|e| e.to_string())?;
                key.set_raw_value(p.value, &value.to_registry()?)
                    .map_err(|e| e.to_string())?;
                flush_key(&key)
            }
            None => match root.open_subkey_with_flags(p.key, KEY_SET_VALUE | KEY_QUERY_VALUE) {
                Ok(key) => match key.delete_value(p.value) {
                    Ok(()) => flush_key(&key),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e.to_string()),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            },
        }
    }
    fn journal(&self) -> Result<Journal, String> {
        match raw_value(
            &RegKey::predef(HKEY_CURRENT_USER),
            JOURNAL_KEY,
            JOURNAL_VALUE,
        )? {
            None => Ok(Journal::default()),
            Some(raw) if raw.kind == REG_BINARY as u32 => Journal::decode(&raw.bytes),
            Some(_) => Err("The privacy undo journal has an unexpected type".into()),
        }
    }
    fn save_journal(&mut self, journal: &Journal) -> Result<(), String> {
        let bytes = serde_json::to_vec(journal).map_err(|e| e.to_string())?;
        // HKCU uses the signed-in account's access controls. No machine settings
        // or executable paths are loaded from this user-owned data.
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(JOURNAL_KEY)
            .map_err(|e| e.to_string())?;
        key.set_raw_value(
            JOURNAL_VALUE,
            &RegValue {
                vtype: REG_BINARY,
                bytes,
            },
        )
        .map_err(|e| e.to_string())?;
        flush_key(&key)
    }
}

fn apply<S: Store>(store: &mut S, id: &str, expected: Option<RawValue>) -> Result<(), String> {
    preference(id)?;
    let mut journal = store.journal()?;
    if journal.entries.iter().any(|entry| entry.id == id) {
        return Err(
            "An undo point already exists. Restore it before making another change.".into(),
        );
    }
    if store.read(id)? != expected {
        return Err("The preference changed since review. Refresh and try again.".into());
    }
    let after = RawValue::dword(0);
    journal.entries.push(UndoEntry {
        id: id.into(),
        before: expected,
        after: after.clone(),
    });
    // Persist the original bytes before making the first change. Even an
    // interrupted write leaves enough information to offer a precise undo.
    store.save_journal(&journal)?;
    store.write(id, Some(&after))?;
    if store.read(id)? != Some(after) {
        return Err("Windows did not retain the preference. The undo point is preserved.".into());
    }
    Ok(())
}

fn undo<S: Store>(store: &mut S, id: &str) -> Result<(), String> {
    preference(id)?;
    let mut journal = store.journal()?;
    let index = journal
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or("There is no saved undo point for this preference")?;
    let entry = &journal.entries[index];
    let current = store.read(id)?;
    if current != entry.before {
        if current != Some(entry.after.clone()) {
            return Err("Another app or Windows changed this preference. Undo will not overwrite that newer choice.".into());
        }
        store.write(id, entry.before.as_ref())?;
        if store.read(id)? != entry.before {
            return Err(
                "Windows did not retain the restored value. The undo point is preserved.".into(),
            );
        }
    }
    journal.entries.remove(index);
    store.save_journal(&journal)
}

fn policy_present(p: &Preference) -> Result<bool, String> {
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let root = RegKey::predef(hive);
        for name in p.policy_values {
            if raw_value(&root, p.policy_key, name)?.is_some() {
                return Ok(true);
            }
        }
    }
    // Only provider-owned values indicate MDM management. PolicyManager also
    // contains defaults; the mere existence of the category is not management.
    let (category, names): (&str, &[&str]) = match p.id {
        "advertising_id" => ("Privacy", &["DisableAdvertisingId"]),
        "windows_tips" => ("Experience", &["AllowWindowsTips", "AllowWindowsSpotlight"]),
        "welcome_experience" => (
            "Experience",
            &[
                "AllowWindowsSpotlightWindowsWelcomeExperience",
                "AllowWindowsSpotlight",
            ],
        ),
        "start_suggestions" => (
            "Experience",
            &["AllowWindowsConsumerFeatures", "AllowWindowsSpotlight"],
        ),
        "lock_screen_tips" => (
            "Experience",
            &[
                "ConfigureWindowsSpotlightOnLockScreen",
                "AllowWindowsSpotlight",
            ],
        ),
        _ => return Err("Unknown privacy preference".into()),
    };
    let branch = format!(r"Software\Microsoft\PolicyManager\current\device\{category}");
    for name in names {
        if raw_value(
            &RegKey::predef(HKEY_LOCAL_MACHINE),
            &branch,
            &format!("{name}_ProviderSet"),
        )?
        .as_ref()
        .and_then(RawValue::as_dword)
        .is_some_and(|v| v != 0)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn windows_build() -> u32 {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .and_then(|k| k.get_value::<String, _>("CurrentBuildNumber"))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn available(p: &Preference, build: u32) -> bool {
    build >= 10240 && p.max_build.is_none_or(|max| build <= max)
}

struct Item {
    id: String,
    value: Option<RawValue>,
    status: String,
    can_disable: bool,
    can_undo: bool,
}

struct Inventory {
    build: u32,
    edition: String,
    items: Vec<Item>,
    journal_error: Option<String>,
    clipboard: String,
    recall: String,
}

fn inventory() -> Inventory {
    let build = windows_build();
    let edition = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .and_then(|k| k.get_value::<String, _>("EditionID"))
        .unwrap_or_else(|_| "Unknown edition".into());
    let store = RegistryStore;
    let journal = store.journal();
    let journal_error = journal.as_ref().err().cloned();
    let mut items = Vec::new();
    for p in PREFERENCES {
        let saved = journal
            .as_ref()
            .ok()
            .and_then(|j| j.entries.iter().find(|e| e.id == p.id));
        let read = store.read(p.id);
        let value = read.as_ref().ok().cloned().flatten();
        let policy = policy_present(p);
        let managed = policy.as_ref().copied().unwrap_or(true);
        let supported = available(p, build);
        let bit = value.as_ref().and_then(RawValue::as_dword);
        let status = if let Err(e) = read {
            format!("Could not read: {e}")
        } else if let Err(e) = policy {
            format!("Could not verify management: {e}")
        } else if managed {
            "Managed by policy — review in Windows".into()
        } else if !supported {
            "Different controls on this Windows version".into()
        } else {
            match bit {
                Some(0) => "Off (saved preference)".into(),
                Some(1) => "On (saved preference)".into(),
                _ => "Not recognized on this PC — review in Windows".into(),
            }
        };
        items.push(Item {
            id: p.id.into(),
            value,
            status,
            can_disable: supported
                && !managed
                && bit == Some(1)
                && saved.is_none()
                && journal_error.is_none(),
            can_undo: !managed && saved.is_some() && journal_error.is_none(),
        });
    }
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let describe = |name| match raw_value(&root, r"Software\Microsoft\Clipboard", name)
        .ok()
        .flatten()
        .and_then(|v| v.as_dword())
    {
        Some(0) => "off",
        Some(1) => "on",
        _ => "review in Windows",
    };
    let clipboard = format!(
        "History: {} · Sync across devices: {}",
        describe("EnableClipboardHistory"),
        describe("EnableCloudClipboard")
    );
    let recall_policy = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .any(|hive| {
            let root = RegKey::predef(hive);
            let branch = r"Software\Policies\Microsoft\Windows\WindowsAI";
            raw_value(&root, branch, "DisableAIDataAnalysis")
                .ok()
                .flatten()
                .and_then(|v| v.as_dword())
                == Some(1)
                || raw_value(&root, branch, "AllowRecallEnablement")
                    .ok()
                    .flatten()
                    .and_then(|v| v.as_dword())
                    == Some(0)
        });
    let recall = if build < 26100 {
        "Not available on this Windows build".into()
    } else if recall_policy {
        "Recall or snapshot saving is blocked by a Windows policy".into()
    } else {
        "Supported Copilot+ hardware required. Verify snapshot saving in Windows.".into()
    };
    Inventory {
        build,
        edition,
        items,
        journal_error,
        clipboard,
        recall,
    }
}

#[derive(Clone)]
pub enum PrivacyAction {
    Disable {
        id: String,
        expected: Option<RawValue>,
    },
    Undo(String),
    ClearClipboard,
}

fn clear_clipboard() -> Result<(), String> {
    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(owner: isize) -> i32;
        fn EmptyClipboard() -> i32;
        fn CloseClipboard() -> i32;
    }
    unsafe {
        if OpenClipboard(0) == 0 {
            return Err("The clipboard is in use. Try again.".into());
        }
        let cleared = EmptyClipboard() != 0;
        CloseClipboard();
        if cleared {
            Ok(())
        } else {
            Err("Windows could not clear the current clipboard".into())
        }
    }
}

struct PrivacyLock(isize);

impl PrivacyLock {
    fn acquire() -> Result<Self, String> {
        #[link(name = "kernel32")]
        extern "system" {
            fn CreateMutexW(
                attributes: *const std::ffi::c_void,
                owner: i32,
                name: *const u16,
            ) -> isize;
            fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
            fn CloseHandle(handle: isize) -> i32;
        }
        let name: Vec<u16> = "Local\\PCSweep.PrivacyUndo.v1"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle == 0 {
                return Err(
                    "Could not protect the privacy undo journal. Changes were stopped.".into(),
                );
            }
            match WaitForSingleObject(handle, 0) {
                0 | 0x80 => Ok(Self(handle)),
                _ => {
                    CloseHandle(handle);
                    Err(
                        "Another copy of PCSweep is changing privacy settings. Wait and refresh."
                            .into(),
                    )
                }
            }
        }
    }
}

impl Drop for PrivacyLock {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        extern "system" {
            fn ReleaseMutex(handle: isize) -> i32;
            fn CloseHandle(handle: isize) -> i32;
        }
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

fn execute(action: PrivacyAction) -> Result<String, String> {
    let _lock = PrivacyLock::acquire()?;
    match action {
        PrivacyAction::ClearClipboard => {
            clear_clipboard()?;
            Ok("Cleared the current clipboard. Windows clipboard history was kept.".into())
        }
        PrivacyAction::Disable { id, expected } => {
            let p = preference(&id)?;
            if !available(p, windows_build()) || policy_present(p)? {
                return Err(
                    "This preference is unavailable or managed. Refresh before changing it.".into(),
                );
            }
            if expected.as_ref().and_then(RawValue::as_dword) != Some(1) {
                return Err("Only a recognized enabled preference can be turned off".into());
            }
            apply(&mut RegistryStore, &id, expected)?;
            Ok(format!(
                "Turned off {}. Windows may apply the change after sign-out.",
                p.name
            ))
        }
        PrivacyAction::Undo(id) => {
            let p = preference(&id)?;
            if policy_present(p)? {
                return Err(
                    "This preference is now managed. Undo will not override policy.".into(),
                );
            }
            undo(&mut RegistryStore, &id)?;
            Ok(format!("Restored the original {} preference", p.name))
        }
    }
}

#[derive(Default)]
pub struct PrivacyState {
    load: Task<Inventory>,
    change: Task<Result<String, String>>,
    inventory: Option<Inventory>,
    loaded: bool,
}

impl PrivacyState {
    pub fn busy(&self) -> bool {
        self.change.running()
    }
}

/// A deterministic account review for headless page rendering. It reads and
/// changes no Windows preferences and never loads the actual undo journal.
#[cfg(test)]
pub(crate) fn preview_fixture() -> PrivacyState {
    let items = PREFERENCES
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let supported = available(p, 26100);
            Item {
                id: p.id.into(),
                value: Some(RawValue::dword(if i == 0 { 0 } else { 1 })),
                status: if !supported {
                    "Different controls on this Windows version".into()
                } else if i == 0 {
                    "Off (saved preference)".into()
                } else {
                    "On (saved preference)".into()
                },
                can_disable: supported && i > 0,
                can_undo: i == 0,
            }
        })
        .collect();
    PrivacyState {
        loaded: true,
        inventory: Some(Inventory {
            build: 26100,
            edition: "Professional".into(),
            items,
            journal_error: None,
            clipboard: "History: on · Sync across devices: off".into(),
            recall: "Supported Copilot+ hardware required. Verify snapshot saving in Windows."
                .into(),
        }),
        ..Default::default()
    }
}

impl App {
    fn refresh_privacy(&mut self, ctx: &egui::Context) {
        if !self.privacy.load.running() && !self.privacy.change.running() {
            self.privacy.load = Task::spawn(ctx, inventory);
        }
    }

    pub fn execute_privacy(&mut self, action: PrivacyAction, ctx: &egui::Context) {
        if self.privacy.busy() {
            return;
        }
        self.privacy.change = Task::spawn(ctx, move || execute(action));
    }

    pub fn poll_privacy(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.privacy.load.poll() {
            self.privacy.loaded = true;
            match result {
                Ok(value) => self.privacy.inventory = Some(value),
                Err(e) => self.toast(ToastKind::Error, format!("Privacy review failed: {e}")),
            }
        }
        if let Some(result) = self.privacy.change.poll() {
            match result.and_then(|r| r) {
                Ok(message) => {
                    util::log(&message);
                    self.toast(ToastKind::Success, message);
                }
                Err(e) => {
                    util::log(format!("Privacy change stopped: {e}"));
                    self.toast(ToastKind::Error, e);
                }
            }
            self.refresh_privacy(ctx);
        }
    }

    pub fn page_privacy(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.privacy.loaded && !self.privacy.load.running() {
            self.refresh_privacy(&ctx);
        }
        widgets::page_header(ui, "Privacy and distractions", "Review your account's preferences, reduce suggestions, and keep a precise undo point for each change.");
        let busy = self.privacy.busy() || self.privacy.load.running();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !busy,
                    widgets::small(widgets::icon_text(ic::REFRESH, "Refresh", 13.0, TEXT)),
                )
                .clicked()
            {
                self.refresh_privacy(&ctx);
            }
            if busy {
                ui.spinner();
            }
            if let Some(i) = &self.privacy.inventory {
                ui.label(widgets::faint(format!(
                    "{} · {} · build {}",
                    if i.build >= 22000 {
                        "Windows 11"
                    } else {
                        "Windows 10"
                    },
                    i.edition,
                    i.build
                )));
            }
        });
        let Some(inv) = &self.privacy.inventory else {
            ui.label(widgets::dim(if self.privacy.load.running() {
                "Reading this account's privacy preferences…"
            } else {
                "Privacy preferences could not be loaded. Refresh to try again."
            }));
            return;
        };
        let mut requested = None;
        ScrollArea::vertical().id_salt("privacy_page").show(ui, |ui| {
            if let Some(error) = &inv.journal_error {
                widgets::banner(ui, WARN, ic::WARN, |ui| { ui.label(error); });
            }
            ui.label(widgets::faint("Preferences are read back after every change. Windows builds and organization policy can affect the visible result; use Review in Windows to inspect it. Existing or unrecognized policy is never overridden."));
            widgets::section(ui, "Your account preferences");
            for item in &inv.items {
                let Ok(p) = preference(&item.id) else { continue; };
                widgets::tight_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(widgets::icon(ic::SHIELD, 18.0, ACCENT));
                        ui.label(widgets::strong(p.name, 15.0));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(widgets::small("Review in Windows")).clicked() { util::open_uri(p.settings); }
                            if item.can_undo && ui.add_enabled(!busy, widgets::small("Undo PCSweep change")).clicked() {
                                requested = Some((format!("Restore {}?", p.name), "Restore the exact value saved before PCSweep changed it. A newer choice made elsewhere will not be overwritten.".into(), PrivacyAction::Undo(item.id.clone()), false));
                            } else if item.can_disable && ui.add_enabled(!busy, widgets::small("Turn off")).clicked() {
                                requested = Some((format!("Turn off {}?", p.name), p.description.into(), PrivacyAction::Disable { id: item.id.clone(), expected: item.value.clone() }, false));
                            }
                        });
                    });
                    ui.label(widgets::dim(p.description));
                    ui.label(RichText::new(&item.status).color(if item.can_disable { WARN } else { TEXT_DIM }).size(12.5));
                });
                ui.add_space(4.0);
            }
            widgets::section(ui, "Personalization and permissions");
            widgets::card(ui, |ui| {
                ui.label(widgets::strong("Personalized offers and optional diagnostics", 15.0));
                ui.label(widgets::dim("Review personalized offers (formerly Tailored experiences), Settings suggestions and optional diagnostic data. Windows still sends required diagnostic data on Home and Pro. These controls do not remove malware."));
                ui.horizontal_wrapped(|ui| {
                    if ui.add(widgets::small("Recommendations and offers")).clicked() { util::open_uri("ms-settings:privacy-general"); }
                    if ui.add(widgets::small("Diagnostic data")).clicked() { util::open_uri("ms-settings:privacy-feedback"); }
                    if ui.add(widgets::small("Start recommendations")).clicked() { util::open_uri("ms-settings:personalization-start"); }
                    if ui.add(widgets::small("Search permissions")).clicked() { util::open_uri("ms-settings:search-permissions"); }
                });
                ui.add_space(5.0);
                ui.label(widgets::strong("App permissions", 15.0));
                ui.horizontal_wrapped(|ui| {
                    for (label, uri) in [("Camera", "ms-settings:privacy-webcam"), ("Microphone", "ms-settings:privacy-microphone"), ("Location", "ms-settings:privacy-location")] {
                        if ui.add(widgets::small(label)).clicked() { util::open_uri(uri); }
                    }
                });
            });
            widgets::section(ui, "AI privacy");
            widgets::card(ui, |ui| {
                ui.label(widgets::strong("Recall and snapshots", 15.0));
                ui.label(widgets::dim(&inv.recall));
                ui.label(widgets::faint("Recall is opt-in and processes snapshots locally. Review saving, app/site filters, storage and deletion in Windows. Windows Hello protects changes; PCSweep does not read your snapshots."));
                if ui.add_enabled(inv.build >= 26100, widgets::small("Review Recall and snapshots")).clicked() { util::open_uri("ms-settings:privacy-recall"); }
            });
            widgets::section(ui, "Clipboard privacy");
            widgets::card(ui, |ui| {
                ui.label(widgets::strong("Clipboard history and cloud sync", 15.0));
                ui.label(widgets::dim(&inv.clipboard));
                ui.label(widgets::faint("History and synchronization are separate choices. Clearing the current clipboard does not delete pinned history or cloud items; review those in Windows."));
                ui.horizontal_wrapped(|ui| {
                    if ui.add(widgets::small("Review history and sync")).clicked() { util::open_uri("ms-settings:clipboard"); }
                    if ui.add_enabled(!busy, widgets::small("Clear current clipboard…")).clicked() {
                        requested = Some(("Clear the current clipboard?".into(), "The content currently available for Paste will be removed. Clipboard history and cloud items are kept. This cannot be undone.".into(), PrivacyAction::ClearClipboard, true));
                    }
                });
            });
            ui.add_space(15.0);
        });
        if let Some((title, body, action, danger)) = requested {
            self.modal = Some(Modal {
                title,
                body,
                details: Vec::new(),
                note: if danger {
                    None
                } else {
                    Some("Only this Windows account changes. Sign-out may be needed. Your original preference is saved before applying the change.".into())
                },
                confirm: if danger {
                    "Clear clipboard".into()
                } else {
                    "Confirm change".into()
                },
                danger,
                action: Action::Privacy(action),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryStore {
        values: HashMap<String, RawValue>,
        saved: Vec<u8>,
        fail_journal: bool,
        fail_write: bool,
        ignore_write: bool,
    }
    impl Store for MemoryStore {
        fn read(&self, id: &str) -> Result<Option<RawValue>, String> {
            Ok(self.values.get(id).cloned())
        }
        fn write(&mut self, id: &str, value: Option<&RawValue>) -> Result<(), String> {
            if self.fail_write {
                return Err("write denied".into());
            }
            if self.ignore_write {
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
        fn journal(&self) -> Result<Journal, String> {
            if self.saved.is_empty() {
                Ok(Journal::default())
            } else {
                Journal::decode(&self.saved)
            }
        }
        fn save_journal(&mut self, journal: &Journal) -> Result<(), String> {
            if self.fail_journal {
                return Err("journal denied".into());
            }
            self.saved = serde_json::to_vec(journal).unwrap();
            Ok(())
        }
    }

    #[test]
    fn undo_preserves_value_type_bytes_and_absence() {
        for original in [
            None,
            Some(RawValue::dword(1)),
            Some(RawValue {
                kind: 3,
                bytes: vec![0, 7, 255],
            }),
        ] {
            let mut store = MemoryStore::default();
            if let Some(value) = &original {
                store.values.insert("advertising_id".into(), value.clone());
            }
            apply(&mut store, "advertising_id", original.clone()).unwrap();
            assert_eq!(
                store.read("advertising_id").unwrap(),
                Some(RawValue::dword(0))
            );
            undo(&mut store, "advertising_id").unwrap();
            assert_eq!(store.read("advertising_id").unwrap(), original);
            assert!(store.journal().unwrap().entries.is_empty());
        }
    }

    #[test]
    fn journal_failure_prevents_any_change() {
        let mut store = MemoryStore {
            fail_journal: true,
            ..Default::default()
        };
        assert!(apply(&mut store, "advertising_id", None).is_err());
        assert!(store.values.is_empty());
    }

    #[test]
    fn failed_write_keeps_recoverable_undo_and_newer_choice_is_preserved() {
        let mut store = MemoryStore {
            fail_write: true,
            ..Default::default()
        };
        assert!(apply(&mut store, "advertising_id", None).is_err());
        assert_eq!(store.journal().unwrap().entries.len(), 1);
        store.fail_write = false;
        undo(&mut store, "advertising_id").unwrap();
        apply(&mut store, "advertising_id", None).unwrap();
        store
            .values
            .insert("advertising_id".into(), RawValue::dword(2));
        assert!(undo(&mut store, "advertising_id").is_err());
        assert_eq!(
            store.read("advertising_id").unwrap(),
            Some(RawValue::dword(2))
        );
    }

    #[test]
    fn confirmation_and_journal_scope_cannot_redirect_writes() {
        let mut store = MemoryStore::default();
        store
            .values
            .insert("advertising_id".into(), RawValue::dword(1));
        assert!(apply(&mut store, "advertising_id", None).is_err());
        assert!(apply(&mut store, r"Software\anything", None).is_err());
        let bytes = br#"{"entries":[{"id":"arbitrary_key","before":null,"after":{"kind":4,"bytes":[0,0,0,0]}}]}"#;
        assert!(Journal::decode(bytes).is_err());
    }

    #[test]
    fn malformed_journal_and_inapplicable_builds_fail_closed() {
        assert!(Journal::decode(b"not JSON").is_err());
        assert!(!available(preference("start_suggestions").unwrap(), 26100));
        assert!(available(preference("start_suggestions").unwrap(), 19045));
        assert!(!available(preference("advertising_id").unwrap(), 0));
    }

    #[test]
    fn readback_failure_retains_original_undo_data() {
        let mut store = MemoryStore {
            ignore_write: true,
            ..Default::default()
        };
        assert!(apply(&mut store, "advertising_id", None).is_err());
        assert_eq!(store.journal().unwrap().entries.len(), 1);
        assert!(store.read("advertising_id").unwrap().is_none());
        store.ignore_write = false;
        undo(&mut store, "advertising_id").unwrap();
        assert!(store.journal().unwrap().entries.is_empty());
    }

    #[test]
    fn interrupted_undo_can_be_repeated_without_losing_the_original() {
        let mut store = MemoryStore::default();
        store
            .values
            .insert("advertising_id".into(), RawValue::dword(1));
        apply(&mut store, "advertising_id", Some(RawValue::dword(1))).unwrap();
        store.fail_journal = true;
        // Restoring succeeds but saving completion fails. The next attempt
        // recognizes the restored bytes and safely completes bookkeeping.
        assert!(undo(&mut store, "advertising_id").is_err());
        assert_eq!(
            store.read("advertising_id").unwrap(),
            Some(RawValue::dword(1))
        );
        assert_eq!(store.journal().unwrap().entries.len(), 1);
        store.fail_journal = false;
        undo(&mut store, "advertising_id").unwrap();
        assert!(store.journal().unwrap().entries.is_empty());
    }

    #[test]
    fn registry_snapshot_and_flush_use_only_a_disposable_test_key() {
        struct TestKey(String);
        impl Drop for TestKey {
            fn drop(&mut self) {
                let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
            }
        }
        let path = TestKey(format!(
            r"Software\PCSweepTests\Privacy-{}",
            std::process::id()
        ));
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = root.create_subkey(&path.0).unwrap();
        let original = RawValue {
            kind: REG_BINARY as u32,
            bytes: vec![0, 7, 255],
        };
        key.set_raw_value("Snapshot", &original.to_registry().unwrap())
            .unwrap();
        drop(key);
        let key = root
            .open_subkey_with_flags(&path.0, KEY_SET_VALUE | KEY_QUERY_VALUE)
            .unwrap();
        flush_key(&key).unwrap();
        assert_eq!(
            raw_value(&root, &path.0, "Snapshot").unwrap(),
            Some(original)
        );
        key.delete_value("Snapshot").unwrap();
        flush_key(&key).unwrap();
        assert!(raw_value(&root, &path.0, "Snapshot").unwrap().is_none());
    }
}
