use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const ATTR_REPARSE: u32 = 0x0000_0400;
pub const ATTR_OFFLINE: u32 = 0x0000_1000;
pub const ATTR_RECALL_ON_OPEN: u32 = 0x0004_0000;
pub const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
/// Cloud placeholders (OneDrive "online-only" files) take no local disk space.
pub const CLOUD_MASK: u32 = ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS;

// ---------------------------------------------------------------- formatting

pub fn fmt_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{:.0} {}", v, UNITS[i])
    } else if v >= 10.0 {
        format!("{:.1} {}", v, UNITS[i])
    } else {
        format!("{:.2} {}", v, UNITS[i])
    }
}

pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn age_days(t: SystemTime) -> f64 {
    SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs_f64() / 86_400.0)
        .unwrap_or(0.0)
}

pub fn fmt_age(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "unknown".into() };
    let d = age_days(t);
    if d < 1.0 {
        "today".into()
    } else if d < 2.0 {
        "yesterday".into()
    } else if d < 31.0 {
        format!("{} days ago", d as u64)
    } else if d < 365.0 {
        let m = (d / 30.4).round().max(1.0) as u64;
        format!("{m} month{} ago", if m == 1 { "" } else { "s" })
    } else {
        let y = d / 365.25;
        if y < 1.95 {
            "1 year ago".into()
        } else {
            format!("{:.0} years ago", y.floor())
        }
    }
}

pub fn fmt_duration(secs: u64) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

pub fn fmt_date(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => chrono::DateTime::<chrono::Local>::from(t)
            .format("%b %d, %Y")
            .to_string(),
        None => "unknown".into(),
    }
}

// ---------------------------------------------------------------- paths

pub fn env(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Expands %VARIABLE% references. Unknown variables are left as-is.
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let name = &after[..end];
            match std::env::var(name) {
                Ok(v) if !name.is_empty() => out.push_str(&v),
                _ => {
                    out.push('%');
                    out.push_str(name);
                    out.push('%');
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// Case-insensitive Windows path key, including lexical `.`/`..` normalization.
/// Verbatim and ordinary UNC paths must produce the same key for protection checks.
pub fn norm(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(r) = s.strip_prefix(r"\\?\unc\") {
        s = format!(r"\\{r}");
    } else if let Some(r) = s.strip_prefix(r"\\?\") {
        s = r.to_string();
    }
    // Almost every scanned path is already clean; avoid allocating/rebuilding
    // each component on that hot path.
    if s.split('\\').any(|part| part == "." || part == "..") {
        let mut normalized = PathBuf::new();
        for part in Path::new(&s).components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    if matches!(
                        normalized.components().next_back(),
                        Some(Component::Normal(_))
                    ) {
                        normalized.pop();
                    } else if !normalized.has_root() {
                        normalized.push(part.as_os_str());
                    }
                }
                _ => normalized.push(part.as_os_str()),
            }
        }
        s = normalized.to_string_lossy().into_owned();
    }
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    s
}

/// True if `p` equals `root` or is inside it (both normalized).
pub fn is_within(p: &str, root: &str) -> bool {
    if root.is_empty() {
        return false;
    }
    if p == root {
        return true;
    }
    if root.ends_with('\\') {
        return p.starts_with(root);
    }
    p.starts_with(root) && p.as_bytes().get(root.len()) == Some(&b'\\')
}

fn within_any(p: &str, roots: &[String]) -> bool {
    roots.iter().any(|r| is_within(p, r))
}

pub fn is_reparse(md: &fs::Metadata) -> bool {
    md.file_type().is_symlink() || md.file_attributes() & ATTR_REPARSE != 0
}

pub struct Folders {
    downloads: Vec<String>,
    libraries: Vec<String>,
    system: Vec<String>,
    pub profile: PathBuf,
    pub downloads_main: PathBuf,
}

