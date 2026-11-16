//! Portable, user-controlled updates. Network work never runs on the UI thread.
use crate::app::{Action, App, Modal, ToastKind};
use crate::task::Task;
use crate::theme::*;
use crate::util::{self, fmt_bytes};
use crate::widgets::{self, ic};
use eframe::egui::{self, RichText, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const API: &str = "https://api.github.com/repos/kashrtx/pcsweep/releases/latest";
const RELEASES: &str = "https://github.com/kashrtx/pcsweep/releases/latest";
const MAX_EXE: u64 = 128 * 1024 * 1024;
const MAX_JSON: u64 = 1024 * 1024;
const DAY: u64 = 86_400;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Manifest {
    schema: u32,
    version: String,
    revision: u64,
    commit: String,
    arch: String,
    asset: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Deserialize)]
struct Asset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Clone)]
pub struct AvailableUpdate {
    manifest: Manifest,
    url: String,
    release_url: String,
    notes: String,
    tag: String,
}

impl AvailableUpdate {
    fn label(&self) -> String {
        format!(
            "{} · build {}",
            self.manifest.version, self.manifest.revision
        )
    }
    fn identity(&self) -> String {
        format!(
            "{}:{}:{}",
            self.manifest.version, self.manifest.revision, self.manifest.commit
        )
    }
}

#[derive(Clone)]
pub enum UpdateAction {
    Install(AvailableUpdate),
    DownloadOnly(AvailableUpdate),
}

#[derive(Default)]
struct DownloadProgress {
    downloaded: AtomicU64,
    total: AtomicU64,
    cancel: AtomicBool,
}

enum DownloadResult {
    Restart,
    Saved(PathBuf),
}

pub struct UpdateState {
    check: Task<Result<Option<AvailableUpdate>, String>>,
    install: Task<Result<DownloadResult, String>>,
    available: Option<AvailableUpdate>,
    progress: Arc<DownloadProgress>,
    automatic: bool,
    last_checked: u64,
    last_attempt: u64,
    skipped: String,
    status: String,
    error: Option<String>,
    saved: Option<PathBuf>,
}

impl Default for UpdateState {
    fn default() -> Self {
        let settings = util::load_settings();
        let last = settings
            .get("updates.last_check")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Self {
            check: Task::default(),
            install: Task::default(),
            available: None,
            progress: Arc::new(DownloadProgress::default()),
            automatic: settings.get("updates.automatic").is_some_and(|s| s == "1"),
            last_checked: last,
            last_attempt: last,
            skipped: settings.get("updates.skipped").cloned().unwrap_or_default(),
            status: "Check for a new version whenever you're ready.".into(),
            error: std::env::args_os().any(|a| a == "--pcsweep-update-failed").then(|| "The last update could not finish. The previous version has been restored; check Activity log for details.".into()),
            saved: None,
        }
    }
}

