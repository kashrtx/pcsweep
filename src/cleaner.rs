use crate::app::{Action, App, Modal, ToastKind};
use crate::journal::{self, Act, Outcome};
use crate::system;
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_age, fmt_bytes, fmt_num};
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    System,
    Browsers,
    Apps,
    Developer,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::System => "Windows",
            Group::Browsers => "Web browsers",
            Group::Apps => "Apps",
            Group::Developer => "Developer tools",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Special {
    None,
    RecycleBin,
    /// Shader caches in every Steam library folder (found from libraryfolders.vdf).
    SteamShaders,
}

#[derive(Clone, Copy)]
pub struct Category {
    pub name: &'static str,
    pub desc: &'static str,
    pub group: Group,
    pub icon: char,
    pub targets: &'static [&'static str],
    pub admin: bool,
    pub default_on: bool,
    pub min_age_days: u32,
    pub caution: Option<&'static str>,
    pub processes: &'static [&'static str],
    pub special: Special,
}

impl Category {
    fn new(
        name: &'static str,
        desc: &'static str,
        group: Group,
        icon: char,
        targets: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            desc,
            group,
            icon,
            targets,
            admin: false,
            default_on: true,
            min_age_days: 0,
            caution: None,
            processes: &[],
            special: Special::None,
        }
    }
    fn admin(mut self) -> Self {
        self.admin = true;
        self
    }
    fn off(mut self) -> Self {
        self.default_on = false;
        self
    }
    fn age(mut self, days: u32) -> Self {
        self.min_age_days = days;
        self
    }
    fn caution(mut self, text: &'static str) -> Self {
        self.caution = Some(text);
        self
    }
    fn procs(mut self, p: &'static [&'static str]) -> Self {
        self.processes = p;
        self
    }
}

pub fn categories() -> Vec<Category> {
    use Group::*;
    vec![
        Category::new(
            "Temporary files",
            "Leftovers that apps write to your temp folder and forget about. Anything touched in the last 24 hours is skipped.",
            System, ic::CLEAN,
            &[r"%TEMP%", r"%LOCALAPPDATA%\Temp"],
        ).age(1),
        Category::new(
            "Windows temporary files",
            "The system-wide temp folder used by installers and services.",
            System, ic::CLEAN,
            &[r"%SystemRoot%\Temp"],
        ).admin().age(1),
        Category::new(
            "Thumbnail cache",
            "Saved previews of pictures and videos. Windows rebuilds them as you browse.",
            System, ic::PHOTO,
            &[r"%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db"],
        ),
        Category::new(
            "Microsoft Store app temp files",
            "Temporary files and web caches kept by Store apps. Anything used in the last 24 hours is skipped.",
            System, ic::APPS,
            &[
                r"%LOCALAPPDATA%\Packages\*\AC\Temp",
                r"%LOCALAPPDATA%\Packages\*\AC\INetCache",
                r"%LOCALAPPDATA%\Packages\*\TempState",
            ],
        ).age(1),
        Category::new(
            "Driver installer leftovers",
            "Setup files that AMD, NVIDIA and other driver installers extract and leave behind. Installed drivers are not affected.",
            System, ic::CHIP,
            &[
                r"%SystemDrive%\AMD",
                r"%SystemDrive%\NVIDIA",
                r"%ProgramData%\NVIDIA Corporation\Downloader",
            ],
        ).admin().off().age(7).caution("Review these folders first. Only select them if they contain extracted driver installers you no longer need."),
        Category::new(
            "Internet cache",
            "Web content cached by Windows components and older apps.",
            System, ic::GLOBE,
            &[r"%LOCALAPPDATA%\Microsoft\Windows\INetCache"],
        ),
        Category::new(
            "Error reports and crash dumps",
            "Diagnostic reports and memory dumps from crashes. Only useful when troubleshooting, and they can be several GB.",
            System, ic::BUG,
            &[
                r"%LOCALAPPDATA%\CrashDumps",
                r"%LOCALAPPDATA%\Microsoft\Windows\WER",
                r"%ProgramData%\Microsoft\Windows\WER\ReportArchive",
                r"%ProgramData%\Microsoft\Windows\WER\ReportQueue",
                r"%SystemRoot%\Minidump",
                r"%SystemRoot%\MEMORY.DMP",
                r"%SystemRoot%\LiveKernelReports",
            ],
        ).admin(),
        Category::new(
            "Delivery Optimization files",
            "Pieces of updates that Windows keeps to share with other PCs.",
            System, ic::UPDATE,
            &[
                r"%SystemRoot%\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache",
                r"%SystemRoot%\SoftwareDistribution\DeliveryOptimization",
            ],
        ).admin(),
        Category::new(
            "Windows Update downloads",
            "Installer files for updates that are already installed.",
            System, ic::UPDATE,
            &[r"%SystemRoot%\SoftwareDistribution\Download"],
        ).admin().off().age(2).caution("Don't clean this while Windows Update is installing something."),
        Category::new(
            "Old Windows logs",
            "Servicing and setup logs older than two weeks.",
            System, ic::FILE,
            &[r"%SystemRoot%\Logs\CBS", r"%SystemRoot%\Logs\DISM", r"%SystemRoot%\Logs\MoSetup", r"%SystemRoot%\Logs\WindowsUpdate"],
        ).admin().age(14),
        Category::new(
            "GPU shader caches",
            "Compiled graphics shaders. They rebuild automatically, so games may stutter briefly the first time you play.",
            System, ic::GAME,
            &[
                r"%LOCALAPPDATA%\D3DSCache",
                r"%LOCALAPPDATA%\NVIDIA\DXCache",
                r"%LOCALAPPDATA%\NVIDIA\GLCache",
                r"%ProgramData%\NVIDIA Corporation\NV_Cache",
                r"%LOCALAPPDATA%\AMD\DxCache",
                r"%LOCALAPPDATA%\AMD\DxcCache",
                r"%LOCALAPPDATA%\AMD\GLCache",
                r"%LOCALAPPDATA%\AMD\VkCache",
                r"%LOCALAPPDATA%\Intel\ShaderCache",
            ],
        ).off(),
        Category::new(
            "Prefetch data",
            "Windows uses this to start your apps faster. Cleaning it frees almost nothing and makes apps launch slower for a while.",
            System, ic::SPEED,
            &[r"%SystemRoot%\Prefetch"],
        ).admin().off().caution("Not recommended. Clearing Prefetch is a popular myth and slows app launches."),
        Category {
            special: Special::RecycleBin,
            ..Category::new(
                "Recycle Bin",
                "Files you already deleted. Emptying it is permanent, so check it first if you're unsure.",
                System, ic::CLEAN,
                &[],
            ).off()
        },
        Category::new(
            "Google Chrome cache",
            "Cached web pages, images and scripts. Your passwords, history, cookies and tabs are not touched.",
            Browsers, ic::GLOBE,
            &[
                r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Cache",
                r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Code Cache",
                r"%LOCALAPPDATA%\Google\Chrome\User Data\*\GPUCache",
                r"%LOCALAPPDATA%\Google\Chrome\User Data\ShaderCache",
                r"%LOCALAPPDATA%\Google\Chrome\User Data\GrShaderCache",
                r"%LOCALAPPDATA%\Google\Chrome\User Data\GraphiteDawnCache",
            ],
        ).procs(&["chrome.exe"]),
        Category::new(
            "Microsoft Edge cache",
            "Cached web pages, images and scripts. Your passwords, history, cookies and tabs are not touched.",
            Browsers, ic::GLOBE,
            &[
                r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Cache",
                r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Code Cache",
                r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\GPUCache",
                r"%LOCALAPPDATA%\Microsoft\Edge\User Data\ShaderCache",
                r"%LOCALAPPDATA%\Microsoft\Edge\User Data\GrShaderCache",
                r"%LOCALAPPDATA%\Microsoft\Edge\User Data\GraphiteDawnCache",
            ],
        ).procs(&["msedge.exe"]),
        Category::new(
            "Firefox cache",
            "Cached web content. Your bookmarks, passwords and history are not touched.",
            Browsers, ic::GLOBE,
            &[
                r"%LOCALAPPDATA%\Mozilla\Firefox\Profiles\*\cache2",
                r"%LOCALAPPDATA%\Mozilla\Firefox\Profiles\*\startupCache",
                r"%LOCALAPPDATA%\Mozilla\Firefox\Profiles\*\thumbnails",
            ],
        ).procs(&["firefox.exe"]),
        Category::new(
            "Brave, Opera and Vivaldi cache",
            "Cached web content for other Chromium browsers. Personal data is not touched.",
            Browsers, ic::GLOBE,
            &[
                r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data\*\Cache",
                r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data\*\Code Cache",
                r"%LOCALAPPDATA%\BraveSoftware\Brave-Browser\User Data\*\GPUCache",
                r"%LOCALAPPDATA%\Opera Software\Opera Stable\Cache",
                r"%LOCALAPPDATA%\Opera Software\Opera Stable\*\Cache",
                r"%LOCALAPPDATA%\Opera Software\Opera GX Stable\Cache",
                r"%LOCALAPPDATA%\Opera Software\Opera GX Stable\*\Cache",
                r"%LOCALAPPDATA%\Vivaldi\User Data\*\Cache",
                r"%LOCALAPPDATA%\Vivaldi\User Data\*\Code Cache",
            ],
        ).procs(&["brave.exe", "opera.exe", "vivaldi.exe"]),
        Category::new(
            "Discord cache",
            "Cached images, videos and app code. Messages and settings are not touched.",
            Apps, ic::CHAT,
            &[r"%APPDATA%\discord\Cache", r"%APPDATA%\discord\Code Cache", r"%APPDATA%\discord\GPUCache"],
        ).procs(&["discord.exe"]),
        Category::new(
            "Microsoft Teams cache",
            "Cached content for classic and new Teams. Chats live in the cloud and are not affected.",
            Apps, ic::CHAT,
            &[
                r"%APPDATA%\Microsoft\Teams\Cache",
                r"%APPDATA%\Microsoft\Teams\Code Cache",
                r"%APPDATA%\Microsoft\Teams\GPUCache",
                r"%LOCALAPPDATA%\Packages\MSTeams_8wekyb3d8bbwe\LocalCache\Microsoft\MSTeams\EBWebView\*\Cache",
                r"%LOCALAPPDATA%\Packages\MSTeams_8wekyb3d8bbwe\LocalCache\Microsoft\MSTeams\EBWebView\*\Code Cache",
            ],
        ).procs(&["teams.exe", "ms-teams.exe"]),
        Category::new(
            "Slack cache",
            "Cached files and app code. Workspaces and messages are not touched.",
            Apps, ic::CHAT,
            &[r"%APPDATA%\Slack\Cache", r"%APPDATA%\Slack\Code Cache", r"%APPDATA%\Slack\GPUCache"],
        ).procs(&["slack.exe"]),
        Category::new(
            "VS Code cache",
            "Cached data and downloaded extension packages. Settings and extensions stay installed.",
            Apps, ic::CODE,
            &[
                r"%APPDATA%\Code\Cache",
                r"%APPDATA%\Code\CachedData",
                r"%APPDATA%\Code\Code Cache",
                r"%APPDATA%\Code\GPUCache",
                r"%APPDATA%\Code\CachedExtensionVSIXs",
            ],
        ).procs(&["code.exe"]),
        Category::new(
            "Steam web cache",
            "The store and community pages cache. Your games are not touched.",
            Apps, ic::GAME,
            &[r"%LOCALAPPDATA%\Steam\htmlcache"],
        ).procs(&["steam.exe", "steamwebhelper.exe"]),
        Category {
            special: Special::SteamShaders,
            ..Category::new(
                "Steam shader cache",
                "Pre-compiled shaders for your Steam games, in every Steam library. Games rebuild them, so the first launch afterwards may stutter briefly.",
                Apps, ic::GAME,
                &[],
            ).off().procs(&["steam.exe"])
        },
        Category::new(
            "Other app caches",
            "Web caches inside apps built on Chromium or Electron, such as chat apps, launchers and editors. Only their cache folders are touched.",
            Apps, ic::APPS,
            &[
                r"%APPDATA%\*\Cache\Cache_Data",
                r"%APPDATA%\*\Code Cache",
                r"%APPDATA%\*\GPUCache",
                r"%APPDATA%\*\DawnCache",
                r"%APPDATA%\*\DawnGraphiteCache",
                r"%APPDATA%\*\DawnWebGPUCache",
                r"%LOCALAPPDATA%\*\Cache\Cache_Data",
                r"%LOCALAPPDATA%\*\GPUCache",
            ],
        ).age(1),
        Category::new(
            "Package manager caches",
            "Download caches for npm, pip, uv, Yarn, NuGet, Go, Gradle, Cargo and Electron. The next install or build downloads what it needs again.",
            Developer, ic::CODE,
            &[
                r"%LOCALAPPDATA%\npm-cache\_cacache",
                r"%LOCALAPPDATA%\pip\Cache",
                r"%LOCALAPPDATA%\Yarn\Cache",
                r"%LOCALAPPDATA%\NuGet\v3-cache",
                r"%LOCALAPPDATA%\NuGet\plugins-cache",
                r"%LOCALAPPDATA%\go-build",
                r"%LOCALAPPDATA%\uv\cache",
                r"%LOCALAPPDATA%\electron\Cache",
                r"%LOCALAPPDATA%\electron-builder\Cache",
                r"%USERPROFILE%\.gradle\caches",
                r"%USERPROFILE%\.cargo\registry\cache",
            ],
        ).off(),
    ]
}