fn shell_folder(value: &str) -> Vec<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut out = Vec::new();
    for sub in [
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders",
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Shell Folders",
    ] {
        if let Ok(k) = hkcu.open_subkey(sub) {
            if let Ok(v) = k.get_value::<String, _>(value) {
                let v = expand_env(v.trim());
                if !v.is_empty() {
                    out.push(PathBuf::from(v));
                }
            }
        }
    }
    out
}

fn push_variants(list: &mut Vec<String>, p: &Path) {
    let n = norm(p);
    if !n.is_empty() && !list.contains(&n) {
        list.push(n);
    }
    if let Ok(c) = fs::canonicalize(p) {
        let n = norm(&c);
        if !n.is_empty() && !list.contains(&n) {
            list.push(n);
        }
    }
}

/// Known user/system folders, resolved once (including redirected locations).
pub fn folders() -> &'static Folders {
    static F: OnceLock<Folders> = OnceLock::new();
    F.get_or_init(|| {
        let profile = env("USERPROFILE").unwrap_or_else(|| PathBuf::from(r"C:\Users\Default"));

        let mut dl_paths = shell_folder("{374DE290-123F-4565-9164-39C4925E467B}");
        dl_paths.push(profile.join("Downloads"));
        let downloads_main = dl_paths
            .first()
            .cloned()
            .unwrap_or_else(|| profile.join("Downloads"));
        let mut downloads = Vec::new();
        for p in &dl_paths {
            push_variants(&mut downloads, p);
        }

        let mut libraries = Vec::new();
        for v in ["Desktop", "Personal", "My Pictures", "My Music", "My Video"] {
            for p in shell_folder(v) {
                push_variants(&mut libraries, &p);
            }
        }
        for d in [
            "Desktop",
            "Documents",
            "Pictures",
            "Music",
            "Videos",
            "OneDrive",
        ] {
            push_variants(&mut libraries, &profile.join(d));
        }
        for var in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
            if let Some(p) = env(var) {
                push_variants(&mut libraries, &p);
            }
        }

        let mut system = Vec::new();
        for var in [
            "SystemRoot",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "ProgramData",
        ] {
            if let Some(p) = env(var) {
                push_variants(&mut system, &p);
            }
        }

        Folders {
            downloads,
            libraries,
            system,
            profile,
            downloads_main,
        }
    })
}

pub fn is_downloads(p: &Path) -> bool {
    within_any(&norm(p), &folders().downloads)
}

/// The junk cleaner may only touch paths outside Downloads and personal libraries.
pub fn junk_delete_allowed(p: &Path) -> bool {
    let n = norm(p);
    let f = folders();
    !within_any(&n, &f.downloads) && !within_any(&n, &f.libraries)
}

pub(crate) fn personal_overlap(path: &Path) -> bool {
    let n = norm(path);
    let f = folders();
    f.downloads
        .iter()
        .chain(f.libraries.iter())
        .any(|p| is_within(&n, p) || is_within(p, &n))
}

/// Why a user-selected file/folder must not be moved to the Recycle Bin (fast, no disk access).
pub fn block_reason_fast(p: &Path) -> Option<&'static str> {
    let n = norm(p);
    let f = folders();
    if !p.is_absolute() {
        return Some("An absolute path is required");
    }
    if within_any(&n, &f.downloads) {
        return Some("In Downloads, which PCSweep never deletes from");
    }
    if within_any(&n, &f.system) {
        return Some("System or program folder. Use an uninstaller instead");
    }
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if matches!(
        name.as_str(),
        "pagefile.sys" | "hiberfil.sys" | "swapfile.sys" | "dumpstack.log" | "dumpstack.log.tmp"
    ) {
        return Some("Managed by Windows. See Tools");
    }
    if n.len() <= 3 || p.parent().is_none() {
        return Some("Drive root");
    }
    // Never allow removing a folder that *contains* something protected.
    if is_within(&norm(&f.profile), &n) {
        return Some("Contains your user profile");
    }
    if f.downloads.iter().any(|d| is_within(d, &n)) {
        return Some("Contains your Downloads folder");
    }
    if f.system
        .iter()
        .chain(f.libraries.iter())
        .any(|d| is_within(d, &n))
    {
        return Some("Contains a protected folder");
    }
    None
}