impl UpdateState {
    pub fn automatic_checks(&self) -> bool {
        self.automatic
    }
    pub fn has_available_update(&self) -> bool {
        self.available
            .as_ref()
            .is_some_and(|u| self.skipped != u.identity())
    }
    pub fn running(&self) -> bool {
        self.check.running() || self.install.running()
    }
    pub fn installing(&self) -> bool {
        self.install.running()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn revision() -> u64 {
    option_env!("PCSWEEP_BUILD_REVISION")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
fn commit() -> &'static str {
    option_env!("PCSWEEP_BUILD_COMMIT").unwrap_or("local")
}

fn base_version(s: &str) -> Result<semver::Version, String> {
    let mut v = semver::Version::parse(s)
        .map_err(|_| "The update contains an invalid version.".to_string())?;
    v.build = semver::BuildMetadata::EMPTY;
    Ok(v)
}

fn is_newer(
    m: &Manifest,
    current: &str,
    current_revision: u64,
    current_commit: &str,
) -> Result<bool, String> {
    let remote = base_version(&m.version)?;
    let local = base_version(current)?;
    if remote != local {
        return Ok(remote > local);
    }
    if m.commit == current_commit {
        return Ok(false);
    }
    Ok(m.revision > current_revision)
}

fn valid_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate_manifest(m: &Manifest, tag: &str, exe: &Asset) -> Result<(), String> {
    let version = base_version(&m.version)?;
    let base_tag = format!("v{}", m.version);
    let separator = if m.version.contains('+') { "." } else { "+" };
    let build_tag = format!("{base_tag}{separator}build.{}", m.revision);
    if m.schema != 1
        || !version.pre.is_empty()
        || m.arch != "windows-x64"
        || m.asset != "PCSweep.exe"
        || m.revision == 0
        || !valid_hex(&m.commit, 40)
        || !valid_hex(&m.sha256, 64)
        || !(4096..=MAX_EXE).contains(&m.size)
        || m.size != exe.size
        || (tag != base_tag && tag != build_tag)
    {
        return Err(
            "The update metadata did not pass validation. Your installed app is unchanged.".into(),
        );
    }
    if let Some(digest) = &exe.digest {
        if !digest.eq_ignore_ascii_case(&format!("sha256:{}", m.sha256)) {
            return Err(
                "The release and update checksums disagree. The update was refused.".into(),
            );
        }
    }
    validate_asset_url(&exe.browser_download_url, tag, "PCSweep.exe")
}

fn validate_asset_url(url: &str, tag: &str, file: &str) -> Result<(), String> {
    // Tags are parsed as SemVer before reaching here. Do not accept another repo,
    // arbitrary URLs from release descriptions, credentials or query strings.
    let decoded = url.replace("%2B", "+").replace("%2b", "+");
    if decoded == format!("https://github.com/kashrtx/pcsweep/releases/download/{tag}/{file}") {
        Ok(())
    } else {
        Err("The update URL is not an official PCSweep release asset.".into())
    }
}

fn allowed_redirect(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    matches!(
        host,
        "github.com"
            | "api.github.com"
            | "release-assets.githubusercontent.com"
            | "objects.githubusercontent.com"
            | "github-releases.githubusercontent.com"
    )
}

fn request(url: &str) -> Result<ureq::Response, String> {
    let tls = ureq::native_tls::TlsConnector::builder()
        .min_protocol_version(Some(ureq::native_tls::Protocol::Tlsv12))
        .build()
        .map_err(|_| "Windows could not initialize a secure update connection.".to_string())?;
    let agent = ureq::AgentBuilder::new()
        .https_only(true)
        .tls_connector(Arc::new(tls))
        .redirects(0)
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(20))
        .timeout_write(Duration::from_secs(20))
        .build();
    let mut url = url.to_string();
    for _ in 0..5 {
        if !allowed_redirect(&url) {
            return Err("An update download redirected outside GitHub. It was stopped.".into());
        }
        let response = agent
            .get(&url)
            .set("User-Agent", concat!("PCSweep/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(90))
            .call();
        let response = match response {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) if code == 403 || code == 429 => {
                return Err("GitHub has temporarily limited update checks. Try again later; PCSweep still works normally.".into());
            }
            Err(ureq::Error::Status(404, _)) => return Err("No compatible update is published yet. Try again later.".into()),
            Err(ureq::Error::Status(code, _)) => return Err(format!("GitHub returned HTTP {code}. Try again later.")),
            Err(_) => return Err("Could not connect to GitHub. Check your connection or proxy, then try again. Your current app still works.".into()),
        };
        if (300..400).contains(&response.status()) {
            url = response
                .header("Location")
                .ok_or("The download redirect was incomplete.")?
                .to_string();
            continue;
        }
        if response.status() != 200 {
            return Err("The update server returned an unexpected response.".into());
        }
        return Ok(response);
    }
    Err("The update download redirected too many times.".into())
}

fn read_limited(mut reader: impl Read, max: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "The update download was interrupted.".to_string())?;
    if bytes.len() as u64 > max {
        return Err("The update response exceeded its expected size.".into());
    }
    Ok(bytes)
}

fn check_latest() -> Result<Option<AvailableUpdate>, String> {
    check_with_fetch(
        |url, limit| read_limited(request(url)?.into_reader(), limit),
        env!("CARGO_PKG_VERSION"),
        revision(),
        commit(),
    )
}

fn check_with_fetch(
    mut fetch: impl FnMut(&str, u64) -> Result<Vec<u8>, String>,
    current_version: &str,
    current_revision: u64,
    current_commit: &str,
) -> Result<Option<AvailableUpdate>, String> {
    let release: Release = serde_json::from_slice(&fetch(API, MAX_JSON)?)
        .map_err(|_| "GitHub returned invalid release information.".to_string())?;
    if release.draft || release.prerelease {
        return Err("The latest release is not a stable public build.".into());
    }
    let remote_version = base_version(
        release
            .tag_name
            .strip_prefix('v')
            .ok_or("The release tag is invalid.")?,
    )?;
    if remote_version < base_version(current_version)? {
        return Ok(None);
    }
    if !release
        .html_url
        .starts_with("https://github.com/kashrtx/pcsweep/releases/tag/")
    {
        return Err("The release page could not be verified.".into());
    }
    let manifest_asset = release
        .assets
        .iter()
        .find(|a| a.name == "update.json")
        .ok_or(
        "This release has no in-app update metadata. Use the official release page to download it.",
    )?;
    let exe = release
        .assets
        .iter()
        .find(|a| a.name == "PCSweep.exe")
        .ok_or("The Windows executable is missing from this release.")?;
    validate_asset_url(
        &manifest_asset.browser_download_url,
        &release.tag_name,
        "update.json",
    )?;
    let bytes = fetch(&manifest_asset.browser_download_url, 16 * 1024)?;
    if manifest_asset.size != bytes.len() as u64 {
        return Err("The update metadata download was incomplete.".into());
    }
    if let Some(expected) = &manifest_asset.digest {
        if !expected.eq_ignore_ascii_case(&format!("sha256:{:x}", Sha256::digest(&bytes))) {
            return Err("The update metadata checksum is incorrect.".into());
        }
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|_| "The update metadata format is invalid.".to_string())?;
    validate_manifest(&manifest, &release.tag_name, exe)?;
    if !is_newer(&manifest, current_version, current_revision, current_commit)? {
        return Ok(None);
    }
    Ok(Some(AvailableUpdate {
        manifest,
        url: exe.browser_download_url.clone(),
        release_url: release.html_url,
        notes: release
            .body
            .unwrap_or_default()
            .chars()
            .take(30_000)
            .collect(),
        tag: release.tag_name,
    }))
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let n = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn verify_executable(path: &Path, size: u64, expected: &str) -> Result<(), String> {
    let md = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if util::is_reparse(&md)
        || !md.is_file()
        || md.len() != size
        || !sha256(path)?.eq_ignore_ascii_case(expected)
    {
        return Err(
            "The downloaded file failed size or SHA-256 verification. It will not be installed."
                .into(),
        );
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut header = [0; 64];
    file.read_exact(&mut header)
        .map_err(|_| "The downloaded executable is incomplete.".to_string())?;
    if &header[..2] != b"MZ" {
        return Err("The download is not a Windows executable.".into());
    }
    Ok(())
}

fn unique() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
}
fn update_dir() -> PathBuf {
    util::log_path().with_file_name("updates")
}

#[derive(Serialize, Deserialize)]
struct InstallRequest {
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    size: u64,
    sha256: String,
    old_sha256: String,
    parent_pid: u32,
    parent_created: u64,
    token: String,
}

fn creation_time(handle: isize) -> Result<u64, String> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err("Could not verify the running app's identity.".into());
    }
    Ok((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64)
}

fn reject_links(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Update paths must be absolute.".into());
    }
    for p in path.ancestors() {
        let md = fs::symlink_metadata(p).map_err(|e| e.to_string())?;
        if util::is_reparse(&md) {
            return Err("Updates cannot be installed through links or junctions. Move PCSweep to a regular folder first.".into());
        }
    }
    Ok(())
}

