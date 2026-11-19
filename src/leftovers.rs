//! Evidence-based post-uninstall review. No fuzzy matches or automatic deletion.
//! Inspired by BCUninstaller's public confidence/ownership design; implemented
//! independently in Rust. See docs/UNINSTALLER_RESEARCH.md.
use crate::{
    apps::{self, InstalledApp},
    journal::{Act, Outcome, Session},
    scan_cache,
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, ScrollArea};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use winreg::{enums::*, RegKey, RegValue};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Target {
    pub name: String,
    pub publisher: String,
    pub location: String,
    #[serde(default)]
    pub location_reported: bool,
}
impl From<&InstalledApp> for Target {
    fn from(a: &InstalledApp) -> Self {
        Self {
            name: a.name.clone(),
            publisher: a.publisher.clone(),
            location: a.location.clone(),
            location_reported: a.location_reported,
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Confidence {
    High,
    Review,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Value {
    name: String,
    kind: u32,
    bytes: Vec<u8>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct KeyTree {
    values: Vec<Value>,
    children: Vec<(String, KeyTree)>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RegistryItem {
    machine: bool,
    path: String,
    tree: KeyTree,
}
#[derive(Clone, Debug)]
pub enum Item {
    Folder {
        path: PathBuf,
        signature: u64,
        bytes: u64,
    },
    Registry(RegistryItem),
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub item: Item,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
}
impl Candidate {
    fn label(&self) -> String {
        match &self.item {
            Item::Folder { path, .. } => path.display().to_string(),
            Item::Registry(k) => format!("{}\\{}", if k.machine { "HKLM" } else { "HKCU" }, k.path),
        }
    }
}
#[derive(Default)]
pub struct Review {
    pub candidates: Vec<Candidate>,
    pub notes: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct BackupArchive {
    backups: Vec<Backup>,
}
#[derive(Serialize, Deserialize)]
struct Backup {
    target: Target,
    items: Vec<RegistryItem>,
}
pub struct RestorePlan {
    pub stamp: u64,
    pub product: String,
    pub paths: Vec<String>,
}
#[derive(Default)]
pub struct State {
    pub target: Option<Target>,
    pub restore: Task<Result<Option<Target>, String>>,
    pub restore_review: Task<Result<RestorePlan, String>>,
    pub scan: Task<Result<Review, String>>,
    pub removal: Task<Result<String, String>>,
    pub launch: Task<Result<(), String>>,
    pub review: Option<Review>,
    pub selected: HashSet<usize>,
    pub error: Option<String>,
    pub message: Option<String>,
}
impl State {
    pub fn busy(&self) -> bool {
        self.launch.running() || self.removal.running()
    }
}
fn token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn product_name(s: &str) -> bool {
    let t = token(s);
    t.len() >= 4
        && ![
            "microsoft",
            "windows",
            "policies",
            "classes",
            "wow6432node",
            "clients",
            "registeredapplications",
            "commonfiles",
            "shared",
            "applications",
            "programs",
            "data",
            "software",
            "cache",
            "temp",
            "packages",
            "steamapps",
            "games",
            "users",
            "public",
            "documents",
            "downloads",
            "desktop",
            "pcsweep",
        ]
        .contains(&t.as_str())
        && !s.contains(['\\', '/', ':', '\0'])
        && s.trim() == s
        && !matches!(s, "." | "..")
}
fn same_app(a: &InstalledApp, t: &Target) -> bool {
    token(&a.name) == token(&t.name)
        && (t.publisher.is_empty() || token(&a.publisher) == token(&t.publisher))
}
fn overlaps(a: &Path, b: &Path) -> bool {
    let a = util::norm(a);
    let b = util::norm(b);
    util::is_within(&a, &b) || util::is_within(&b, &a)
}
fn local_safe(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    matches!(path.components().next(),Some(Component::Prefix(p)) if matches!(p.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_)))
        && path.is_absolute()
        && path
            .components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir))
}
fn folder_allowed(path: &Path, t: &Target, installed: &[InstalledApp]) -> bool {
    if !util::fixed_local(path)
        || !local_safe(path)
        || !product_name(&path.file_name().unwrap_or_default().to_string_lossy())
    {
        return false;
    }
    let mut planned = Vec::new();
    if !t.location.is_empty() {
        planned.push(PathBuf::from(&t.location));
    }
    if product_name(&t.name) {
        for root in ["LOCALAPPDATA", "APPDATA", "ProgramData"] {
            if let Some(base) = util::env(root) {
                planned.push(base.join(&t.name));
                if product_name(&t.publisher) {
                    planned.push(base.join(&t.publisher).join(&t.name));
                }
            }
        }
    }
    if !planned.iter().any(|p| util::norm(p) == util::norm(path)) {
        return false;
    }
    if path.components().any(|p| {
        [
            "windowsapps",
            "commonfiles",
            "windowsdefender",
            "windowspowershell",
            "dotnet",
            "windowskits",
            "windowsnt",
            "referenceassemblies",
        ]
        .contains(&token(&p.as_os_str().to_string_lossy()).as_str())
    }) {
        return false;
    }
    if ["runtime", "redistributable", "driver"]
        .iter()
        .any(|s| token(&t.name).contains(s))
    {
        return false;
    }
    let f = util::folders();
    // A leftover is confined to a product folder in a known application-data
    // root or a previously inventoried product install location. Never a parent.
    let bases: Vec<_> = [
        "LOCALAPPDATA",
        "APPDATA",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
    ]
    .iter()
    .filter_map(|n| util::env(n))
    .collect();
    let n = util::norm(path);
    let known = bases
        .iter()
        .any(|b| n != util::norm(b) && util::is_within(&n, &util::norm(b)));
    let exact_install = !t.location.is_empty()
        && n == util::norm(Path::new(&t.location))
        && token(&path.file_name().unwrap_or_default().to_string_lossy()) == token(&t.name);
    if (!t.location_reported && !exact_install && n == util::norm(Path::new(&t.location)))
        || (!known && !exact_install)
    {
        return false;
    }
    if n == util::norm(&f.profile) || util::personal_overlap(path) {
        return false;
    }
    if let Some(w) = util::env("WINDIR") {
        if overlaps(path, &w) {
            return false;
        }
    }
    if bases.iter().any(|b| util::is_within(&util::norm(b), &n)) {
        return false;
    }
    if installed
        .iter()
        .any(|a| !a.location.is_empty() && overlaps(path, Path::new(&a.location)))
    {
        return false;
    }
    // Name collision with another product/publisher is a hard exclusion.
    if installed.iter().any(|a| token(&a.name) == token(&t.name)) {
        return false;
    }
    true
}
pub(crate) fn snapshot_folder(path: &Path) -> Result<(u64, u64, bool), String> {
    use std::os::windows::fs::MetadataExt;
    for ancestor in path.ancestors() {
        let md = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        if util::is_reparse(&md) {
            return Err("Links and junctions are excluded".into());
        }
    }
    let start = Instant::now();
    let mut todo = vec![path.to_path_buf()];
    let mut rows = Vec::new();
    let mut bytes = 0u64;
    let mut code = false;
    while let Some(p) = todo.pop() {
        if rows.len() > 100_000 || start.elapsed() > Duration::from_secs(10) {
            return Err(
                "Folder is too large for a bounded leftover review; inspect it with Space analyzer"
                    .into(),
            );
        }
        let md = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if util::is_reparse(&md) || md.file_attributes() & util::CLOUD_MASK != 0 {
            return Err("Contains a link or cloud placeholder; preserved".into());
        }
        if md.is_dir() {
            for e in fs::read_dir(&p).map_err(|e| e.to_string())? {
                if todo.len() + rows.len() > 100_000 || start.elapsed() > Duration::from_secs(10) {
                    return Err("Folder exceeds the bounded review limit".into());
                }
                todo.push(e.map_err(|e| e.to_string())?.path());
            }
        } else if md.is_file() {
            bytes = bytes.saturating_add(md.len());
            code |= p.extension().is_some_and(|e| {
                ["exe", "dll", "sys"]
                    .iter()
                    .any(|x| e.eq_ignore_ascii_case(x))
            });
        }
        rows.push((
            p,
            md.file_size(),
            md.creation_time(),
            md.last_write_time(),
            md.file_attributes(),
        ));
    }
    rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rows.hash(&mut h);
    Ok((h.finish(), bytes, code))
}
fn reg_root(machine: bool) -> RegKey {
    RegKey::predef(if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    })
}
fn open_key(root: &RegKey, path: &str, access: u32) -> Result<RegKey, String> {
    // Open each component as a link object, never through a registry symlink.
    let mut current = root
        .open_subkey_with_flags("", access)
        .map_err(|e| e.to_string())?;
    for component in path.split('\\') {
        if component.is_empty() {
            return Err("Invalid registry path".into());
        }
        let mut raw = 0;
        let name: Vec<_> = component.encode_utf16().chain(Some(0)).collect();
        let error = unsafe {
            windows_sys::Win32::System::Registry::RegOpenKeyExW(
                current.raw_handle() as _,
                name.as_ptr(),
                windows_sys::Win32::System::Registry::REG_OPTION_OPEN_LINK,
                access,
                &mut raw,
            )
        };
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error as i32).to_string());
        }
        let next = RegKey::predef(raw as _);
        for v in next.enum_values() {
            if v.map_err(|e| e.to_string())?.1.vtype == REG_LINK {
                return Err("Registry symbolic links are excluded".into());
            }
        }
        current = next;
    }
    Ok(current)
}
fn create_key(root: &RegKey, path: &str) -> Result<RegKey, String> {
    let mut current = root
        .open_subkey_with_flags("", KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)
        .map_err(|e| e.to_string())?;
    for component in path.split('\\') {
        // Creation is component-by-component. Existing link objects are refused
        // before a subkey can be created through them.
        let (next, _) = current
            .create_subkey_with_flags(component, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)
            .map_err(|e| e.to_string())?;
        drop(next);
        current = open_key(&current, component, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)?;
    }
    Ok(current)
}
fn key_tree(key: &RegKey, depth: usize, budget: &mut usize) -> Result<KeyTree, String> {
    if depth > 8 {
        *budget = 0;
        return Err("Registry tree is too deep".into());
    }
    let mut values = Vec::new();
    let mut children = Vec::new();
    for value in key.enum_values() {
        let (name, v) = value.map_err(|e| e.to_string())?;
        if v.vtype == REG_LINK {
            return Err("Registry links are excluded".into());
        }
        let cost = v.bytes.len() + name.len() + 32;
        *budget = budget
            .checked_sub(cost)
            .ok_or("Registry review size limit reached")?;
        values.push(Value {
            name,
            kind: v.vtype as u32,
            bytes: v.bytes,
        });
    }
    for name in key.enum_keys() {
        let name = name.map_err(|e| e.to_string())?;
        *budget = budget
            .checked_sub(name.len() + 64)
            .ok_or("Registry review size limit reached")?;
        let k = open_key(key, &name, KEY_READ | KEY_WOW64_64KEY)?;
        children.push((name, key_tree(&k, depth + 1, budget)?));
    }
    values.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    children.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(KeyTree { values, children })
}
fn registry_paths(t: &Target) -> Vec<String> {
    let mut out = Vec::new();
    if !product_name(&t.name) {
        return out;
    }
    for prefix in [r"Software", r"Software\WOW6432Node"] {
        out.push(format!("{prefix}\\{}", t.name));
        if product_name(&t.publisher) {
            out.push(format!("{prefix}\\{}\\{}", t.publisher, t.name));
        }
    }
    out
}
fn registry_allowed(k: &RegistryItem, t: &Target, installed: &[InstalledApp]) -> bool {
    registry_paths(t).contains(&k.path)
        && !installed.iter().any(|a| {
            token(&a.name) == token(&t.name)
                || (!t.publisher.is_empty() && token(&a.publisher) == token(&t.publisher))
        })
}
pub fn scan(t: &Target) -> Result<Review, String> {
    let installed = apps::ownership_inventory();
    if installed.iter().any(|a| same_app(a, t)) {
        return Err("The app is still registered as installed. Finish its uninstaller, then check again. No leftovers were removed.".into());
    }
    let mut r = Review::default();
    let mut paths = Vec::new();
    if !t.location.is_empty() {
        paths.push(PathBuf::from(&t.location));
    }
    if product_name(&t.name) {
        for root in ["LOCALAPPDATA", "APPDATA", "ProgramData"] {
            if let Some(base) = util::env(root) {
                paths.push(base.join(&t.name));
                if product_name(&t.publisher) {
                    paths.push(base.join(&t.publisher).join(&t.name));
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    for p in paths {
        if !p.exists() {
            continue;
        }
        if !folder_allowed(&p, t, &installed) {
            r.notes.push(format!(
                "Preserved shared or protected location: {}",
                p.display()
            ));
            continue;
        }
        match snapshot_folder(&p) {
            Ok((signature, bytes, code)) => {
                let exact =
                    !t.location.is_empty() && util::norm(&p) == util::norm(Path::new(&t.location));
                r.candidates.push(Candidate {
                    item: Item::Folder {
                        path: p,
                        signature,
                        bytes,
                    },
                    confidence: if exact && t.location_reported && !code {
                        Confidence::High
                    } else {
                        Confidence::Review
                    },
                    evidence: vec![
                        if exact {
                            "Matches the inventoried installation folder"
                        } else {
                            "Exact product-name folder in app data"
                        }
                        .into(),
                        "No remaining installed app claims this location".into(),
                        if code {
                            "Executable files remain: review carefully"
                        } else {
                            "May contain preferences, saves or other app data"
                        }
                        .into(),
                    ],
                });
            }
            Err(e) => r.notes.push(format!("{}: {e}", p.display())),
        }
    }
    for machine in [false, true] {
        for path in registry_paths(t) {
            let key = match open_key(&reg_root(machine), &path, KEY_READ | KEY_WOW64_64KEY) {
                Ok(k) => k,
                Err(_) => continue,
            };
            match key_tree(&key, 0, &mut (1024 * 1024)) {
                Ok(tree) => {
                    let item = RegistryItem {
                        machine,
                        path,
                        tree,
                    };
                    if registry_allowed(&item, t, &installed) {
                        r.candidates.push(Candidate {
                            item: Item::Registry(item),
                            confidence: Confidence::Review,
                            evidence: vec![
                                "Exact product-name software registry key".into(),
                                "Publisher is not used by another registered app".into(),
                                "A typed-value backup is required before removal".into(),
                            ],
                        });
                    }
                }
                Err(e) => r.notes.push(format!("Registry folder preserved: {e}")),
            }
        }
    }
    Ok(r)
}
fn erase_tree(key: &RegKey, tree: &KeyTree) -> Result<(), String> {
    // Delete only snapshotted values/subkeys. Concurrent additions are kept.
    for (name, child) in &tree.children {
        let k = open_key(key, name, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)?;
        erase_tree(&k, child)?;
        drop(k);
        key.delete_subkey(name).map_err(|e| e.to_string())?;
    }
    for v in &tree.values {
        let current = key.get_raw_value(&v.name).map_err(|e| e.to_string())?;
        if current.vtype as u32 != v.kind || current.bytes != v.bytes {
            return Err("Registry value changed; preserved".into());
        }
        key.delete_value(&v.name).map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn remove(t: Target, items: Vec<Candidate>) -> Result<String, String> {
    let installed = apps::ownership_inventory();
    if installed.iter().any(|a| same_app(a, &t)) {
        return Err("The app is installed again; nothing was removed".into());
    }
    let keys: Vec<_> = items
        .iter()
        .filter_map(|c| {
            if let Item::Registry(k) = &c.item {
                Some(k.clone())
            } else {
                None
            }
        })
        .collect();
    if !keys.is_empty() {
        let mut archive = scan_cache::load::<BackupArchive>("leftover-registry-backups.json")?
            .map(|s| s.data)
            .unwrap_or(BackupArchive {
                backups: Vec::new(),
            });
        if archive.backups.len() > 16 {
            return Err("Registry backup archive is invalid; no cleanup was started".into());
        }
        archive.backups.push(Backup {
            target: t.clone(),
            items: keys,
        });
        if archive.backups.len() > 16 {
            archive.backups.remove(0);
        }
        scan_cache::save(
            "leftover-registry-backups.json",
            scan_cache::now(),
            &archive,
        )?;
    }

    let mut journal = if items.iter().any(|c| matches!(c.item, Item::Folder { .. })) {
        Some(Session::begin("Uninstaller leftovers")?)
    } else {
        None
    };
    let mut done = 0;
    let mut failures = Vec::new();
    for c in items {
        match c.item {
            Item::Folder {
                path,
                signature,
                bytes,
            } => {
                if !folder_allowed(&path, &t, &installed)
                    || !snapshot_folder(&path).is_ok_and(|s| s.0 == signature)
                {
                    failures.push(format!("{} changed or is protected", path.display()));
                    continue;
                }
                let journal = journal.as_mut().ok_or("Cleanup history is unavailable")?;
                let ids = journal.intend(
                    "Reviewed uninstaller leftover",
                    Act::Recycle,
                    &[(path.clone(), bytes)],
                )?;
                match trash::delete(&path) {
                    Ok(()) => {
                        done += 1;
                        journal.finish(&[(ids[0], Outcome::Recycled, None)])?;
                    }
                    Err(e) => {
                        journal.finish(&[(ids[0], Outcome::Failed, Some(e.to_string()))])?;
                        failures.push(e.to_string());
                    }
                }
            }
            Item::Registry(k) => {
                let result = (|| {
                    if !registry_allowed(&k, &t, &installed) {
                        return Err("Registry ownership changed; preserved".into());
                    }
                    let root = reg_root(k.machine);
                    let key = open_key(&root, &k.path, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY)?;
                    if key_tree(&key, 0, &mut (1024 * 1024))? != k.tree {
                        return Err("Registry key changed since review; preserved".into());
                    }
                    erase_tree(&key, &k.tree)?;
                    drop(key);
                    root.delete_subkey(&k.path).map_err(|e| e.to_string())?;
                    Ok::<_, String>(())
                })();
                match result {
                    Ok(()) => {
                        done += 1;
                        util::log(format!(
                            "Removed reviewed leftover registry key {} (backup retained)",
                            k.path
                        ));
                    }
                    Err(e) => failures.push(e),
                }
            }
        }
    }
    let summary=format!("Removed {done} reviewed leftovers. {} kept or failed. Files are in the Recycle Bin; registry removals have a local backup. {}",failures.len(),failures.join("; "));
    util::log(&summary);
    Ok(summary)
}
fn restore_tree(key: &RegKey, tree: &KeyTree) -> Result<(), String> {
    for v in &tree.values {
        let kind = match v.kind {
            0 => REG_NONE,
            1 => REG_SZ,
            2 => REG_EXPAND_SZ,
            3 => REG_BINARY,
            4 => REG_DWORD,
            5 => REG_DWORD_BIG_ENDIAN,
            6 => return Err("Registry links are excluded".into()),
            7 => REG_MULTI_SZ,
            8 => REG_RESOURCE_LIST,
            9 => REG_FULL_RESOURCE_DESCRIPTOR,
            10 => REG_RESOURCE_REQUIREMENTS_LIST,
            11 => REG_QWORD,
            _ => return Err("Unsupported registry value type".into()),
        };
        // Keep anything that appeared after cleanup, including changed values.
        match key.get_raw_value(&v.name) {
            Ok(current) if current.bytes == v.bytes && current.vtype == kind => {}
            Ok(_) => return Err("A registry value now differs; it was preserved".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                key.set_raw_value(
                    &v.name,
                    &RegValue {
                        bytes: v.bytes.clone(),
                        vtype: kind,
                    },
                )
                .map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    for (name, t) in &tree.children {
        let k = create_key(key, name)?;
        restore_tree(&k, t)?;
    }
    Ok(())
}
fn valid_tree(t: &KeyTree, depth: usize, budget: &mut usize) -> bool {
    if depth > 8 {
        return false;
    }
    for v in &t.values {
        if v.name.contains('\0') || v.kind > 11 || v.kind == 6 {
            return false;
        }
        let Some(n) = budget.checked_sub(v.bytes.len() + v.name.len() + 32) else {
            return false;
        };
        *budget = n;
    }
    for (name, child) in &t.children {
        let Some(n) = budget.checked_sub(name.len() + 64) else {
            return false;
        };
        *budget = n;
        if name.is_empty()
            || name.contains(['\\', '/', '\0'])
            || !valid_tree(child, depth + 1, budget)
        {
            return false;
        }
    }
    true
}
pub fn restore_plan() -> Result<RestorePlan, String> {
    let stored = scan_cache::load::<BackupArchive>("leftover-registry-backups.json")?
        .ok_or("No registry backup is available")?;
    if stored.data.backups.len() > 16 {
        return Err("Registry backup archive is invalid".into());
    }
    let b = stored
        .data
        .backups
        .last()
        .ok_or("No registry backup is available")?;
    if b.items.len() > 8 || b.items.is_empty() {
        return Err("Registry backup contents are invalid".into());
    }
    for k in &b.items {
        if !registry_paths(&b.target).contains(&k.path)
            || !valid_tree(&k.tree, 0, &mut (1024 * 1024))
        {
            return Err("Registry backup failed validation".into());
        }
    }
    Ok(RestorePlan {
        stamp: stored.saved,
        product: b.target.name.clone(),
        paths: b
            .items
            .iter()
            .map(|k| format!("{}\\{}", if k.machine { "HKLM" } else { "HKCU" }, k.path))
            .collect(),
    })
}
pub fn restore_registry(expected: u64) -> Result<String, String> {
    let stored = scan_cache::load::<BackupArchive>("leftover-registry-backups.json")?
        .ok_or("No registry backup is available")?;
    if stored.saved != expected {
        return Err("The backup changed. Review it again before restoring.".into());
    }
    let mut archive = stored.data;
    if archive.backups.len() > 16 {
        return Err("Registry backup archive is invalid".into());
    }
    let b = archive
        .backups
        .last()
        .ok_or("No registry backup is available")?;
    for k in &b.items {
        if !registry_paths(&b.target).contains(&k.path)
            || !valid_tree(&k.tree, 0, &mut (1024 * 1024))
        {
            return Err("Registry backup failed validation; nothing more was restored".into());
        }
    }
    for k in &b.items {
        let root = create_key(&reg_root(k.machine), &k.path)?;
        restore_tree(&root, &k.tree)?;
    }
    // Remove only this successfully restored generation, retaining older Undo.
    archive.backups.pop();
    scan_cache::save(
        "leftover-registry-backups.json",
        scan_cache::now(),
        &archive,
    )?;
    Ok("Restored saved registry values. Existing, different values were preserved.".into())
}
impl crate::app::App {
    pub fn poll_leftovers(&mut self, ctx: &egui::Context) {
        if self.modal.is_none() && !self.mutations_running() {
            if let Some(result) = self.apps.leftovers.restore_review.poll() {
                match result.and_then(|r|r) {
                    Ok(plan)=>self.modal=Some(crate::app::Modal{title:format!("Restore {} registry backup?",plan.product),body:"Restore the listed registry keys from the last cleanup backup. Differing values added since cleanup are preserved; a conflict leaves the backup available. Files can be restored separately from the Recycle Bin.".into(),details:plan.paths,note:None,confirm:"Restore backup".into(),danger:false,action:crate::app::Action::RestoreLeftoverRegistry(plan.stamp)}),
                    Err(e)=>self.apps.leftovers.error=Some(e),
                }
            }
        }
        if let Some(r) = self.apps.leftovers.restore.poll() {
            if self.apps.leftovers.target.is_none() {
                if let Ok(Ok(Some(target))) = r {
                    self.apps.leftovers.target = Some(target);
                }
            }
        }
        if let Some(r) = self.apps.leftovers.launch.poll() {
            match r.and_then(|r|r){Ok(())=>self.apps.leftovers.message=Some("The uninstaller has returned. Check leftovers when its removal is finished; some uninstallers start another process.".into()),Err(e)=>self.apps.leftovers.error=Some(e)}
            self.start_apps_load(ctx);
        }
        if let Some(r) = self.apps.leftovers.scan.poll() {
            match r.and_then(|r| r) {
                Ok(review) => {
                    self.apps.leftovers.review = Some(review);
                    self.apps.leftovers.selected.clear();
                    self.apps.leftovers.error = None;
                }
                Err(e) => self.apps.leftovers.error = Some(e),
            }
        }
        if let Some(r) = self.apps.leftovers.removal.poll() {
            match r.and_then(|r| r) {
                Ok(message) => {
                    self.apps.leftovers.message = Some(message);
                    self.apps.leftovers.review = None;
                    self.apps.leftovers.selected.clear();
                }
                Err(e) => self.apps.leftovers.error = Some(e),
            }
        }
    }
    pub(crate) fn leftovers_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let state = &mut self.apps.leftovers;
        let Some(target) = state.target.clone() else {
            return;
        };
        let mut modal = None;
        egui::CollapsingHeader::new("Uninstaller leftovers").default_open(true).show(ui,|ui| {
            widgets::tight_card(ui,|ui| {
                ui.label(widgets::strong(format!("After uninstalling {}",target.name),16.0));
                ui.add(egui::Label::new("Review exact product folders and registry keys after removal. Confidence is evidence, not a guarantee. Preferences and saved data may be included. Nothing is selected automatically.").wrap());
                if let Some(error)=&state.error {ui.add(egui::Label::new(egui::RichText::new(error).color(color_warning())).wrap());}
                if let Some(message)=&state.message {ui.add(egui::Label::new(message).wrap());}
                let busy=state.busy()||state.scan.running()||state.restore_review.running();
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!busy,widgets::small("Check leftovers")).clicked() {
                        let t=target.clone();state.error=None;
                        state.scan=Task::spawn(&ctx,move||scan(&t));
                    }
                    if ui.add_enabled(!busy,widgets::small("Restore last registry backup…")).clicked() {
                        state.error=None;state.restore_review=Task::spawn(&ctx,restore_plan);
                    }
                    if busy {ui.spinner();}
                });
                if let Some(review)=&state.review {
                    if review.candidates.is_empty() {
                        ui.add(egui::Label::new(widgets::dim("No eligible exact-match leftovers found. Shared, protected and uncertain locations are kept.")).wrap());
                    }
                    ScrollArea::vertical().id_salt("leftover_review").max_height((ui.available_height()*0.35).clamp(80.0,240.0)).auto_shrink([false,true]).show(ui,|ui| {
                        for(i,c)in review.candidates.iter().enumerate() {
                            ui.push_id(i,|ui| {
                                ui.horizontal(|ui| {
                                    let mut on=state.selected.contains(&i);
                                    if ui.add_enabled(!busy,egui::Checkbox::without_text(&mut on)).changed() {
                                        if on {state.selected.insert(i);} else {state.selected.remove(&i);}
                                    }
                                    ui.vertical(|ui| {
                                        let confidence=if c.confidence==Confidence::High {"High confidence"} else {"Needs review"};
                                        ui.label(widgets::strong(confidence,13.0));
                                        ui.add(egui::Label::new(c.label()).wrap());
                                        ui.add(egui::Label::new(widgets::faint(c.evidence.join(" · "))).wrap());
                                    });
                                });
                                ui.separator();
                            });
                        }
                        for note in &review.notes {ui.add(egui::Label::new(widgets::dim(note)).wrap());}
                    });
                    if ui.add_enabled(!state.selected.is_empty()&&!busy,widgets::small(format!("Review removal ({})…",state.selected.len()))).clicked() {
                        let mut indices:Vec<_>=state.selected.iter().copied().collect();indices.sort_unstable();
                        let chosen:Vec<_>=indices.into_iter().filter_map(|i|review.candidates.get(i).cloned()).collect();
                        modal=Some(crate::app::Modal {
                            title:"Remove reviewed app leftovers?".into(),
                            body:"Selected folders will move to the Recycle Bin. Registry keys are backed up before removal. Check the paths carefully: app preferences, profiles and saves can be included.".into(),
                            details:chosen.iter().map(Candidate::label).collect(),
                            note:Some("Shared paths, links, changes since the scan and newly installed apps are checked again before removal. The latest 16 registry cleanup backups are retained.".into()),
                            confirm:"Remove selected leftovers".into(),danger:true,
                            action:crate::app::Action::RemoveLeftovers(target.clone(),chosen),
                        });
                    }
                }
            });
        });
        if modal.is_some() {
            self.modal = modal;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(name: &str) -> Target {
        Target {
            name: name.into(),
            publisher: "PCSweep Test Publisher".into(),
            location: String::new(),
            location_reported: true,
        }
    }
    #[test]
    fn candidate_identity_never_accepts_generic_or_path_names() {
        for s in [
            "Microsoft",
            "Common Files",
            "Software",
            "Policies",
            "Classes",
            "Downloads",
            "..",
            r"Vendor\Product",
            "PCSweep",
        ] {
            assert!(!product_name(s), "{s}");
        }
        assert!(product_name("Example Product"));
        let t = target("Example Product");
        let paths = registry_paths(&t);
        assert!(paths.contains(&r"Software\Example Product".into()));
        assert!(!paths.iter().any(|p| p.ends_with(&t.publisher)));
    }
    #[test]
    fn shared_paths_and_name_collisions_are_excluded() {
        let mut t = target("Example Product");
        let base = util::env("LOCALAPPDATA").unwrap();
        let path = base.join(&t.name);
        t.location = path.display().to_string();
        assert!(folder_allowed(&path, &t, &[]));
        let mut installed = apps::preview_fixture().list.remove(0);
        installed.location = path.join("shared-child").display().to_string();
        assert!(!folder_allowed(&path, &t, &[installed.clone()]));
        installed.location.clear();
        installed.name = t.name.clone();
        assert!(!folder_allowed(&path, &t, &[installed]));
        assert!(!folder_allowed(&base, &t, &[]));
        assert!(!folder_allowed(&base.join("Other Product"), &t, &[]));
        assert!(!folder_allowed(Path::new(r"C:\Windows\System32"), &t, &[]));
    }
    #[test]
    fn folder_snapshot_detects_content_changes_without_reading_file_contents() {
        let dir =
            std::env::temp_dir().join(format!("pcsweep-leftover-manifest-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("data.bin"), b"one").unwrap();
        let before = snapshot_folder(&dir).unwrap();
        assert_eq!(before.1, 3);
        assert!(!before.2);
        fs::write(dir.join("more.exe"), b"two").unwrap();
        let after = snapshot_folder(&dir).unwrap();
        assert_ne!(before.0, after.0);
        assert!(after.2);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_registry_roundtrip_preserves_types_and_later_values() {
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let path = format!(r"Software\PCSweep-Test-Leftovers-{}", std::process::id());
        let k = create_key(&root, &path).unwrap();
        k.set_value("Text", &"original").unwrap();
        k.set_value("Number", &123u32).unwrap();
        let child = create_key(&k, "Child").unwrap();
        child
            .set_raw_value(
                "Binary",
                &RegValue {
                    bytes: vec![0, 255, 1],
                    vtype: REG_BINARY,
                },
            )
            .unwrap();
        drop(child);
        let tree = key_tree(&k, 0, &mut (1024 * 1024)).unwrap();
        assert!(valid_tree(&tree, 0, &mut (1024 * 1024)));
        erase_tree(&k, &tree).unwrap();
        assert_eq!(k.enum_values().count(), 0);
        assert_eq!(k.enum_keys().count(), 0);
        restore_tree(&k, &tree).unwrap();
        assert_eq!(key_tree(&k, 0, &mut (1024 * 1024)).unwrap(), tree);
        k.set_value("Text", &"outside change").unwrap();
        assert!(restore_tree(&k, &tree).is_err());
        assert_eq!(k.get_value::<String, _>("Text").unwrap(), "outside change");
        drop(k);
        root.delete_subkey_all(&path).unwrap();
    }
    #[test]
    fn backup_rejects_registry_links_and_escaping_child_names() {
        let mut tree = KeyTree {
            values: vec![Value {
                name: "SymbolicLinkValue".into(),
                kind: 6,
                bytes: vec![],
            }],
            children: Vec::new(),
        };
        assert!(!valid_tree(&tree, 0, &mut 1024));
        tree.values.clear();
        tree.children.push((
            r"..\Microsoft".into(),
            KeyTree {
                values: Vec::new(),
                children: Vec::new(),
            },
        ));
        assert!(!valid_tree(&tree, 0, &mut 1024));
    }
    #[test]
    fn native_registry_cleanup_has_persistent_reviewed_undo_and_generation_checks() {
        let name = format!("PCSweepLeftoversE2E{}", std::process::id());
        let t = target(&name);
        let path = format!("Software\\{name}");
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let key = create_key(&root, &path).unwrap();
        key.set_value("Saved preference", &42u32).unwrap();
        let tree = key_tree(&key, 0, &mut (1024 * 1024)).unwrap();
        drop(key);
        let item = Candidate {
            item: Item::Registry(RegistryItem {
                machine: false,
                path: path.clone(),
                tree,
            }),
            confidence: Confidence::Review,
            evidence: vec!["disposable test key".into()],
        };
        let result = remove(t, vec![item]).unwrap();
        assert!(result.starts_with("Removed 1"), "{result}");
        assert!(root.open_subkey(&path).is_err());
        let plan = restore_plan().unwrap();
        assert_eq!(plan.product, name);
        assert_eq!(plan.paths, vec![format!("HKCU\\{path}")]);
        assert!(restore_registry(plan.stamp.saturating_sub(1)).is_err());
        assert!(root.open_subkey(&path).is_err());
        restore_registry(plan.stamp).unwrap();
        assert_eq!(
            root.open_subkey(&path)
                .unwrap()
                .get_value::<u32, _>("Saved preference")
                .unwrap(),
            42
        );
        root.delete_subkey_all(&path).unwrap();
        assert!(restore_plan().is_err());
    }
}