/// Reject links anywhere along the path, including a cache directory replaced
/// by a junction between scanning and cleaning. Metadata errors fail closed.
fn has_reparse_ancestor(p: &Path) -> Result<bool, String> {
    for ancestor in p.ancestors() {
        let md = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        if is_reparse(&md) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Full check, also resolving links/short names on disk.
pub fn block_reason(p: &Path) -> Option<&'static str> {
    if let Some(r) = block_reason_fast(p) {
        return Some(r);
    }
    match has_reparse_ancestor(p) {
        Ok(false) => {}
        Ok(true) => return Some("Links and junctions are protected"),
        Err(_) => return Some("The item could not be checked; scan again"),
    }
    match fs::canonicalize(p) {
        Ok(c) => block_reason_fast(&c),
        Err(_) => Some("The item could not be checked; scan again"),
    }
}

pub fn in_appdata(p: &Path) -> bool {
    norm(p).contains(r"\appdata\")
}

// ---------------------------------------------------------------- deleting

/// Permanently deletes a reviewed junk file only if it still matches the scan.
/// Canonical protection and ancestor checks are repeated at deletion time.
pub fn delete_junk_file(
    p: &Path,
    expected_size: u64,
    expected_modified: Option<SystemTime>,
    expected_created: Option<SystemTime>,
) -> Result<u64, String> {
    if !p.is_absolute() || !junk_delete_allowed(p) || has_reparse_ancestor(p)? {
        return Err("protected".into());
    }
    let resolved = fs::canonicalize(p).map_err(|e| e.to_string())?;
    if !junk_delete_allowed(&resolved) {
        return Err("protected".into());
    }
    let md = fs::symlink_metadata(&resolved).map_err(|e| e.to_string())?;
    if !md.is_file() || is_reparse(&md) || md.file_attributes() & CLOUD_MASK != 0 {
        return Err("skipped".into());
    }
    if md.len() != expected_size
        || md.modified().ok() != expected_modified
        || md.created().ok() != expected_created
    {
        return Err("changed since scan".into());
    }
    let size = md.len();
    match fs::remove_file(&resolved) {
        Ok(()) => Ok(size),
        Err(e) => {
            if md.permissions().readonly() {
                let original = md.permissions();
                let mut perm = original.clone();
                perm.set_readonly(false);
                if fs::set_permissions(&resolved, perm).is_ok() {
                    if fs::remove_file(&resolved).is_ok() {
                        return Ok(size);
                    }
                    // A locked file should keep its original read-only attribute.
                    let _ = fs::set_permissions(&resolved, original);
                }
            }
            Err(e.to_string())
        }
    }
}

/// Removes one reviewed empty cache folder. The caller must persist its intent first.
pub fn remove_empty_dir(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !junk_delete_allowed(path) || has_reparse_ancestor(path)? {
        return Err("Folder is protected or could not be checked".into());
    }
    let resolved = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !junk_delete_allowed(&resolved) {
        return Err("Folder is protected".into());
    }
    let md = fs::symlink_metadata(&resolved).map_err(|e| e.to_string())?;
    if !md.is_dir() || is_reparse(&md) {
        return Err("Links and non-folders are protected".into());
    }
    fs::remove_dir(resolved).map_err(|e| format!("Folder was kept: {e}"))
}

pub(crate) fn fixed_local(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    let drive = match path.components().next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some(d),
            _ => None,
        },
        _ => None,
    };
    drive.is_some_and(|d| {
        let root: Vec<_> = format!("{}:\\", d as char)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) == 3 }
    })
}
#[derive(Clone, PartialEq, Eq)]
pub struct CheckedPath {
    pub path: PathBuf,
    pub size: u64,
    pub folder: bool,
    file_id: (u32, u64),
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    tree: Option<u64>,
}

pub struct SelectionReview {
    pub items: Vec<CheckedPath>,
    pub skipped: Vec<(PathBuf, String)>,
}