fn download_update(
    update: AvailableUpdate,
    restart: bool,
    progress: Arc<DownloadProgress>,
) -> Result<DownloadResult, String> {
    validate_asset_url(&update.url, &update.tag, "PCSweep.exe")?;
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let current = fs::canonicalize(&current).map_err(|e| e.to_string())?;
    let nonce = unique();
    let job = update_dir().join(format!("job-{nonce}"));
    fs::create_dir_all(&job)
        .map_err(|e| format!("Could not create an update download folder: {e}"))?;
    reject_links(&job)?;
    let job = fs::canonicalize(&job).map_err(|e| e.to_string())?;
    let downloaded = job.join("PCSweep.exe");
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&downloaded)
            .map_err(|e| e.to_string())?;
        let mut input = request(&update.url)?.into_reader();
        progress
            .total
            .store(update.manifest.size, Ordering::Relaxed);
        let mut size = 0u64;
        let mut buf = [0; 64 * 1024];
        let start = Instant::now();
        loop {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err("Download cancelled. Your installed app is unchanged.".into());
            }
            if start.elapsed() > Duration::from_secs(180) {
                return Err("The update download timed out. Try again later.".into());
            }
            let n = input
                .read(&mut buf)
                .map_err(|_| "The update download was interrupted. Try again.".to_string())?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > update.manifest.size {
                return Err("The update download exceeded its expected size.".into());
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            progress.downloaded.store(size, Ordering::Relaxed);
        }
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        verify_executable(&downloaded, update.manifest.size, &update.manifest.sha256)?;
        if progress.cancel.load(Ordering::Relaxed) {
            return Err("Download cancelled. Your installed app is unchanged.".into());
        }
        if !restart {
            return Ok(DownloadResult::Saved(downloaded.clone()));
        }
        reject_links(&current)?;
        let parent = current
            .parent()
            .ok_or("The app's folder could not be located.")?;
        let staged = parent.join(format!(".pcsweep-update-{nonce}.exe"));
        let backup = parent.join(format!(".pcsweep-backup-{nonce}.exe"));
        // A protected/readonly portable location gets a verified download instead
        // of an elevation request or a partially installed app.
        let Ok(mut stage) = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)
        else {
            return Ok(DownloadResult::Saved(downloaded.clone()));
        };
        let copy = (|| {
            let mut input = File::open(&downloaded).map_err(|e| e.to_string())?;
            std::io::copy(&mut input, &mut stage).map_err(|e| e.to_string())?;
            stage.sync_all().map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        })();
        drop(stage);
        if let Err(e) = copy {
            let _ = fs::remove_file(&staged);
            return Err(e);
        }
        let handed = handoff(&current, &staged, &backup, &job, &update.manifest, nonce);
        if handed.is_err() {
            let _ = fs::remove_file(&staged);
        }
        handed?;
        Ok(DownloadResult::Restart)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&downloaded);
    }
    result
}

