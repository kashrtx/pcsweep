use std::fs;
use std::io::Write;
use std::os::windows::fs::MetadataExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
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

/// Lower-case, backslash-separated, no trailing slash, no \\?\ prefix.
pub fn norm(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('/', "\\").to_lowercase();
    if let Some(r) = s.strip_prefix(r"\\?\") {
        s = r.to_string();
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
        for d in ["Desktop", "Documents", "Pictures", "Music", "Videos", "OneDrive"] {
            push_variants(&mut libraries, &profile.join(d));
        }
        for var in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
            if let Some(p) = env(var) {
                push_variants(&mut libraries, &p);
            }
        }

        let mut system = Vec::new();
        for var in ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)", "ProgramW6432", "ProgramData"] {
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

/// Why a user-selected file/folder must not be moved to the Recycle Bin (fast, no disk access).
pub fn block_reason_fast(p: &Path) -> Option<&'static str> {
    let n = norm(p);
    let f = folders();
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
    if n.len() <= 3 {
        return Some("Drive root");
    }
    // Never allow removing a folder that *contains* something protected.
    if is_within(&norm(&f.profile), &n) {
        return Some("Contains your user profile");
    }
    if f.downloads.iter().any(|d| is_within(d, &n)) {
        return Some("Contains your Downloads folder");
    }
    if f.system.iter().chain(f.libraries.iter()).any(|d| is_within(d, &n)) {
        return Some("Contains a protected folder");
    }
    None
}

/// Full check, also resolving links/short names on disk.
pub fn block_reason(p: &Path) -> Option<&'static str> {
    if let Some(r) = block_reason_fast(p) {
        return Some(r);
    }
    if let Ok(c) = fs::canonicalize(p) {
        if let Some(r) = block_reason_fast(&c) {
            return Some(r);
        }
    }
    None
}

pub fn in_appdata(p: &Path) -> bool {
    norm(p).contains(r"\appdata\")
}

// ---------------------------------------------------------------- deleting

/// Permanently deletes one junk file. Refuses protected locations, folders and links.
pub fn delete_junk_file(p: &Path) -> Result<u64, String> {
    if !junk_delete_allowed(p) {
        return Err("protected".into());
    }
    let md = fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if md.is_dir() || is_reparse(&md) {
        return Err("skipped".into());
    }
    let size = md.len();
    match fs::remove_file(p) {
        Ok(()) => Ok(size),
        Err(e) => {
            if md.permissions().readonly() {
                let mut perm = md.permissions();
                perm.set_readonly(false);
                if fs::set_permissions(p, perm).is_ok() && fs::remove_file(p).is_ok() {
                    return Ok(size);
                }
            }
            Err(e.to_string())
        }
    }
}

/// Removes folders that became empty after cleaning (deepest first). Never removes non-empty folders.
pub fn remove_empty_dirs(mut dirs: Vec<PathBuf>) {
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        if !junk_delete_allowed(&d) {
            continue;
        }
        if let Ok(md) = fs::symlink_metadata(&d) {
            if md.is_dir() && !is_reparse(&md) {
                let _ = fs::remove_dir(&d);
            }
        }
    }
}

pub struct RecycleReport {
    pub done: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    pub bytes: u64,
}

/// Moves user-chosen items to the Recycle Bin (restorable). Protected paths are refused.
pub fn recycle(items: Vec<(PathBuf, u64)>) -> RecycleReport {
    let mut rep = RecycleReport {
        done: Vec::new(),
        failed: Vec::new(),
        bytes: 0,
    };
    let mut lines = Vec::new();
    for (p, size) in items {
        if let Some(reason) = block_reason(&p) {
            rep.failed.push((p, reason.to_string()));
            continue;
        }
        match trash::delete(&p) {
            Ok(()) => {
                lines.push(format!("  moved to Recycle Bin: {} ({})", p.display(), fmt_bytes(size)));
                rep.bytes += size;
                rep.done.push(p);
            }
            Err(e) => rep.failed.push((p, e.to_string())),
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

// ---------------------------------------------------------------- shell

pub fn show_in_explorer(p: &Path) {
    let _ = Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", p.display()))
        .spawn();
}

pub fn open_path(p: &Path) {
    let _ = Command::new("explorer.exe").arg(p).spawn();
}

pub fn open_uri(uri: &str) {
    let _ = Command::new("explorer.exe").arg(uri).spawn();
}

pub fn launch(program: &str) {
    let _ = Command::new(program).spawn();
}

pub fn hidden_command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}