fn check_path(path: &Path) -> Result<CheckedPath, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    if !fixed_local(path) {
        return Err("This drive does not provide a fixed local Recycle Bin.".into());
    }
    if let Some(reason) = block_reason(path) {
        return Err(reason.into());
    }
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|e| e.to_string())?;
    let md = file.metadata().map_err(|e| e.to_string())?;
    if is_reparse(&md) || md.file_attributes() & CLOUD_MASK != 0 {
        return Err("Links and cloud placeholders are kept.".into());
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let file_id = (
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    );
    let (tree, size) = if md.is_dir() {
        let (signature, bytes, _) = crate::leftovers::snapshot_folder(path)?;
        (Some(signature), bytes)
    } else if md.is_file() {
        (None, md.len())
    } else {
        return Err("Only ordinary files and folders can be moved.".into());
    };
    Ok(CheckedPath {
        path: path.to_path_buf(),
        size,
        folder: md.is_dir(),
        file_id,
        modified: md.modified().ok(),
        created: md.created().ok(),
        tree,
    })
}

pub fn check_selected(mut items: Vec<(PathBuf, u64)>) -> Result<SelectionReview, String> {
    if items.len() > 10_000 {
        return Err("Select fewer than 10,000 items per review.".into());
    }
    // A folder and one of its descendants can both be selected from file views.
    // Count the child once and retain it if its parent cannot be moved.
    items.sort_unstable_by(|a, b| {
        a.0.components()
            .count()
            .cmp(&b.0.components().count())
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut seen = std::collections::HashSet::new();
    let mut directories = std::collections::HashSet::new();
    let mut review = SelectionReview {
        items: Vec::new(),
        skipped: Vec::new(),
    };
    for (path, _) in items {
        if !seen.insert(norm(&path))
            || path
                .ancestors()
                .skip(1)
                .any(|p| directories.contains(&norm(p)))
        {
            continue;
        }
        match check_path(&path) {
            Ok(item) => {
                if item.folder {
                    directories.insert(norm(&path));
                }
                review.items.push(item);
            }
            Err(error) => review.skipped.push((path, error)),
        }
    }
    Ok(review)
}

pub fn recycle_checked(items: Vec<CheckedPath>) -> RecycleReport {
    let paths = items
        .iter()
        .map(|item| (item.path.clone(), item.size))
        .collect();
    let checks: HashMap<_, _> = items
        .iter()
        .map(|item| (item.path.as_path(), item))
        .collect();
    recycle_with(paths, "Space analyzer", |path| {
        let expected = checks.get(path).ok_or("This item was not reviewed.")?;
        let current = check_path(path)?;
        if &current != *expected {
            return Err("Changed since review. Check the current file before moving it.".into());
        }
        Ok(())
    })
}

pub struct RecycleReport {
    pub done: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    pub bytes: u64,
    pub journal_error: Option<String>,
}

/// Moves user-chosen items to the Recycle Bin (restorable). Protected paths are refused.
/// Each item is recorded in the cleanup journal first; if it can't be, nothing moves.
pub fn recycle(items: Vec<(PathBuf, u64)>, source: &str) -> RecycleReport {
    recycle_with(items, source, |path| {
        if !fixed_local(path) {
            return Err("This drive does not provide a fixed local Recycle Bin.".into());
        }
        if let Some(reason) = block_reason(path) {
            return Err(reason.into());
        }
        Ok(())
    })
}

fn recycle_with(
    items: Vec<(PathBuf, u64)>,
    source: &str,
    verify: impl Fn(&Path) -> Result<(), String>,
) -> RecycleReport {
    use crate::journal::{Act, Outcome, Session};
    let mut rep = RecycleReport {
        done: Vec::new(),
        failed: Vec::new(),
        bytes: 0,
        journal_error: None,
    };
    let mut session = match Session::begin(source) {
        Ok(session) => session,
        Err(e) => {
            rep.journal_error = Some(e.clone());
            rep.failed = items
                .into_iter()
                .map(|(p, _)| {
                    (
                        p,
                        format!("Nothing moved: cleanup history unavailable: {e}"),
                    )
                })
                .collect();
            return rep;
        }
    };
    let mut lines = Vec::new();
    for (batch_number, batch) in items.chunks(128).enumerate() {
        let ids = match session.intend(source, Act::Recycle, batch) {
            Ok(ids) => ids,
            Err(e) => {
                rep.journal_error = Some(e.clone());
                rep.failed
                    .extend(items[batch_number * 128..].iter().map(|(p, _)| {
                        (
                            p.clone(),
                            format!("Not moved: cleanup history unavailable: {e}"),
                        )
                    }));
                break;
            }
        };
        let mut outcomes = Vec::with_capacity(batch.len());
        for ((path, size), id) in batch.iter().zip(ids) {
            if let Err(reason) = verify(path) {
                outcomes.push((id, Outcome::Skipped, Some(reason.clone())));
                rep.failed.push((path.clone(), reason));
                continue;
            }
            match trash::delete(path) {
                Ok(()) => {
                    rep.bytes += size;
                    rep.done.push(path.clone());
                    outcomes.push((id, Outcome::Recycled, None));
                    lines.push(format!(
                        "  moved to Recycle Bin: {} ({})",
                        path.display(),
                        fmt_bytes(*size)
                    ));
                }
                Err(e) => {
                    outcomes.push((id, Outcome::Failed, Some(e.to_string())));
                    rep.failed.push((path.clone(), e.to_string()));
                }
            }
        }
        if let Err(e) = session.finish(&outcomes) {
            rep.journal_error = Some(e.clone());
            log(format!(
                "Cleanup history failed after this batch; completed results may be uncertain: {e}"
            ));
            rep.failed.extend(
                items[((batch_number + 1) * 128).min(items.len())..]
                    .iter()
                    .map(|(p, _)| {
                        (
                            p.clone(),
                            format!("Not moved: cleanup history unavailable: {e}"),
                        )
                    }),
            );
            break;
        }
    }
    log(format!(
        "Moved {} item(s) to the Recycle Bin ({}){}",
        rep.done.len(),
        fmt_bytes(rep.bytes),
        if rep.failed.is_empty() {
            String::new()
        } else {
            format!(", {} could not be moved", rep.failed.len())
        }
    ));
    log_file_only(&lines);
    rep
}

// ---------------------------------------------------------------- logging

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn log_path() -> PathBuf {
    env("LOCALAPPDATA")
        .unwrap_or_else(std::env::temp_dir)
        .join("PCSweep")
        .join("activity.log")
}

pub fn log(msg: impl AsRef<str>) {
    let line = format!(
        "[{}] {}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        msg.as_ref()
    );
    if let Ok(mut l) = LOG.lock() {
        l.push(line.clone());
        let len = l.len();
        if len > 3000 {
            l.drain(..len - 3000);
        }
    }
    append_file(&[line]);
}

/// Loads recent history from the log file so the Activity page shows past sessions too.
pub fn load_log_history() {
    let Ok(text) = fs::read_to_string(log_path()) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with('[')).collect();
    let start = lines.len().saturating_sub(500);
    if let Ok(mut l) = LOG.lock() {
        let mut old: Vec<String> = lines[start..].iter().map(|s| s.to_string()).collect();
        old.append(&mut l);
        *l = old;
    }
}

pub fn log_file_only(lines: &[String]) {
    if !lines.is_empty() {
        append_file(lines);
    }
}

fn append_file(lines: &[String]) {
    let p = log_path();
    if let Some(d) = p.parent() {
        let _ = fs::create_dir_all(d);
    }
    if let Ok(md) = fs::metadata(&p) {
        if md.len() > 5 * 1024 * 1024 {
            let _ = fs::rename(&p, p.with_file_name("activity.old.log"));
        }
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&p) {
        for l in lines {
            let _ = writeln!(f, "{l}");
        }
    }
}

pub fn log_lines() -> Vec<String> {
    LOG.lock().map(|l| l.clone()).unwrap_or_default()
}

// ---------------------------------------------------------------- settings

pub fn settings_path() -> PathBuf {
    log_path().with_file_name("settings.txt")
}

fn parse_settings(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// Simple key=value settings stored next to the activity log.
pub fn load_settings() -> HashMap<String, String> {
    fs::read_to_string(settings_path())
        .map(|text| parse_settings(&text))
        .unwrap_or_default()
}

pub fn save_settings(pairs: &[(String, String)]) {
    if let Err(error) = save_settings_at(&settings_path(), pairs) {
        log(format!("Could not save app settings. {error}"));
    }
}

fn save_settings_at(path: &Path, pairs: &[(String, String)]) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    static WRITER: Mutex<()> = Mutex::new(());
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    let _guard = WRITER
        .lock()
        .map_err(|_| std::io::Error::other("Settings writer unavailable"))?;
    let mut settings = match fs::read_to_string(path) {
        Ok(text) => parse_settings(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(error) => return Err(error),
    };
    settings.extend(pairs.iter().cloned());
    let mut entries: Vec<_> = settings.into_iter().collect();
    entries.sort_unstable();
    let folder = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Missing settings folder"))?;
    fs::create_dir_all(folder)?;

    let new_path = folder.join(format!(
        "settings-{}-{}.tmp",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&new_path)?;
    let result = (|| {
        let text: String = entries
            .iter()
            .map(|(key, value)| format!("{key}={value}\n"))
            .collect();
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        let wide = |path: &Path| {
            path.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let from = wide(&new_path);
        let to = wide(path);
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(new_path);
    }
    result
}

// ---------------------------------------------------------------- shell

fn explorer_command() -> Result<Command, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemWindowsDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    let mut buffer = vec![0u16; 32768];
    let length =
        unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err("Windows Explorer's system folder is unavailable.".into());
    }
    use std::os::windows::ffi::OsStringExt;
    let directory = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    Ok(Command::new(directory.join("explorer.exe")))
}

/// Resolve an internal Windows tool or document without searching the portable
/// application's current folder or PATH, including while running elevated.
fn system_file(name: &str) -> Result<PathBuf, String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-' || c == b'_')
        || name == "."
        || name == ".."
    {
        return Err("Invalid Windows system file name.".into());
    }
    Ok(crate::platform::system_directory()?.join(name))
}

pub fn open_system_tool(program: &str) -> Result<(), String> {
    Command::new(system_file(program)?)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open this Windows tool: {e}"))
}

pub fn open_system_document(program: &str, document: &str) -> Result<(), String> {
    Command::new(system_file(program)?)
        .arg(system_file(document)?)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not open this Windows tool: {e}"))
}

pub fn show_in_explorer(p: &Path) {
    if let Ok(mut command) = explorer_command() {
        let mut selection = std::ffi::OsString::from("/select,\"");
        selection.push(p);
        selection.push("\"");
        let _ = command.raw_arg(selection).spawn();
    }
}

pub fn open_path(p: &Path) {
    if let Ok(mut command) = explorer_command() {
        let _ = command.arg(p).spawn();
    }
}

pub fn open_uri(uri: &str) {
    if let Ok(mut command) = explorer_command() {
        let _ = command.arg(uri).spawn();
    }
}

pub fn launch(program: &str) {
    if let Ok(path) = system_file(program) {
        let _ = Command::new(path).spawn();
    }
}

pub fn hidden_command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn windows_launch_targets_are_absolute_and_reject_path_traversal() {
        assert!(Path::new(explorer_command().unwrap().get_program()).is_absolute());
        assert!(system_file("control.exe").unwrap().is_absolute());
        assert!(system_file("devmgmt.msc").unwrap().is_absolute());
        for input in [
            "",
            ".",
            "..",
            r"..\control.exe",
            "C:\\control.exe",
            "/tmp/tool",
        ] {
            assert!(system_file(input).is_err(), "accepted {input}");
        }
    }

    struct TempFolder(PathBuf);
    impl TempFolder {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "pcsweep-safety-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn path_protection_handles_aliases_and_boundaries() {
        assert_eq!(
            norm(Path::new(r"\\?\C:\Users\Me\Cache\..\Downloads\.\")),
            r"c:\users\me\downloads"
        );
        assert_eq!(
            norm(Path::new(r"\\?\UNC\Server\Share\Downloads\")),
            norm(Path::new(r"\\server\share\downloads"))
        );
        assert!(is_within(r"c:\downloads\file.txt", r"c:\downloads"));
        assert!(!is_within(r"c:\downloads-old\file.txt", r"c:\downloads"));
        assert!(block_reason_fast(Path::new(r"\\server\share\")).is_some());
        assert!(block_reason_fast(Path::new(r"relative\file.txt")).is_some());
    }

    #[test]
    fn cleanup_keeps_files_that_changed_after_review() {
        let tmp = TempFolder::new();
        let p = tmp.0.join("cache.dat");
        fs::write(&p, b"reviewed").unwrap();
        let md = fs::metadata(&p).unwrap();
        fs::write(&p, b"new work since review").unwrap();
        assert!(delete_junk_file(&p, md.len(), md.modified().ok(), md.created().ok()).is_err());
        assert_eq!(fs::read(&p).unwrap(), b"new work since review");
    }

    #[test]
    fn cleanup_removes_an_unchanged_reviewed_file() {
        let tmp = TempFolder::new();
        let p = tmp.0.join("cache.dat");
        fs::write(&p, b"cache").unwrap();
        let md = fs::metadata(&p).unwrap();
        assert_eq!(
            delete_junk_file(&p, md.len(), md.modified().ok(), md.created().ok()),
            Ok(5)
        );
        assert!(!p.exists());
    }

    #[test]
    fn cleanup_keeps_files_with_an_unexpected_timestamp() {
        let tmp = TempFolder::new();
        let p = tmp.0.join("cache.dat");
        fs::write(&p, b"cache").unwrap();
        let md = fs::metadata(&p).unwrap();
        assert!(delete_junk_file(
            &p,
            md.len(),
            Some(SystemTime::UNIX_EPOCH),
            md.created().ok()
        )
        .is_err());
        assert!(p.exists());
    }
}

#[cfg(test)]
mod selected_review_tests {
    use super::*;
    #[test]
    fn changed_files_are_preserved_after_selection_review() {
        let dir =
            std::env::temp_dir().join(format!("pcsweep-recycle-review-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keep.tmp");
        fs::write(&path, b"before").unwrap();
        let reviewed = check_selected(vec![(path.clone(), 1)]).unwrap();
        assert_eq!(reviewed.items[0].size, 6);
        fs::write(&path, b"changed since confirmation").unwrap();
        let report = recycle_checked(reviewed.items);
        assert!(report.done.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert!(path.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn missing_selection_members_do_not_block_the_rest_of_the_review() {
        let dir =
            std::env::temp_dir().join(format!("pcsweep-review-remaining-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let current = dir.join("current.tmp");
        let missing = dir.join("already-removed.tmp");
        fs::write(&current, b"present").unwrap();
        let review = check_selected(vec![(missing.clone(), 20), (current.clone(), 1)]).unwrap();
        assert_eq!(review.items.len(), 1);
        assert_eq!(review.items[0].path, current);
        assert_eq!(review.items[0].size, 7);
        assert_eq!(review.skipped.len(), 1);
        assert_eq!(review.skipped[0].0, missing);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_update_preserves_existing_values_and_a_failed_replace() {
        let dir = std::env::temp_dir().join(format!(
            "pcsweep-settings-safe-write-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.txt");
        fs::write(&path, "updates.automatic=1\nclean.Temp=0\n").unwrap();
        save_settings_at(&path, &[("clean.Temp".into(), "1".into())]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(parse_settings(&text)["updates.automatic"], "1");
        assert_eq!(parse_settings(&text)["clean.Temp"], "1");

        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(save_settings_at(&path, &[("clean.Temp".into(), "0".into())]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        drop(locked);
        fs::remove_dir_all(dir).unwrap();
    }
}