#[derive(Clone, Serialize, Deserialize)]
pub struct JunkFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct CatResult {
    pub files: Vec<JunkFile>,
    pub dirs: Vec<PathBuf>,
    pub bytes: u64,
    pub count: u64,
    pub denied: u64,
    pub recent_skipped: u64,
    pub dupes_skipped: u64,
    pub roots_found: usize,
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, s) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while si < s.len() {
        if pi < p.len() && (p[pi] == s[si] || p[pi] == b'?') {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Expands environment variables and wildcards without traversing junctions.
/// Components preserve drive and UNC roots, including redirected temp folders.
fn expand_pattern(pattern: &str) -> Vec<PathBuf> {
    use std::path::Component;
    let expanded = util::expand_env(pattern).replace('/', "\\");
    if expanded.contains('%') || !std::path::Path::new(&expanded).is_absolute() {
        return Vec::new();
    }
    let mut current = vec![PathBuf::new()];
    for component in std::path::Path::new(&expanded).components() {
        let mut next = Vec::new();
        for base in &current {
            let part = component.as_os_str().to_string_lossy();
            if matches!(component, Component::Normal(_))
                && (part.contains('*') || part.contains('?'))
            {
                let pat = part.to_lowercase();
                if let Ok(rd) = fs::read_dir(base) {
                    for e in rd.flatten() {
                        let name = e.file_name().to_string_lossy().to_lowercase();
                        if glob_match(&pat, &name) {
                            if let Ok(md) = fs::symlink_metadata(e.path()) {
                                if !util::is_reparse(&md) {
                                    next.push(e.path());
                                }
                            }
                        }
                    }
                }
            } else {
                let p = base.join(component.as_os_str());
                if matches!(component, Component::Prefix(_) | Component::RootDir)
                    || fs::symlink_metadata(&p)
                        .map(|md| !util::is_reparse(&md))
                        .unwrap_or(false)
                {
                    next.push(p);
                }
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

fn newest_time(md: &fs::Metadata) -> Option<SystemTime> {
    let m = md.modified().ok();
    let c = md.created().ok();
    match (m, c) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn walk(dir: &std::path::Path, res: &mut CatResult, cutoff: Option<SystemTime>, depth: u32) {
    if depth > 64 || !util::junk_delete_allowed(dir) {
        return;
    }
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => {
            res.denied += 1;
            return;
        }
    };
    let mut subs = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if !util::junk_delete_allowed(&path) {
            continue;
        }
        let Ok(md) = fs::symlink_metadata(&path) else {
            continue;
        };
        if util::is_reparse(&md) {
            continue;
        }
        if md.is_dir() {
            subs.push(e.path());
            continue;
        }
        if let Some(cut) = cutoff {
            if newest_time(&md).map(|t| t > cut).unwrap_or(true) {
                res.recent_skipped += 1;
                continue;
            }
        }
        res.bytes += md.len();
        res.files.push(JunkFile {
            path: e.path(),
            size: md.len(),
            modified: md.modified().ok(),
            created: md.created().ok(),
        });
    }
    for s in subs {
        walk(&s, res, cutoff, depth + 1);
        res.dirs.push(s);
    }
}

/// Every Steam library's shadercache folder, read from Steam's own library list.
fn steam_shader_roots() -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = Vec::new();
    let steam = util::env("ProgramFiles(x86)").map(|p| p.join("Steam"));
    if let Some(steam) = steam {
        libs.push(steam.clone());
        let vdf = steam.join(r"steamapps\libraryfolders.vdf");
        if let Ok(text) = fs::read_to_string(vdf) {
            for line in text.lines() {
                let parts: Vec<&str> = line.split('"').collect();
                // Lines look like:   "path"    "D:\\SteamLibrary"
                if parts.len() >= 4 && parts[1].eq_ignore_ascii_case("path") {
                    let p = PathBuf::from(parts[3].replace("\\\\", "\\"));
                    if !libs.iter().any(|l| util::norm(l) == util::norm(&p)) {
                        libs.push(p);
                    }
                }
            }
        }
    }
    libs.into_iter()
        .map(|l| l.join(r"steamapps\shadercache"))
        .filter(|p| p.is_dir())
        .collect()
}

pub fn scan_category(cat: &Category) -> CatResult {
    let mut res = CatResult::default();
    if cat.special == Special::RecycleBin {
        let (b, n) = system::recycle_bin_info();
        res.bytes = b;
        res.count = n;
        res.roots_found = 1;
        return res;
    }
    let cutoff = if cat.min_age_days > 0 {
        SystemTime::now().checked_sub(Duration::from_secs(cat.min_age_days as u64 * 86_400))
    } else {
        None
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut roots: Vec<PathBuf> = cat.targets.iter().flat_map(|p| expand_pattern(p)).collect();
    if cat.special == Special::SteamShaders {
        roots.extend(steam_shader_roots());
    }
    {
        for root in roots {
            let canon = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            if !seen.insert(util::norm(&canon)) {
                continue;
            }
            // Safety net: a target must never resolve into Downloads or personal folders.
            if !util::junk_delete_allowed(&root) || !util::junk_delete_allowed(&canon) {
                continue;
            }
            let Ok(md) = fs::symlink_metadata(&root) else {
                continue;
            };
            if util::is_reparse(&md) {
                continue;
            }
            res.roots_found += 1;
            if md.is_file() {
                let recent = cutoff
                    .map(|cut| newest_time(&md).map(|t| t > cut).unwrap_or(true))
                    .unwrap_or(false);
                if recent {
                    res.recent_skipped += 1;
                } else {
                    res.bytes += md.len();
                    res.files.push(JunkFile {
                        path: root.clone(),
                        size: md.len(),
                        modified: md.modified().ok(),
                        created: md.created().ok(),
                    });
                }
            } else if md.is_dir() {
                walk(&root, &mut res, cutoff, 0);
            }
        }
    }
    res.files.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    res.count = res.files.len() as u64;
    res
}

fn running_process_names() -> HashSet<String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes();
    sys.processes()
        .values()
        .map(|p| p.name().to_lowercase())
        .collect()
}

pub struct ScanOutput {
    pub results: Vec<CatResult>,
    pub running: Vec<Vec<String>>,
    saved_at: u64,
    cache_error: Option<String>,
}

pub struct CleanJob {
    pub name: &'static str,
    pub files: Vec<JunkFile>,
    pub dirs: Vec<PathBuf>,
    pub recycle_bin: bool,
}

#[derive(Default, Clone)]
pub struct CleanReport {
    pub freed: u64,
    pub deleted: u64,
    pub failed: u64,
    /// Set when the cleanup journal couldn't be written, so cleaning stopped first.
    pub journal_error: Option<String>,
}

pub fn run_clean(jobs: Vec<CleanJob>) -> CleanReport {
    let mut rep = CleanReport::default();
    let mut journal = match journal::Session::begin("Junk cleaner") {
        Ok(session) => session,
        Err(error) => {
            rep.journal_error = Some(error);
            return rep;
        }
    };
    for job in jobs {
        if job.recycle_bin {
            let (bytes, count) = system::recycle_bin_info();
            if bytes == 0 && count == 0 {
                continue;
            }
            let ids = match journal.intend(
                "Recycle Bin",
                Act::EmptyRecycleBin,
                &[(
                    PathBuf::from(format!("{} items (batch)", fmt_num(count))),
                    bytes,
                )],
            ) {
                Ok(ids) => ids,
                Err(e) => {
                    rep.journal_error = Some(e);
                    return rep;
                }
            };
            let success = system::empty_recycle_bin();
            if success {
                rep.freed += bytes;
                rep.deleted += count;
            } else {
                rep.failed += count.max(1);
            }
            let result = if success {
                Outcome::Deleted
            } else {
                Outcome::Failed
            };
            if let Err(e) = journal.finish(&[(ids[0], result, None)]) {
                rep.journal_error = Some(e);
                return rep;
            }
            util::log(if success {
                format!("Emptied the Recycle Bin ({})", fmt_bytes(bytes))
            } else {
                "Could not empty the Recycle Bin".into()
            });
            continue;
        }
        let (mut freed, mut deleted, mut failed) = (0, 0, 0);
        for batch in job.files.chunks(128) {
            let items: Vec<_> = batch.iter().map(|f| (f.path.clone(), f.size)).collect();
            let ids = match journal.intend(job.name, Act::Delete, &items) {
                Ok(ids) => ids,
                Err(e) => {
                    rep.journal_error = Some(e);
                    return rep;
                }
            };
            let results: Vec<_> = batch
                .par_iter()
                .map(|f| util::delete_junk_file(&f.path, f.size, f.modified, f.created))
                .collect();
            let mut outcomes = Vec::with_capacity(batch.len());
            for (result, id) in results.into_iter().zip(ids) {
                match result {
                    Ok(bytes) => {
                        freed += bytes;
                        deleted += 1;
                        rep.freed += bytes;
                        rep.deleted += 1;
                        outcomes.push((id, Outcome::Deleted, None));
                    }
                    Err(reason) => {
                        failed += 1;
                        rep.failed += 1;
                        outcomes.push((id, Outcome::Skipped, Some(reason)));
                    }
                }
            }
            if let Err(e) = journal.finish(&outcomes) {
                rep.journal_error = Some(e);
                return rep;
            }
        }
        // Folder removal is also destructive: record intentions BEFORE trying it.
        let mut dirs = job.dirs;
        dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        dirs.dedup();
        for batch in dirs.chunks(128) {
            let items: Vec<_> = batch.iter().map(|p| (p.clone(), 0)).collect();
            let ids = match journal.intend(job.name, Act::RemoveFolder, &items) {
                Ok(ids) => ids,
                Err(e) => {
                    rep.journal_error = Some(e);
                    return rep;
                }
            };
            let outcomes: Vec<_> = batch
                .iter()
                .zip(ids)
                .map(|(path, id)| match util::remove_empty_dir(path) {
                    Ok(()) => (id, Outcome::Removed, None),
                    Err(reason) => (id, Outcome::Skipped, Some(reason)),
                })
                .collect();
            if let Err(e) = journal.finish(&outcomes) {
                rep.journal_error = Some(e);
                return rep;
            }
        }
        util::log(format!(
            "Cleaned {}: freed {}, {} files deleted, {} skipped (in use or protected)",
            job.name,
            fmt_bytes(freed),
            fmt_num(deleted),
            fmt_num(failed)
        ));
    }
    rep
}

// ====================================================================== state

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewSort {
    #[default]
    Largest,
    Newest,
    Oldest,
    Path,
}

/// The full-page review of one category's files.
pub struct FileReview {
    pub category: usize,
    pub filter: String,
    pub sort: ReviewSort,
    /// Indexes into the category's files after filtering and sorting.
    view: Vec<usize>,
    /// What `view` was built from; it's rebuilt only when this changes.
    key: Option<(String, ReviewSort, u64)>,
    /// Lower-case paths for fast filtering, built once per scan result.
    lower: Vec<String>,
    lower_for: Option<u64>,
}

impl FileReview {
    pub fn new(category: usize) -> Self {
        Self {
            category,
            filter: String::new(),
            sort: ReviewSort::Largest,
            view: Vec::new(),
            key: None,
            lower: Vec::new(),
            lower_for: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct CleanerDisk {
    categories: Vec<String>,
    results: Vec<Option<CatResult>>,
    excluded: Vec<HashSet<PathBuf>>,
    review: Option<(usize, String, ReviewSort)>,
}
#[derive(Serialize)]
struct CleanerDiskRef<'a> {
    categories: Vec<&'a str>,
    results: Vec<Option<&'a CatResult>>,
    excluded: &'a [HashSet<PathBuf>],
    review: Option<(usize, &'a str, ReviewSort)>,
}
struct RestoredCleaner {
    results: Vec<Option<Arc<CatResult>>>,
    excluded: Vec<HashSet<PathBuf>>,
    excluded_bytes: Vec<u64>,
    review: Option<FileReview>,
}
#[derive(Default)]
pub struct CleanerCache {
    load: Task<Result<Option<crate::scan_cache::Stored<RestoredCleaner>>, String>>,
    save: Task<Result<(), String>>,
    stamp: Option<u64>,
    from_disk: bool,
    dirty: Option<Instant>,
    error: Option<String>,
}
fn validate_cleaner_cache(data: &CleanerDisk, cats: &[Category]) -> Result<(), String> {
    if data.categories != cats.iter().map(|c| c.name.to_string()).collect::<Vec<_>>()
        || data.results.len() != cats.len()
        || data.excluded.len() != cats.len()
        || data.results.iter().flatten().any(|r| {
            r.files.len() > 500_000
                || r.dirs.len() > 500_000
                || r.files.iter().any(|f| !f.path.is_absolute())
                || r.dirs.iter().any(|p| !p.is_absolute())
        })
    {
        return Err("Saved cleaner categories changed; a fresh scan is needed.".into());
    }
    for (cat, result) in cats.iter().zip(&data.results) {
        if let Some(result) = result {
            if result.bytes > u64::MAX / 1024
                || result.count > u64::MAX / 1024
                || (cat.special != Special::RecycleBin
                    && (result.count != result.files.len() as u64
                        || result
                            .files
                            .iter()
                            .try_fold(0u64, |sum, file| sum.checked_add(file.size))
                            != Some(result.bytes)))
            {
                return Err("Saved cleaner totals are inconsistent.".into());
            }
        }
    }
    Ok(())
}

pub struct CleanerState {
    pub cats: Vec<Category>,
    pub selected: Vec<bool>,
    pub results: Vec<Option<Arc<CatResult>>>,
    pub cache: CleanerCache,
    pub excluded: Vec<HashSet<PathBuf>>,
    pub excluded_bytes: Vec<u64>,
    pub running: Vec<Vec<String>>,
    pub scan: Task<ScanOutput>,
    pub clean: Task<CleanReport>,
    pub review: Option<FileReview>,
    /// Bumped whenever scan results are replaced, so cached review views rebuild.
    pub generation: u64,
    pub last_report: Option<CleanReport>,
    pub scanned_once: bool,
}

impl CleanerState {
    pub fn new() -> Self {
        let cats = categories();
        let n = cats.len();
        let saved = util::load_settings();
        Self {
            selected: cats
                .iter()
                .map(
                    |c| match saved.get(&format!("clean.{}", c.name)).map(|v| v.as_str()) {
                        Some("1") => true,
                        Some("0") => false,
                        _ => c.default_on,
                    },
                )
                .collect(),
            results: (0..n).map(|_| None).collect(),
            cache: CleanerCache::default(),
            excluded: vec![HashSet::new(); n],
            excluded_bytes: vec![0; n],
            running: vec![Vec::new(); n],
            cats,
            scan: Task::default(),
            clean: Task::default(),
            review: None,
            generation: 0,
            last_report: None,
            scanned_once: false,
        }
    }

    /// Rebuilds the review list only when its filter, sort or data changed, so a
    /// category with tens of thousands of files never re-sorts every frame.
    fn prepare_review(&mut self) {
        let generation = self.generation;
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let Some(Some(result)) = self.results.get(review.category) else {
            return;
        };
        let key = (review.filter.trim().to_lowercase(), review.sort, generation);
        if review.key.as_ref() == Some(&key) {
            return;
        }
        if review.lower_for != Some(generation) {
            review.lower = result
                .files
                .iter()
                .map(|f| f.path.to_string_lossy().to_lowercase())
                .collect();
            review.lower_for = Some(generation);
        }
        let needle = key.0.as_str();
        let lower = &review.lower;
        let files = &result.files;
        let mut view: Vec<usize> = (0..files.len())
            .filter(|&i| needle.is_empty() || lower[i].contains(needle))
            .collect();
        match key.1 {
            ReviewSort::Largest => view.sort_by(|&a, &b| files[b].size.cmp(&files[a].size)),
            ReviewSort::Newest => view.sort_by(|&a, &b| files[b].modified.cmp(&files[a].modified)),
            ReviewSort::Oldest => view.sort_by(|&a, &b| files[a].modified.cmp(&files[b].modified)),
            ReviewSort::Path => view.sort_by(|&a, &b| lower[a].cmp(&lower[b])),
        }
        review.view = view;
        review.key = Some(key);
    }

    /// Includes or keeps every file currently shown in the review (respecting the filter).
    fn set_shown(&mut self, include: bool) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let i = review.category;
        let Some(Some(result)) = self.results.get(i) else {
            return;
        };
        for &index in &review.view {
            let file = &result.files[index];
            if include {
                if self.excluded[i].remove(&file.path) {
                    self.excluded_bytes[i] = self.excluded_bytes[i].saturating_sub(file.size);
                }
            } else if self.excluded[i].insert(file.path.clone()) {
                self.excluded_bytes[i] += file.size;
            }
        }
    }

    pub fn cat_bytes(&self, i: usize) -> u64 {
        self.results[i]
            .as_ref()
            .map(|r| r.bytes.saturating_sub(self.excluded_bytes[i]))
            .unwrap_or(0)
    }

    pub fn cat_files(&self, i: usize) -> u64 {
        self.results[i]
            .as_ref()
            .map(|r| r.count.saturating_sub(self.excluded[i].len() as u64))
            .unwrap_or(0)
    }

    pub fn selected_total(&self) -> (u64, u64, usize) {
        let mut b = 0;
        let mut f = 0;
        let mut n = 0;
        for i in 0..self.cats.len() {
            if self.selected[i] && self.cat_bytes(i) > 0 {
                b += self.cat_bytes(i);
                f += self.cat_files(i);
                n += 1;
            }
        }
        (b, f, n)
    }

    /// Junk found in categories that are on by default (what we'd recommend cleaning).
    pub fn recommended_total(&self) -> u64 {
        (0..self.cats.len())
            .filter(|&i| self.cats[i].default_on)
            .map(|i| self.cat_bytes(i))
            .sum()
    }

    pub fn recycle_bin_bytes(&self) -> u64 {
        (0..self.cats.len())
            .filter(|&i| self.cats[i].special == Special::RecycleBin)
            .map(|i| self.cat_bytes(i))
            .sum()
    }

    /// Junk found in optional (off by default) categories.
    pub fn optional_total(&self) -> u64 {
        (0..self.cats.len())
            .filter(|&i| !self.cats[i].default_on)
            .map(|i| self.cat_bytes(i))
            .sum()
    }

    pub fn save_selection(&self) {
        let pairs: Vec<(String, String)> = self
            .cats
            .iter()
            .zip(self.selected.iter())
            .map(|(c, s)| {
                (
                    format!("clean.{}", c.name),
                    if *s { "1".to_string() } else { "0".to_string() },
                )
            })
            .collect();
        util::save_settings(&pairs);
    }

    /// Top recommended categories by size (index, bytes), for the dashboard.
    pub fn top(&self, n: usize) -> Vec<(usize, u64)> {
        let mut v: Vec<(usize, u64)> = (0..self.cats.len())
            .filter(|&i| self.cats[i].default_on)
            .map(|i| (i, self.cat_bytes(i)))
            .filter(|(_, b)| *b > 0)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.truncate(n);
        v
    }
}

enum CAct {
    SetSel(usize, bool),
    SelectRecommended,
    SelectAllFound,
    SelectNone,
    Expand(usize),
    CloseReview,
    ReviewFilter(String),
    ReviewSort(ReviewSort),
    KeepShown,
    IncludeShown,
    CopyPath(String),
    Toggle(usize, PathBuf, u64, bool),
    Show(PathBuf),
    Ask,
    Rescan,
    Relaunch,
}

impl App {
    pub(crate) fn restore_cleaner_cache(&mut self, ctx: &egui::Context) {
        let cats = self.cleaner.cats.clone();
        self.cleaner.cache.load = Task::spawn(ctx, move || {
            let value = crate::scan_cache::load::<CleanerDisk>("cleaner.json")?;
            value
                .map(|value| {
                    validate_cleaner_cache(&value.data, &cats)?;
                    let excluded_bytes = value
                        .data
                        .results
                        .iter()
                        .enumerate()
                        .map(|(i, result)| {
                            result
                                .as_ref()
                                .map(|r| {
                                    r.files
                                        .iter()
                                        .filter(|f| value.data.excluded[i].contains(&f.path))
                                        .map(|f| f.size)
                                        .sum()
                                })
                                .unwrap_or(0)
                        })
                        .collect();
                    let review = value
                        .data
                        .review
                        .filter(|(i, filter, _)| *i < cats.len() && filter.len() <= 8192)
                        .map(|(i, filter, sort)| {
                            let mut review = FileReview::new(i);
                            review.filter = filter;
                            review.sort = sort;
                            review
                        });
                    Ok(crate::scan_cache::Stored {
                        saved: value.saved,
                        data: RestoredCleaner {
                            results: value
                                .data
                                .results
                                .into_iter()
                                .map(|r| r.map(Arc::new))
                                .collect(),
                            excluded: value.data.excluded,
                            excluded_bytes,
                            review,
                        },
                    })
                })
                .transpose()
        });
    }
    pub(crate) fn poll_cleaner_cache(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.cleaner.cache.load.poll() {
            match result.and_then(|r| r) {
                Ok(Some(value)) if !self.cleaner.scan.running() && !self.cleaner.scanned_once => {
                    self.cleaner.results = value.data.results;
                    self.cleaner.excluded = value.data.excluded;
                    self.cleaner.excluded_bytes = value.data.excluded_bytes;
                    self.cleaner.review = value.data.review;
                    self.cleaner.scanned_once = true;
                    self.cleaner.generation += 1;
                    self.cleaner.cache.stamp = Some(value.saved);
                    self.cleaner.cache.from_disk = true;
                }
                Ok(_) => {
                    if !self.cleaner.scanned_once && !self.cleaner.scan.running() {
                        self.start_junk_scan(ctx);
                    }
                }
                Err(e) => {
                    self.cleaner.cache.error = Some(e);
                    if !self.cleaner.scanned_once && !self.cleaner.scan.running() {
                        self.start_junk_scan(ctx);
                    }
                }
            }
        }
        if let Some(result) = self.cleaner.cache.save.poll() {
            if let Err(e) = result.and_then(|r| r) {
                self.cleaner.cache.error = Some(e);
            } else {
                self.cleaner.cache.error = None;
            }
        }
        if !self.cleaner.cache.save.running()
            && self
                .cleaner
                .cache
                .dirty
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(500))
        {
            let categories: Vec<_> = self.cleaner.cats.iter().map(|c| c.name).collect();
            let results = self.cleaner.results.clone();
            let excluded = self.cleaner.excluded.clone();
            let review = self
                .cleaner
                .review
                .as_ref()
                .map(|r| (r.category, r.filter.clone(), r.sort));
            let stamp = self
                .cleaner
                .cache
                .stamp
                .unwrap_or_else(crate::scan_cache::now);
            self.cleaner.cache.dirty = None;
            self.cleaner.cache.save = Task::spawn(ctx, move || {
                let data = CleanerDiskRef {
                    categories,
                    results: results.iter().map(|r| r.as_deref()).collect(),
                    excluded: &excluded,
                    review: review.as_ref().map(|(i, f, s)| (*i, f.as_str(), *s)),
                };
                crate::scan_cache::save("cleaner.json", stamp, &data)
            });
        }
    }
    pub fn start_junk_scan(&mut self, ctx: &egui::Context) {
        self.cleaner.cache.load = Task::default();
        if self.cleaner.scan.running() || self.cleaner.clean.running() {
            return;
        }
        let cats = self.cleaner.cats.clone();
        self.cleaner.scan = Task::spawn(ctx, move || {
            let names = running_process_names();
            let mut results: Vec<CatResult> = cats.par_iter().map(scan_category).collect();
            // A file can match more than one category (for example an app cache that the
            // generic "Other app caches" also covers). Count and clean it only once.
            let mut seen: HashSet<String> = HashSet::new();
            for r in results.iter_mut() {
                let before = r.files.len();
                r.files.retain(|f| seen.insert(util::norm(&f.path)));
                let removed = before - r.files.len();
                if removed > 0 {
                    r.dupes_skipped = removed as u64;
                    r.bytes = r.files.iter().map(|f| f.size).sum();
                    r.count = r.files.len() as u64;
                }
            }
            let running: Vec<Vec<String>> = cats
                .iter()
                .map(|c| {
                    c.processes
                        .iter()
                        .filter(|p| names.contains(&p.to_lowercase()))
                        .map(|s| s.to_string())
                        .collect::<Vec<String>>()
                })
                .collect();
            let saved_at = crate::scan_cache::now();
            let empty = vec![HashSet::new(); cats.len()];
            let data = CleanerDiskRef {
                categories: cats.iter().map(|c| c.name).collect(),
                results: results.iter().map(Some).collect(),
                excluded: &empty,
                review: None,
            };
            let cache_error = crate::scan_cache::save("cleaner.json", saved_at, &data).err();
            ScanOutput {
                results,
                running,
                saved_at,
                cache_error,
            }
        });
    }

    pub fn poll_cleaner(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.cleaner.scan.poll() {
            match r {
                Ok(out) => {
                    let replacement = out.results.into_iter().map(|r| Some(Arc::new(r))).collect();
                    let old = std::mem::replace(&mut self.cleaner.results, replacement);
                    let _ = std::thread::Builder::new()
                        .name("pcsweep-release-cleaner".into())
                        .spawn(move || drop(old));
                    self.cleaner.running = out.running;
                    for s in self.cleaner.excluded.iter_mut() {
                        s.clear();
                    }
                    for b in self.cleaner.excluded_bytes.iter_mut() {
                        *b = 0;
                    }
                    self.cleaner.scanned_once = true;
                    self.cleaner.cache.stamp = Some(out.saved_at);
                    self.cleaner.cache.error = out.cache_error;
                    self.cleaner.cache.from_disk = false;
                    self.cleaner.cache.dirty = None;
                    self.cleaner.generation += 1;
                }
                Err(e) => self.toast(ToastKind::Error, format!("Junk scan failed: {e}")),
            }
        }
        if let Some(r) = self.cleaner.clean.poll() {
            match r {
                Ok(rep) => {
                    if let Some(error) = &rep.journal_error {
                        self.toast(
                            ToastKind::Error,
                            format!("Cleaning stopped: {error}. Completed actions with an unsaved result remain uncertain in history."),
                        );
                    } else {
                        self.toast(
                            ToastKind::Success,
                            format!(
                                "Freed {} ({} files deleted)",
                                fmt_bytes(rep.freed),
                                fmt_num(rep.deleted)
                            ),
                        );
                    }
                    self.cleaner.last_report = Some(rep);
                    self.sys.refresh_drives();
                    self.start_junk_scan(ctx);
                }
                Err(e) => self.toast(ToastKind::Error, format!("Cleaning stopped: {e}")),
            }
        }
    }

    pub fn ask_clean(&mut self) {
        if self.cleaner.cache.from_disk || self.cleaner.cache.load.running() {
            self.toast(ToastKind::Info, "Refresh the saved junk scan before cleaning. This prevents older results from deleting newer Recycle Bin contents.");
            return;
        }
        let (bytes, files, n) = self.cleaner.selected_total();
        if n == 0 {
            self.toast(ToastKind::Info, "Nothing selected to clean.");
            return;
        }
        let mut details = Vec::new();
        let mut has_bin = false;
        for i in 0..self.cleaner.cats.len() {
            if self.cleaner.selected[i] && self.cleaner.cat_bytes(i) > 0 {
                let c = &self.cleaner.cats[i];
                if c.special == Special::RecycleBin {
                    has_bin = true;
                }
                details.push(format!(
                    "{}: {} ({} files)",
                    c.name,
                    fmt_bytes(self.cleaner.cat_bytes(i)),
                    fmt_num(self.cleaner.cat_files(i))
                ));
            }
        }
        let note = if has_bin {
            Some("This includes emptying the Recycle Bin, which can't be undone.".to_string())
        } else {
            None
        };
        self.modal = Some(Modal {
            title: format!("Clean {}?", fmt_bytes(bytes)),
            body: format!(
                "{} files from {} categories will be permanently deleted. Files that are in use are skipped automatically. Your Downloads and personal folders are never touched.",
                fmt_num(files),
                n
            ),
            details,
            note,
            confirm: "Delete permanently".into(),
            danger: true,
            action: Action::CleanJunk,
        });
    }

    pub fn start_clean(&mut self, ctx: &egui::Context) {
        if self.cleaner.cache.from_disk || self.cleaner.cache.load.running() {
            self.toast(
                ToastKind::Info,
                "Refresh this saved scan and review the new results before cleaning.",
            );
            return;
        }
        if self.cleaner.clean.running()
            || self.analyzer.recycle.running()
            || self.dups.recycle.running()
        {
            self.toast(
                ToastKind::Info,
                "A cleanup is already running. Wait for it to finish before cleaning more files.",
            );
            return;
        }
        let mut jobs = Vec::new();
        for i in 0..self.cleaner.cats.len() {
            if !self.cleaner.selected[i] {
                continue;
            }
            let cat = self.cleaner.cats[i];
            let Some(res) = &self.cleaner.results[i] else {
                continue;
            };
            if cat.special == Special::RecycleBin {
                if res.bytes > 0 || res.count > 0 {
                    jobs.push(CleanJob {
                        name: cat.name,
                        files: Vec::new(),
                        dirs: Vec::new(),
                        recycle_bin: true,
                    });
                }
                continue;
            }
            let excl = &self.cleaner.excluded[i];
            let files: Vec<JunkFile> = res
                .files
                .iter()
                .filter(|f| !excl.contains(&f.path))
                .cloned()
                .collect();
            if files.is_empty() {
                continue;
            }
            jobs.push(CleanJob {
                name: cat.name,
                files,
                dirs: res.dirs.clone(),
                recycle_bin: false,
            });
        }
        if jobs.is_empty() {
            return;
        }
        self.cleaner.review = None;
        self.cleaner.clean = Task::spawn(ctx, move || run_clean(jobs));
    }

    pub fn page_cleaner(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = Vec::new();
        if self.cleaner.review.is_some() {
            self.cleaner.prepare_review();
            self.render_review(ui, &mut acts);
            if self.modal.is_none() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                acts.push(CAct::CloseReview);
            }
        } else {
            self.render_cleaner(ui, &mut acts);
        }
        let cache_changed = !acts.is_empty();
        let mut selection_changed = false;
        for a in acts {
            match a {
                CAct::SetSel(i, v) => {
                    self.cleaner.selected[i] = v;
                    selection_changed = true;
                }
                CAct::SelectRecommended => {
                    for (i, c) in self.cleaner.cats.iter().enumerate() {
                        self.cleaner.selected[i] = c.default_on;
                    }
                    selection_changed = true;
                }
                CAct::SelectAllFound => {
                    for i in 0..self.cleaner.cats.len() {
                        let cat = self.cleaner.cats[i];
                        // Prefetch is never bulk-selected; it's a myth that it helps.
                        let skip = cat.caution.is_some() && cat.name == "Prefetch data";
                        self.cleaner.selected[i] = !skip && self.cleaner.cat_bytes(i) > 0;
                    }
                    selection_changed = true;
                }
                CAct::SelectNone => {
                    for s in self.cleaner.selected.iter_mut() {
                        *s = false;
                    }
                    selection_changed = true;
                }
                CAct::Expand(i) => self.cleaner.review = Some(FileReview::new(i)),
                CAct::CloseReview => self.cleaner.review = None,
                CAct::ReviewFilter(value) => {
                    if let Some(review) = self.cleaner.review.as_mut() {
                        review.filter = value;
                    }
                }
                CAct::ReviewSort(sort) => {
                    if let Some(review) = self.cleaner.review.as_mut() {
                        review.sort = sort;
                    }
                }
                CAct::KeepShown => self.cleaner.set_shown(false),
                CAct::IncludeShown => self.cleaner.set_shown(true),
                CAct::CopyPath(text) => ctx.output_mut(|o| o.copied_text = text),
                CAct::Toggle(i, path, size, include) => {
                    if include {
                        if self.cleaner.excluded[i].remove(&path) {
                            self.cleaner.excluded_bytes[i] =
                                self.cleaner.excluded_bytes[i].saturating_sub(size);
                        }
                    } else if self.cleaner.excluded[i].insert(path) {
                        self.cleaner.excluded_bytes[i] += size;
                    }
                }
                CAct::Show(p) => util::show_in_explorer(&p),
                CAct::Ask => self.ask_clean(),
                CAct::Rescan => self.start_junk_scan(&ctx),
                CAct::Relaunch => self.relaunch_admin(&ctx),
            }
        }
        if selection_changed {
            self.cleaner.save_selection();
        }
        if cache_changed && self.cleaner.cache.stamp.is_some() {
            self.cleaner.cache.dirty = Some(Instant::now());
        }
    }

    fn render_cleaner(&self, ui: &mut Ui, acts: &mut Vec<CAct>) {
        let c = &self.cleaner;
        widgets::page_header(
            ui,
            "Junk cleaner",
            "Caches, temp files and leftovers that are safe to remove. Nothing is deleted until you review it and confirm.",
        );
        if c.cache.load.running() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(widgets::dim("Restoring the last junk scan…"));
            });
        }
        if let Some(stamp) = c.cache.stamp {
            ui.label(widgets::dim(format!(
                "Scan from {}. {}",
                crate::scan_cache::saved_label(stamp),
                if c.cache.from_disk {
                    "Saved results · Scan again before cleaning."
                } else {
                    "Refresh when files change."
                }
            )));
        }
        if let Some(error) = &c.cache.error {
            ui.label(widgets::faint(format!("Saving scan results: {error}")));
        }
        let scanning = c.scan.running() || c.cache.load.running();
        let cleaning = c.clean.running();
        let (sel_bytes, sel_files, sel_n) = c.selected_total();

        widgets::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(widgets::dim("Selected to clean"));
                    if scanning && !c.scanned_once {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(widgets::strong("Looking for junk…", 20.0));
                        });
                    } else {
                        ui.label(
                            RichText::new(fmt_bytes(sel_bytes))
                                .font(widgets::light(44.0))
                                .color(TEXT),
                        );
                        ui.label(widgets::dim(format!(
                            "{} files in {} {}",
                            fmt_num(sel_files),
                            sel_n,
                            if sel_n == 1 { "category" } else { "categories" }
                        )));
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let can_clean = !c.cache.from_disk
                        && !scanning
                        && !cleaning
                        && !self.analyzer.recycle.running()
                        && !self.dups.recycle.running()
                        && sel_n > 0;
                    if ui
                        .add_enabled(
                            can_clean,
                            widgets::primary(widgets::icon_text(
                                ic::CLEAN,
                                "Clean selected",
                                14.0,
                                egui::Color32::WHITE,
                            )),
                        )
                        .clicked()
                    {
                        acts.push(CAct::Ask);
                    }
                    if ui
                        .add_enabled(
                            !scanning && !cleaning,
                            widgets::ghost(widgets::icon_text(
                                ic::REFRESH,
                                "Scan again",
                                14.0,
                                TEXT,
                            )),
                        )
                        .clicked()
                    {
                        acts.push(CAct::Rescan);
                    }
                    if cleaning {
                        ui.label(widgets::dim("Cleaning…"));
                        ui.spinner();
                    } else if scanning {
                        ui.label(widgets::dim("Scanning…"));
                        ui.spinner();
                    }
                });
            });
            if let Some(rep) = &c.last_report {
                ui.add_space(6.0);
                widgets::banner(
                    ui,
                    if rep.journal_error.is_some() {
                        WARN
                    } else {
                        GOOD
                    },
                    if rep.journal_error.is_some() {
                        ic::WARN
                    } else {
                        ic::CHECK
                    },
                    |ui| {
                        ui.vertical(|ui| {
                    ui.add(Label::new(format!(
                        "Last clean freed {}. {} files deleted{}.",
                        fmt_bytes(rep.freed),
                        fmt_num(rep.deleted),
                        if rep.failed > 0 {
                            format!(", {} skipped (in use, changed, protected or unavailable)", fmt_num(rep.failed))
                        } else {
                            String::new()
                        }
                    )).wrap());
                    if let Some(error)=&rep.journal_error {ui.add(Label::new(widgets::dim(format!("Cleanup stopped: {error}. Check Cleanup history for uncertain results."))).wrap());}
                    });
                    },
                );
            }
        });

        if !self.is_admin {
            ui.add_space(4.0);
            widgets::banner(ui, WARN, ic::SHIELD, |ui| {
                ui.label("Some Windows locations need administrator rights to scan and clean.");
                if ui.add(widgets::small("Restart as administrator")).clicked() {
                    acts.push(CAct::Relaunch);
                }
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(widgets::dim("Select"));
            if ui
                .add(widgets::small("Recommended"))
                .on_hover_text("The categories that are safe for everyone")
                .clicked()
            {
                acts.push(CAct::SelectRecommended);
            }
            if ui
                .add(widgets::small("Everything found"))
                .on_hover_text("All categories with something to clean, except Prefetch")
                .clicked()
            {
                acts.push(CAct::SelectAllFound);
            }
            if ui.add(widgets::small("None")).clicked() {
                acts.push(CAct::SelectNone);
            }
            ui.label(widgets::faint("Your choices are remembered next time."));
        });
        ScrollArea::vertical()
            .id_salt("cleaner_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for group in [Group::System, Group::Browsers, Group::Apps, Group::Developer] {
                    let idxs: Vec<usize> = (0..c.cats.len())
                        .filter(|&i| c.cats[i].group == group)
                        .filter(|&i| match &c.results[i] {
                            Some(r) => r.roots_found > 0 || c.cats[i].special == Special::RecycleBin,
                            None => true,
                        })
                        .collect();
                    if idxs.is_empty() {
                        continue;
                    }
                    widgets::section(ui, group.label());
                    for i in idxs {
                        self.cleaner_row(ui, i, acts);
                        ui.add_space(2.0);
                    }
                }
                ui.add_space(8.0);
                ui.label(widgets::faint(
                    "Categories for apps you don't have installed are hidden. Every clean is written to the Activity log.",
                ));
                ui.add_space(20.0);
            });
    }

    fn cleaner_row(&self, ui: &mut Ui, i: usize, acts: &mut Vec<CAct>) {
        let c = &self.cleaner;
        let cat = c.cats[i];
        let res = c.results[i].as_ref();
        let bytes = c.cat_bytes(i);
        let files = c.cat_files(i);

        widgets::tight_frame().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // Fixed-width columns: name/description on the left, size and button on the right.
            ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                let mut sel = c.selected[i];
                if ui.checkbox(&mut sel, "").changed() {
                    acts.push(CAct::SetSel(i, sel));
                }
                ui.label(widgets::icon(cat.icon, 20.0, if sel { ACCENT } else { TEXT_FAINT }));
                ui.add_space(4.0);
                let right_w = 260.0;
                let text_w = (ui.available_width() - right_w).max(240.0);
                ui.allocate_ui_with_layout(egui::vec2(text_w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(text_w);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(widgets::strong(cat.name, 15.0));
                        if cat.admin && !self.is_admin {
                            widgets::badge(ui, "Needs admin", WARN);
                        }
                        if cat.caution.is_some() {
                            widgets::badge(ui, "Read first", WARN);
                        }
                        if !cat.default_on {
                            widgets::badge(ui, "Optional", TEXT_DIM);
                        }
                        for p in &c.running[i] {
                            widgets::badge(ui, &format!("{} is open", p.trim_end_matches(".exe")), WARN)
                                .on_hover_text("Close it for the most thorough clean. Files it's using are skipped.");
                        }
                    });
                    ui.label(widgets::dim(cat.desc));
                    if let Some(text) = cat.caution {
                        ui.label(RichText::new(text).color(WARN).size(12.5));
                    }
                    if let Some(r) = res {
                        let mut parts = Vec::new();
                        if r.recent_skipped > 0 {
                            parts.push(format!("{} recently used files kept", fmt_num(r.recent_skipped)));
                        }
                        if r.denied > 0 && !self.is_admin && cat.admin {
                            parts.push("some folders need admin to read".to_string());
                        }
                        if r.dupes_skipped > 0 {
                            parts.push(format!("{} files already counted in another category", fmt_num(r.dupes_skipped)));
                        }
                        if !parts.is_empty() {
                            ui.label(widgets::faint(parts.join(", ")));
                        }
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    let can_review = res.map(|r| !r.files.is_empty()).unwrap_or(false);
                    ui.allocate_ui_with_layout(egui::vec2(110.0, 30.0), Layout::right_to_left(Align::Center), |ui| {
                        if can_review && ui.add(widgets::small("Review files")).clicked() {
                            acts.push(CAct::Expand(i));
                        }
                    });
                    ui.allocate_ui_with_layout(egui::vec2(120.0, 0.0), Layout::top_down(Align::Max), |ui| {
                        ui.set_width(120.0);
                        match res {
                            None => {
                                ui.spinner();
                            }
                            Some(r) if r.bytes == 0 && r.count == 0 => {
                                ui.label(RichText::new("Already clean").color(GOOD));
                            }
                            Some(_) => {
                                ui.label(widgets::strong(fmt_bytes(bytes), 16.0));
                                ui.label(widgets::faint(format!("{} files", fmt_num(files))));
                            }
                        }
                    });
                });
            });

        });
    }

    /// Full-page review: a fixed summary and toolbar, with the list using all the
    /// remaining height instead of a small box nested in the category page.
    fn render_review(&self, ui: &mut Ui, acts: &mut Vec<CAct>) {
        let c = &self.cleaner;
        let Some(review) = c.review.as_ref() else {
            return;
        };
        let i = review.category;
        let cat = c.cats[i];
        let Some(Some(result)) = c.results.get(i) else {
            ui.label(widgets::dim(
                "These results are no longer available. Scan again.",
            ));
            if ui.add(widgets::ghost("Back to categories")).clicked() {
                acts.push(CAct::CloseReview);
            }
            return;
        };
        let excluded = &c.excluded[i];
        ui.horizontal(|ui| {
            if ui
                .add(widgets::ghost(widgets::icon_text(
                    ic::BACK,
                    "All categories",
                    14.0,
                    TEXT,
                )))
                .on_hover_text("Back to the category list (Esc). Your choices are kept.")
                .clicked()
            {
                acts.push(CAct::CloseReview);
            }
            ui.label(widgets::strong(format!("Review: {}", cat.name), 22.0));
        });
        ui.add_space(4.0);
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut sel = c.selected[i];
                if ui.checkbox(&mut sel, "Clean this category").changed() {
                    acts.push(CAct::SetSel(i, sel));
                }
                ui.label(widgets::strong(fmt_bytes(c.cat_bytes(i)), 16.0));
                ui.label(widgets::dim(format!(
                    "in {} of {} files will be cleaned. {} kept.",
                    fmt_num(c.cat_files(i)),
                    fmt_num(result.count),
                    fmt_num(excluded.len() as u64)
                )));
            });
            ui.label(widgets::faint(cat.desc));
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let mut filter = review.filter.clone();
            if widgets::search_box(ui, &mut filter, "Filter by name or folder", 240.0).changed() {
                acts.push(CAct::ReviewFilter(filter));
            }
            ui.label(widgets::dim("Sort"));
            for (sort, label) in [
                (ReviewSort::Largest, "Largest"),
                (ReviewSort::Newest, "Newest"),
                (ReviewSort::Oldest, "Oldest"),
                (ReviewSort::Path, "Folder"),
            ] {
                if ui.add(widgets::chip(label, review.sort == sort)).clicked() {
                    acts.push(CAct::ReviewSort(sort));
                }
            }
        });
        let shown = review.view.len();
        let filtered = !review.filter.trim().is_empty();
        ui.horizontal_wrapped(|ui| {
            let scope = if filtered {
                format!("the {} shown", fmt_num(shown as u64))
            } else {
                "all".to_string()
            };
            if ui
                .add_enabled(shown > 0, widgets::small(format!("Keep {scope}")))
                .on_hover_text("Kept files are skipped when you clean.")
                .clicked()
            {
                acts.push(CAct::KeepShown);
            }
            if ui
                .add_enabled(shown > 0, widgets::small(format!("Include {scope}")))
                .clicked()
            {
                acts.push(CAct::IncludeShown);
            }
            if filtered {
                ui.label(widgets::faint(format!(
                    "{} of {} files match the filter. Bulk actions only affect matching files.",
                    fmt_num(shown as u64),
                    fmt_num(result.count)
                )));
            } else {
                ui.label(widgets::faint("Untick anything you want to keep."));
            }
        });
        ui.add_space(2.0);
        if shown == 0 {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(widgets::dim("No files match this filter.")));
            return;
        }
        ScrollArea::vertical()
            .id_salt(("review_list", i))
            .auto_shrink([false, false])
            .show_rows(ui, 44.0, shown, |ui, range| {
                for &index in &review.view[range] {
                    let f = &result.files[index];
                    widgets::row(ui, 44.0, |ui| {
                        let mut inc = !excluded.contains(&f.path);
                        if ui.checkbox(&mut inc, "").changed() {
                            acts.push(CAct::Toggle(i, f.path.clone(), f.size, inc));
                        }
                        let icon_color = if inc { ACCENT } else { TEXT_FAINT };
                        ui.label(widgets::icon(ic::FILE, 16.0, icon_color));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            if ui
                                .add(widgets::small(widgets::icon(ic::OPEN, 13.0, TEXT_DIM)))
                                .on_hover_text("Show in Explorer")
                                .clicked()
                            {
                                acts.push(CAct::Show(f.path.clone()));
                            }
                            widgets::cell(ui, 110.0, true, widgets::faint(fmt_age(f.modified)));
                            widgets::cell(ui, 84.0, true, widgets::strong(fmt_bytes(f.size), 14.0));
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                let name = f
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                let folder = f
                                    .path
                                    .parent()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default();
                                let full = f.path.display().to_string();
                                let response = ui
                                    .vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 1.0;
                                        let text = if inc {
                                            RichText::new(&name).color(TEXT)
                                        } else {
                                            RichText::new(&name).color(TEXT_FAINT).strikethrough()
                                        };
                                        ui.add(Label::new(text).truncate());
                                        ui.add(Label::new(widgets::faint(folder)).truncate());
                                    })
                                    .response
                                    .interact(egui::Sense::click())
                                    .on_hover_text(&full);
                                response.context_menu(|ui| {
                                    if ui.button("Copy full path").clicked() {
                                        acts.push(CAct::CopyPath(full.clone()));
                                        ui.close_menu();
                                    }
                                    if ui.button("Show in Explorer").clicked() {
                                        acts.push(CAct::Show(f.path.clone()));
                                        ui.close_menu();
                                    }
                                });
                            });
                        });
                    });
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_cleaner_snapshot_requires_refresh_before_cleaning() {
        let mut app = App::test_fixture();
        app.cleaner.cache.from_disk = true;
        app.ask_clean();
        assert!(app.modal.is_none());
        app.start_clean(&egui::Context::default());
        assert!(
            !app.cleaner.clean.running(),
            "restored Recycle Bin totals must never authorize an empty-bin action"
        );
    }
    #[test]
    fn cleaner_cache_preserves_kept_files_and_rejects_changed_category_schema() {
        let cats = categories();
        let path = PathBuf::from(r"C:\Users\Example\AppData\Local\Temp\kept.tmp");
        let mut results: Vec<Option<CatResult>> = (0..cats.len())
            .map(|_| Some(CatResult::default()))
            .collect();
        results[0] = Some(CatResult {
            files: vec![JunkFile {
                path: path.clone(),
                size: 12,
                modified: Some(SystemTime::UNIX_EPOCH),
                created: Some(SystemTime::UNIX_EPOCH),
            }],
            bytes: 12,
            count: 1,
            ..Default::default()
        });
        let mut excluded = vec![HashSet::new(); cats.len()];
        excluded[0].insert(path.clone());
        let disk = CleanerDisk {
            categories: cats.iter().map(|c| c.name.into()).collect(),
            results,
            excluded,
            review: Some((0, "kept".into(), ReviewSort::Path)),
        };
        let mut decoded: CleanerDisk =
            serde_json::from_slice(&serde_json::to_vec(&disk).unwrap()).unwrap();
        validate_cleaner_cache(&decoded, &cats).unwrap();
        assert!(decoded.excluded[0].contains(&path));
        assert_eq!(decoded.results[0].as_ref().unwrap().files[0].size, 12);
        decoded.categories[0] = "a different cleanup definition".into();
        assert!(validate_cleaner_cache(&decoded, &cats).is_err());
    }

    #[test]
    fn target_expansion_keeps_files_and_supports_question_mark_wildcards() {
        let root = std::env::temp_dir().join(format!("pcsweep-patterns-{}", std::process::id()));
        fs::create_dir_all(root.join("profile1").join("Cache")).unwrap();
        fs::create_dir_all(root.join("profile10").join("Cache")).unwrap();
        fs::write(
            root.join("profile1").join("Cache").join("one.dat"),
            b"cache",
        )
        .unwrap();
        let pattern = root
            .join("profile?")
            .join("Cache")
            .to_string_lossy()
            .into_owned();
        let expanded = expand_pattern(&pattern);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0], root.join("profile1").join("Cache"));
        let mut res = CatResult::default();
        walk(&expanded[0], &mut res, None, 0);
        assert_eq!(res.files.len(), 1);
        assert_eq!(res.bytes, 5);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_file_scan_skips_new_cache_files() {
        let root = std::env::temp_dir().join(format!("pcsweep-age-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("recent.dat"), b"active cache").unwrap();
        let mut res = CatResult::default();
        walk(
            &root,
            &mut res,
            SystemTime::now().checked_sub(Duration::from_secs(86_400)),
            0,
        );
        assert!(res.files.is_empty());
        assert_eq!(res.recent_skipped, 1);
        fs::remove_dir_all(root).unwrap();
    }
}