fn handoff(
    current: &Path,
    staged: &Path,
    backup: &Path,
    job: &Path,
    manifest: &Manifest,
    token: String,
) -> Result<(), String> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let helper = job.join("PCSweep-updater.exe");
    let bytes = fs::read(current).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&helper)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let request = InstallRequest {
        target: current.to_path_buf(),
        staged: staged.to_path_buf(),
        backup: backup.to_path_buf(),
        size: manifest.size,
        sha256: manifest.sha256.clone(),
        old_sha256: sha256(current)?,
        parent_pid: std::process::id(),
        parent_created: creation_time(unsafe { GetCurrentProcess() })?,
        token,
    };
    let path = job.join("request.json");
    let data = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(&data).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let digest = format!("{:x}", Sha256::digest(&data));
    let mut child = util::hidden_command(
        helper
            .to_str()
            .ok_or("The updater path could not be encoded.")?,
    )
    .arg("--pcsweep-update-helper")
    .arg(&path)
    .arg(digest)
    .spawn()
    .map_err(|e| format!("Could not start the update helper: {e}"))?;
    let start = Instant::now();
    loop {
        if fs::read_to_string(job.join("ready")).is_ok_and(|s| s == request.token) {
            return Ok(());
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Err(
                "The update helper stopped before it was ready. Your installed app is unchanged."
                    .into(),
            );
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            return Err(
                "The update helper did not start in time. Your installed app is unchanged.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn validate_request_layout(
    request: &InstallRequest,
    job: &Path,
    helper: &Path,
    root: &Path,
) -> Result<(), String> {
    if helper.file_name().and_then(|s| s.to_str()) != Some("PCSweep-updater.exe")
        || helper.parent() != Some(job)
        || job.parent() != Some(root)
        || job.file_name().and_then(|s| s.to_str())
            != Some(format!("job-{}", request.token).as_str())
        || request.token.is_empty()
        || request.token.len() > 80
        || !request
            .token
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-')
        || !valid_hex(&request.sha256, 64)
        || !valid_hex(&request.old_sha256, 64)
        || !(4096..=MAX_EXE).contains(&request.size)
        || request.parent_pid == 0
        || request
            .target
            .extension()
            .is_none_or(|s| !s.eq_ignore_ascii_case("exe"))
        || request.staged.parent() != request.target.parent()
        || request.backup.parent() != request.target.parent()
        || request.staged.file_name().and_then(|s| s.to_str())
            != Some(format!(".pcsweep-update-{}.exe", request.token).as_str())
        || request.backup.file_name().and_then(|s| s.to_str())
            != Some(format!(".pcsweep-backup-{}.exe", request.token).as_str())
        || request.target == request.staged
        || request.target == request.backup
    {
        return Err("The update handoff paths are invalid.".into());
    }
    Ok(())
}

fn validate_request(request: &InstallRequest, job: &Path, helper: &Path) -> Result<(), String> {
    let root = fs::canonicalize(update_dir()).map_err(|e| e.to_string())?;
    validate_request_layout(request, job, helper, &root)?;
    reject_links(job)?;
    reject_links(&request.target)?;
    reject_links(&request.staged)?;
    if request.backup.exists() {
        return Err("The update backup already exists; no files were changed.".into());
    }
    if sha256(&request.target)? != request.old_sha256 || sha256(helper)? != request.old_sha256 {
        return Err("The app changed during update preparation. Nothing was installed.".into());
    }
    verify_executable(&request.staged, request.size, &request.sha256)
}

fn restart_original_if_verified(
    request: &InstallRequest,
    launch: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    reject_links(&request.target)?;
    if sha256(&request.target)? != request.old_sha256 {
        return Err("The original app changed; it will not be restarted automatically.".into());
    }
    launch(&request.target)
}

fn replace_with_backup(target: &Path, staged: &Path, backup: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let wide = |p: &Path| {
        p.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let (target, staged, backup) = (wide(target), wide(staged), wide(backup));
    if unsafe {
        ReplaceFileW(
            target.as_ptr(),
            staged.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        Err(format!(
            "Could not replace the portable app: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

fn restore_backup(target: &Path, backup: &Path) -> Result<(), String> {
    if !backup.is_file() {
        return Err("The update backup was not found.".into());
    }
    if target.exists() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
        let wide = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
        };
        let (target, backup) = (wide(target), wide(backup));
        if unsafe {
            ReplaceFileW(
                target.as_ptr(),
                backup.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "Could not restore the app backup: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    } else {
        fs::rename(backup, target).map_err(|e| e.to_string())
    }
}

fn run_install(path: &Path, expected_digest: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if crate::system::is_admin() {
        return Err("The portable update helper must run without administrator rights.".into());
    }
    if path.file_name().and_then(|s| s.to_str()) != Some("request.json")
        || !valid_hex(expected_digest, 64)
    {
        return Err("Invalid update request.".into());
    }
    let data = read_limited(File::open(path).map_err(|e| e.to_string())?, 32 * 1024)?;
    if format!("{:x}", Sha256::digest(&data)) != expected_digest {
        return Err("The update request changed before installation.".into());
    }
    let request: InstallRequest = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    let job = path.parent().ok_or("Invalid update folder.")?;
    let helper = fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    validate_request(&request, job, &helper)?;
    let handle = unsafe {
        OpenProcess(
            SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            request.parent_pid,
        )
    };
    if handle == 0 {
        return Err("The running app could not be identified.".into());
    }
    let identity = creation_time(handle);
    if identity != Ok(request.parent_created) {
        unsafe {
            CloseHandle(handle);
        }
        return Err("The running app identity changed.".into());
    }
    fs::write(job.join("ready"), &request.token).map_err(|e| e.to_string())?;
    let waited = unsafe { WaitForSingleObject(handle, 90_000) };
    unsafe {
        CloseHandle(handle);
    }
    if waited != WAIT_OBJECT_0 {
        return Err("The app did not close in time. Your installed app is unchanged.".into());
    }
    // Revalidate after the wait: preparation can be separated from replacement
    // by a long-running process shutdown.
    if let Err(error) = validate_request(&request, job, &helper) {
        let _ = restart_original_if_verified(&request, |target| {
            Command::new(target)
                .arg("--pcsweep-update-failed")
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        return Err(error);
    }
    if let Err(error) = replace_with_backup(&request.target, &request.staged, &request.backup) {
        if request.backup.is_file() {
            let _ = restore_backup(&request.target, &request.backup);
        }
        let _ = Command::new(&request.target)
            .arg("--pcsweep-update-failed")
            .spawn();
        return Err(error);
    }
    let ack = job.join("started");
    let mut child = match Command::new(&request.target)
        .arg("--pcsweep-update-ack")
        .arg(&ack)
        .arg(&request.token)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            restore_backup(&request.target, &request.backup)?;
            let _ = Command::new(&request.target)
                .arg("--pcsweep-update-failed")
                .spawn();
            return Err(format!(
                "The new version could not start; the previous version was restored: {e}"
            ));
        }
    };
    let start = Instant::now();
    loop {
        if fs::read_to_string(&ack).is_ok_and(|s| s == request.token) {
            util::log("PCSweep updated successfully; startup was confirmed.");
            let _ = fs::remove_file(&request.backup);
            let _ = fs::remove_file(&ack);
            let _ = fs::remove_file(job.join("ready"));
            let _ = fs::remove_file(job.join("PCSweep.exe"));
            let _ = fs::remove_file(path);
            return Ok(());
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some()
            || start.elapsed() > Duration::from_secs(60)
        {
            let _ = child.kill();
            let _ = child.wait();
            restore_backup(&request.target, &request.backup)?;
            let _ = Command::new(&request.target)
                .arg("--pcsweep-update-failed")
                .spawn();
            return Err(
                "The new version did not finish startup. The previous version was restored.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Call before creating the GUI. Helpers never enter eframe or load settings UI.
pub fn run_helper_mode() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|a| a != "--pcsweep-update-helper") {
        return None;
    }
    let result = if args.len() == 4 {
        run_install(Path::new(&args[2]), &args[3].to_string_lossy())
    } else {
        Err("Invalid updater arguments.".into())
    };
    match result {
        Ok(()) => Some(0),
        Err(error) => {
            util::log(format!("App update failed: {error}"));
            Some(1)
        }
    }
}

/// Acknowledge only after eframe has reached its first UI frame. A downloaded
/// executable that cannot initialize the window must never remove the backup.
pub fn confirm_update_startup() {
    static ACK: Once = Once::new();
    ACK.call_once(|| {
        let args: Vec<_> = std::env::args_os().collect();
        if args.len() != 4 || args[1] != "--pcsweep-update-ack" {
            return;
        }
        let path = Path::new(&args[2]);
        let Some(job) = path.parent() else {
            return;
        };
        if path.file_name().is_none_or(|s| s != "started")
            || job.parent() != fs::canonicalize(update_dir()).ok().as_deref()
            || reject_links(job).is_err()
        {
            return;
        }
        let token = args[3].to_string_lossy();
        if token.is_empty()
            || token.len() > 80
            || !token.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        {
            return;
        }
        if let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(path) {
            let _ = file.write_all(token.as_bytes());
        }
    });
}

impl App {
    pub(crate) fn set_automatic_update_checks(&mut self, enabled: bool) {
        self.updates.automatic = enabled;
        util::save_settings(&[(
            "updates.automatic".into(),
            if enabled { "1" } else { "0" }.into(),
        )]);
    }

    pub(crate) fn begin_update_check(&mut self, ctx: &egui::Context) {
        if self.updates.running() {
            return;
        }
        self.updates.error = None;
        self.updates.status = "Checking the official PCSweep release…".into();
        self.updates.last_attempt = now();
        util::save_settings(&[(
            "updates.last_check".into(),
            self.updates.last_attempt.to_string(),
        )]);
        self.updates.check = Task::spawn(ctx, check_latest);
    }

    pub fn poll_updates(&mut self, ctx: &egui::Context) {
        let now = now();
        if self.updates.automatic
            && now.saturating_sub(self.updates.last_attempt) >= DAY
            && !self.updates.running()
        {
            self.begin_update_check(ctx);
        }
        if let Some(result) = self.updates.check.poll() {
            self.updates.last_checked = now;
            match result.and_then(|r| r) {
                Ok(update) => {
                    self.updates.status = if update.is_some() {
                        "A new version is ready.".into()
                    } else {
                        "You're using the latest available build.".into()
                    };
                    if let Some(update) = &update {
                        if self.updates.skipped != update.identity() {
                            self.toast(
                                ToastKind::Info,
                                format!(
                                    "PCSweep {} is available on the Updates page.",
                                    update.label()
                                ),
                            );
                        }
                    }
                    self.updates.available = update;
                }
                Err(error) => {
                    self.updates.status = "Could not finish the update check.".into();
                    self.updates.error = Some(error);
                }
            }
        }
        if let Some(result) = self.updates.install.poll() {
            match result.and_then(|r| r) {
                Ok(DownloadResult::Restart) => {
                    util::log("A verified PCSweep update is ready. Restarting the app.");
                    self.force_exit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Ok(DownloadResult::Saved(path)) => {
                    self.updates.saved = Some(path);
                    self.updates.status = "Your verified download is ready.".into();
                    self.toast(
                        ToastKind::Success,
                        "Verified download saved. Open its folder to use the new version.",
                    );
                }
                Err(error) => {
                    self.updates.error = Some(error);
                    self.updates.status =
                        "The update was not installed. Your current app is unchanged.".into();
                }
            }
        }
    }

    pub fn execute_update(&mut self, action: UpdateAction, ctx: &egui::Context) {
        if self.updates.running() {
            return;
        }
        if self.mutations_running() {
            self.toast(
                ToastKind::Info,
                "Wait for the current operation to finish before updating PCSweep.",
            );
            return;
        }
        let (update, restart) = match action {
            UpdateAction::Install(update) => (update, !self.is_admin),
            UpdateAction::DownloadOnly(update) => (update, false),
        };
        self.updates.error = None;
        self.updates.saved = None;
        self.updates.progress = Arc::new(DownloadProgress::default());
        let progress = self.updates.progress.clone();
        self.updates.status = "Downloading and verifying the update…".into();
        self.updates.install = Task::spawn(ctx, move || download_update(update, restart, progress));
    }

    pub fn page_updates(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut check = false;
        let mut install = None;
        let mut skip = None;
        let mut automatic = self.updates.automatic;
        ScrollArea::vertical().id_salt("updates_page").auto_shrink([false, false]).show(ui, |ui| {
            widgets::page_header(ui, "App updates", "Keep PCSweep current. Checks and downloads run in the background; restart only when you're ready.");
            widgets::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(widgets::icon(ic::UPDATE, 25.0, ACCENT));
                    ui.vertical(|ui| {
                        ui.label(widgets::strong(format!("PCSweep {}", env!("CARGO_PKG_VERSION")), 20.0));
                        ui.label(widgets::dim(if revision() == 0 { "Local build".into() } else { format!("Build {} · {}", revision(), &commit()[..commit().len().min(8)]) }));
                    });
                });
                ui.add_space(8.0);
                ui.label(widgets::dim(&self.updates.status));
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(!self.updates.running(), widgets::primary("Check for updates")).clicked() { check = true; }
                    if self.updates.check.running() { ui.spinner(); }
                    if ui.add(widgets::ghost("Official releases")).clicked() { util::open_uri(RELEASES); }
                    if self.updates.last_checked > 0 {
                        let time = chrono::DateTime::from_timestamp(self.updates.last_checked as i64, 0)
                            .map(|t| t.with_timezone(&chrono::Local).format("%b %d, %H:%M").to_string()).unwrap_or_default();
                        ui.label(widgets::faint(format!("Last checked {time}")));
                    }
                });
                ui.add_space(6.0);
                ui.checkbox(&mut automatic, "Check once a day while PCSweep is running, including in the background");
                ui.label(widgets::faint("Optional and off by default. Update checks contact GitHub without sending a device ID, app inventory or activity history."));
            });
            if let Some(error) = &self.updates.error {
                ui.add_space(8.0);
                widgets::banner(ui, WARN, ic::INFO, |ui| { ui.label(error); });
            }
            if self.updates.install.running() {
                ui.add_space(8.0);
                widgets::card(ui, |ui| {
                    ui.horizontal(|ui| { ui.spinner(); ui.label(widgets::strong("Preparing your update", 17.0)); });
                    let done = self.updates.progress.downloaded.load(Ordering::Relaxed);
                    let total = self.updates.progress.total.load(Ordering::Relaxed);
                    if total > 0 {
                        ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!("{} of {}", fmt_bytes(done), fmt_bytes(total))));
                    }
                    ui.label(widgets::dim("Your current app stays intact until the download has been verified."));
                    if ui.add(widgets::ghost("Cancel download")).clicked() {
                        self.updates.progress.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }
            if let Some(path) = &self.updates.saved {
                ui.add_space(8.0);
                widgets::card(ui, |ui| {
                    ui.label(widgets::strong("Verified download ready", 17.0));
                    ui.label(widgets::dim("This session or app location does not support automatic replacement. Close PCSweep, then replace your portable copy with this download."));
                    if ui.add(widgets::primary("Show downloaded app")).clicked() { util::show_in_explorer(path); }
                });
            }
            if let Some(update) = &self.updates.available {
                ui.add_space(8.0);
                let skipped = self.updates.skipped == update.identity();
                widgets::card(ui, |ui| {
                    ui.label(widgets::strong(format!("Available: {}", update.label()), 20.0));
                    ui.label(widgets::dim(format!("{} download · Windows x64", fmt_bytes(update.manifest.size))));
                    ui.add_space(6.0);
                    if skipped { ui.label(widgets::faint("You've skipped this build. You can still install it below.")); }
                    if self.is_admin {
                        ui.label(widgets::dim("Automatic replacement runs in a normal user session. You can download a verified copy now, or reopen PCSweep normally for Update and restart."));
                    }
                    let busy = self.updates.running() || self.mutations_running();
                    ui.horizontal_wrapped(|ui| {
                        let label = if self.is_admin { "Download verified copy" } else { "Update and restart" };
                        if ui.add_enabled(!busy, widgets::primary(label)).clicked() { install = Some(update.clone()); }
                        if ui.add_enabled(!self.updates.installing(), widgets::ghost(if skipped { "Show notifications for this build" } else { "Skip this build" })).clicked() {
                            skip = Some(if skipped { String::new() } else { update.identity() });
                        }
                        if ui.add(widgets::ghost("Release page")).clicked() { util::open_uri(&update.release_url); }
                    });
                    if self.mutations_running() { ui.label(widgets::faint("Finish the current operation before installing an update.")); }
                    ui.add_space(6.0);
                    ui.label(widgets::strong("What's new", 15.0));
                    // Plain text deliberately avoids executing HTML/links from release notes.
                    ui.add(egui::Label::new(RichText::new(&update.notes).color(TEXT_DIM)).wrap());
                });
            }
        });
        if automatic != self.updates.automatic {
            self.set_automatic_update_checks(automatic);
        }
        if let Some(skipped) = skip {
            self.updates.skipped = skipped.clone();
            util::save_settings(&[("updates.skipped".into(), skipped)]);
        }
        if check {
            self.begin_update_check(&ctx);
        }
        if let Some(update) = install {
            self.modal = Some(Modal {
                title: if self.is_admin { "Download this update?".into() } else { "Update and restart PCSweep?".into() },
                body: format!("Download PCSweep {}, verify the complete file, and {}. Your settings and activity history are preserved.", update.label(), if self.is_admin { "save a portable copy" } else { "restart this app" }),
                details: vec![format!("Download: {}", fmt_bytes(update.manifest.size)), "Source: official kashrtx/pcsweep GitHub release".into()],
                note: Some(if self.is_admin { "The installed app will not be replaced in an administrator session.".into() } else { "If this folder is protected, the verified download will be saved for you instead.".into() }),
                confirm: if self.is_admin { "Download".into() } else { "Update and restart".into() },
                danger: false,
                action: Action::Update(if self.is_admin { UpdateAction::DownloadOnly(update) } else { UpdateAction::Install(update) }),
            });
        }
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> UpdateState {
    let manifest = Manifest {
        schema: 1,
        version: "1.3.0".into(),
        revision: 84,
        commit: "a".repeat(40),
        arch: "windows-x64".into(),
        asset: "PCSweep.exe".into(),
        size: 8_643_584,
        sha256: "b".repeat(64),
    };
    UpdateState {
        check: Task::default(), install: Task::default(),
        available: Some(AvailableUpdate {
            manifest, url: "https://github.com/kashrtx/pcsweep/releases/download/v1.3.0/PCSweep.exe".into(),
            release_url: "https://github.com/kashrtx/pcsweep/releases/tag/v1.3.0".into(),
            notes: "A smoother PCSweep, with more control over your PC.\n\n• Clearer privacy controls and safer cleanup.\n• Faster navigation with more helpful memory insights.\n• This update preserves your preferences and activity history.".into(),
            tag: "v1.3.0".into(),
        }),
        progress: Arc::new(DownloadProgress::default()), automatic: false,
        last_checked: 1_794_700_000, last_attempt: 1_794_700_000,
        skipped: String::new(), status: "A new version is ready.".into(),
        error: None, saved: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> Manifest {
        Manifest {
            schema: 1,
            version: "1.2.0".into(),
            revision: 42,
            commit: "a".repeat(40),
            arch: "windows-x64".into(),
            asset: "PCSweep.exe".into(),
            size: 4096,
            sha256: "b".repeat(64),
        }
    }
    fn asset() -> Asset {
        Asset {
            name: "PCSweep.exe".into(),
            size: 4096,
            browser_download_url:
                "https://github.com/kashrtx/pcsweep/releases/download/v1.2.0+build.42/PCSweep.exe"
                    .into(),
            digest: Some(format!("sha256:{}", "b".repeat(64))),
        }
    }
    #[test]
    fn versions_compare_explicit_build_revisions() {
        let m = manifest();
        assert!(is_newer(&m, "1.2.0", 41, "old").unwrap());
        assert!(!is_newer(&m, "1.2.0", 42, "old").unwrap());
        assert!(!is_newer(&m, "1.2.0", 1, &m.commit).unwrap());
        assert!(!is_newer(&m, "1.3.0", 1, "old").unwrap());
        assert!(is_newer(&m, "1.1.9", 100, "old").unwrap());
    }
    #[test]
    fn metadata_rejects_wrong_platform_size_digest_or_tag() {
        assert!(validate_manifest(&manifest(), "v1.2.0+build.42", &asset()).is_ok());
        let mut m = manifest();
        m.arch = "windows-arm64".into();
        assert!(validate_manifest(&m, "v1.2.0+build.42", &asset()).is_err());
        let mut m = manifest();
        m.size += 1;
        assert!(validate_manifest(&m, "v1.2.0+build.42", &asset()).is_err());
        assert!(validate_manifest(&manifest(), "v1.2.0+build.43", &asset()).is_err());
        let mut m = manifest();
        m.sha256 = "c".repeat(64);
        assert!(validate_manifest(&m, "v1.2.0+build.42", &asset()).is_err());
    }
    #[test]
    fn download_urls_cannot_escape_official_release() {
        assert!(validate_asset_url(
            &asset().browser_download_url,
            "v1.2.0+build.42",
            "PCSweep.exe"
        )
        .is_ok());
        assert!(validate_asset_url(
            "https://github.com/attacker/pcsweep/releases/download/v1.2.0/PCSweep.exe",
            "v1.2.0",
            "PCSweep.exe"
        )
        .is_err());
        assert!(!allowed_redirect("http://github.com/test"));
        assert!(!allowed_redirect("https://github.com.evil.example/test"));
        assert!(!allowed_redirect("https://github.com@evil.example/test"));
    }
    #[test]
    fn bounded_download_reader_rejects_oversized_response() {
        assert!(read_limited(&b"12345"[..], 4).is_err());
        assert_eq!(read_limited(&b"1234"[..], 4).unwrap(), b"1234");
    }
    #[test]
    fn corrupt_download_never_passes_verification() {
        let path = std::env::temp_dir().join(format!("pcsweep-update-test-{}", unique()));
        let mut bytes = vec![0u8; 4096];
        bytes[..2].copy_from_slice(b"MZ");
        fs::write(&path, &bytes).unwrap();
        let hash = format!("{:x}", Sha256::digest(&bytes));
        assert!(verify_executable(&path, 4096, &hash).is_ok());
        fs::write(&path, b"corrupt").unwrap();
        assert!(verify_executable(&path, 4096, &hash).is_err());
        let _ = fs::remove_file(path);
    }

    fn mock_metadata(manifest: &Manifest) -> (Vec<u8>, Vec<u8>) {
        let metadata = serde_json::to_vec(manifest).unwrap();
        let release = serde_json::json!({
            "tag_name": "v1.2.0+build.42", "html_url": "https://github.com/kashrtx/pcsweep/releases/tag/v1.2.0+build.42",
            "body": "Release notes", "draft": false, "prerelease": false,
            "assets": [
                { "name": "update.json", "size": metadata.len(),
                  "browser_download_url": "https://github.com/kashrtx/pcsweep/releases/download/v1.2.0+build.42/update.json",
                  "digest": format!("sha256:{:x}", Sha256::digest(&metadata)) },
                { "name": "PCSweep.exe", "size": manifest.size,
                  "browser_download_url": "https://github.com/kashrtx/pcsweep/releases/download/v1.2.0+build.42/PCSweep.exe",
                  "digest": format!("sha256:{}", manifest.sha256) }
            ]
        });
        (serde_json::to_vec(&release).unwrap(), metadata)
    }

    #[test]
    fn mocked_release_check_detects_same_version_update_and_current_build() {
        let m = manifest();
        let (release, metadata) = mock_metadata(&m);
        let fetch = |url: &str, _| {
            Ok(if url == API {
                release.clone()
            } else {
                metadata.clone()
            })
        };
        let found = check_with_fetch(fetch, "1.2.0", 41, "older")
            .unwrap()
            .unwrap();
        assert_eq!(found.manifest.revision, 42);
        assert!(check_with_fetch(fetch, "1.2.0", 42, &m.commit)
            .unwrap()
            .is_none());
    }

    #[test]
    fn mocked_corrupt_metadata_is_refused_before_download() {
        let (release, mut metadata) = mock_metadata(&manifest());
        metadata.push(b' ');
        let result = check_with_fetch(
            |url, _| {
                Ok(if url == API {
                    release.clone()
                } else {
                    metadata.clone()
                })
            },
            "1.2.0",
            41,
            "older",
        );
        assert!(result.is_err());
    }

    #[test]
    fn helper_layout_rejects_unrelated_backup_or_staging_locations() {
        let root = Path::new(r"C:\Users\Me\AppData\Local\PCSweep\updates");
        let job = root.join("job-1-2");
        let helper = job.join("PCSweep-updater.exe");
        let mut request = InstallRequest {
            target: PathBuf::from(r"C:\Tools\PCSweep.exe"),
            staged: PathBuf::from(r"C:\Tools\.pcsweep-update-1-2.exe"),
            backup: PathBuf::from(r"C:\Tools\.pcsweep-backup-1-2.exe"),
            size: 4096,
            sha256: "a".repeat(64),
            old_sha256: "b".repeat(64),
            parent_pid: 10,
            parent_created: 20,
            token: "1-2".into(),
        };
        assert!(validate_request_layout(&request, &job, &helper, root).is_ok());
        request.backup = PathBuf::from(r"C:\Windows\System32\important.exe");
        assert!(validate_request_layout(&request, &job, &helper, root).is_err());
        request.backup = PathBuf::from(r"C:\Tools\.pcsweep-backup-1-2.exe");
        request.staged = PathBuf::from(r"C:\Elsewhere\.pcsweep-update-1-2.exe");
        assert!(validate_request_layout(&request, &job, &helper, root).is_err());
    }

    #[test]
    fn interrupted_handoff_restarts_only_the_unchanged_verified_original() {
        let folder = std::env::temp_dir().join(format!("pcsweep-recovery-{}", unique()));
        fs::create_dir(&folder).unwrap();
        let target = folder.join("PCSweep.exe");
        fs::write(&target, b"trusted original").unwrap();
        let request = InstallRequest {
            target: target.clone(),
            staged: folder.join("missing-stage.exe"),
            backup: folder.join("backup.exe"),
            size: 4096,
            sha256: "a".repeat(64),
            old_sha256: sha256(&target).unwrap(),
            parent_pid: 1,
            parent_created: 1,
            token: "1-2".into(),
        };
        let mut launched = false;
        restart_original_if_verified(&request, |path| {
            launched = true;
            assert_eq!(path, target);
            Ok(())
        })
        .unwrap();
        assert!(launched);
        fs::write(&target, b"changed after handoff").unwrap();
        assert!(restart_original_if_verified(&request, |_| panic!(
            "must not launch changed executable"
        ))
        .is_err());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn temporary_file_replacement_and_rollback_preserve_original_bytes() {
        let folder = std::env::temp_dir().join(format!("pcsweep-update-files-{}", unique()));
        fs::create_dir(&folder).unwrap();
        let target = folder.join("old.exe");
        let staged = folder.join("staged.exe");
        let backup = folder.join("backup.exe");
        fs::write(&target, b"original app").unwrap();
        fs::write(&staged, b"new app").unwrap();
        replace_with_backup(&target, &staged, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new app");
        assert_eq!(fs::read(&backup).unwrap(), b"original app");
        restore_backup(&target, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"original app");
        assert!(!backup.exists());
        assert!(replace_with_backup(&target, &staged, &backup).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original app");
        fs::remove_dir_all(folder).unwrap();
    }
}
