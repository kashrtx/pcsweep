//! The portable updater's local transaction. This loop also runs in a GUI-free
//! helper after the app has exited. It uses the caller's existing rights.
use crate::{platform, running, system, winfile};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{LocalFree, WAIT_OBJECT_0};
use windows_sys::Win32::Security::{
    Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    },
    Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG},
    GetKernelObjectSecurity, SetKernelObjectSecurity, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, GetVolumeInformationByHandleW, DELETE, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
    FILE_SHARE_WRITE, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

const HELPER: &str = "PCSweep-updater.exe";
const STAGED: &str = "download.exe";
const BACKUP: &str = "previous.exe";
const REQUEST: &str = "request.json";
const STARTED: &str = "started";
const READY: &str = "ready";
const PREFIX: &str = ".pcsweep-update-";
const REQUEST_LIMIT: u64 = 32 * 1024;
const JOB_OWNER: &str = "O:BAD:P";
const JOB_ACES: &str = "(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
const FILE_PERSISTENT_ACLS: u32 = 0x8;

struct RunningApp {
    path: PathBuf,
    file: File,
    directories: Vec<File>,
}

static RUNNING: OnceLock<RunningApp> = OnceLock::new();

pub fn pin_running_app() {
    let _ = (|| -> io::Result<()> {
        let path = std::env::current_exe()?;
        let dirs = winfile::lock_directories(
            path.parent()
                .ok_or_else(|| io::Error::other("App folder missing"))?,
        )?;
        let file = winfile::read_locked(&path, 0)?;
        let _ = RUNNING.set(RunningApp {
            path,
            file,
            directories: dirs,
        });
        Ok(())
    })();
}

pub(crate) fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    let result = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if result != 0 {
        return Err("Windows could not create a secure update token.".into());
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn create_job(path: &Path, parent: &File, elevated: bool) -> io::Result<()> {
    if !elevated {
        return fs::create_dir(path);
    }
    let mut flags = 0u32;
    if unsafe {
        GetVolumeInformationByHandleW(
            winfile::handle(parent),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut flags,
            std::ptr::null_mut(),
            0,
        )
    } == 0
        || flags & FILE_PERSISTENT_ACLS == 0
    {
        return Err(io::Error::other(
            "Administrator updates require a drive that supports Windows permissions.",
        ));
    }
    // Build the protected directory in one call. An elevated copy is never
    // placed in a user-writable update cache before launch.
    let acl = winfile::wide(format!("{JOB_OWNER}{JOB_ACES}S:(ML;;NW;;;HI)"));
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            acl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let created = unsafe { CreateDirectoryW(winfile::wide(path).as_ptr(), &attributes) };
    let result = if created == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    unsafe {
        LocalFree(descriptor.cast());
    }
    result
}

fn lock_job(path: &Path) -> io::Result<Vec<File>> {
    let directories = winfile::lock_directories(path)?;
    if system::is_admin() {
        // Parent write access can allow directory deletion. Check the protected
        // owner and ACL on the pinned directory before trusting any contents.
        let file = winfile::open(
            path,
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )?;
        let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        let mut security = security_descriptor(&file, info)?;
        let mut value = std::ptr::null_mut();
        let mut len = 0u32;
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                security.as_mut_ptr().cast(),
                SDDL_REVISION_1,
                info,
                &mut value,
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let actual =
            unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(value, len as usize)) };
        unsafe {
            LocalFree(value.cast());
        }
        let protected = actual.trim_end_matches('\0').strip_prefix(JOB_OWNER);
        if protected.map(|acl| acl.trim_start_matches("AI").trim_start_matches("AR"))
            != Some(JOB_ACES)
        {
            return Err(io::Error::other("The update folder permissions changed."));
        }
    }
    Ok(directories)
}

fn read_small(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    winfile::read_locked(path, 0)?
        .take(limit + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(io::Error::other("Invalid update handoff size."));
    }
    Ok(data)
}

fn write_new(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

fn sha256(file: &mut File) -> io::Result<String> {
    file.rewind()?;
    let mut hash = Sha256::new();
    io::copy(file, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

#[derive(Deserialize, Serialize)]
struct Request {
    target: PathBuf,
    original: winfile::Stamp,
    old_hash: String,
    hash: String,
    size: u64,
    parent: running::Identity,
    token: String,
    elevated: bool,
}

pub struct Job {
    path: PathBuf,
    token: String,
    _dirs: Vec<File>,
}

impl Job {
    pub fn prepare() -> Result<Self, String> {
        let current = RUNNING.get().ok_or(
            "This copy could not be secured for in-place updates. Use a verified download or run PCSweep from a regular local folder.",
        )?;
        let dir = current.path.parent().ok_or("App folder missing.")?;
        let token = nonce()?;
        let path = dir.join(format!("{PREFIX}{token}"));
        create_job(
            &path,
            current.directories.last().ok_or("App folder missing.")?,
            system::is_admin(),
        )
        .map_err(|e| format!("Could not prepare an update next to PCSweep: {e}"))?;
        let dirs =
            lock_job(&path).map_err(|e| format!("Could not secure the update folder: {e}"))?;
        Ok(Self {
            path,
            token,
            _dirs: dirs,
        })
    }

    pub fn download_path(&self) -> PathBuf {
        self.path.join(STAGED)
    }

    pub fn cancel(self) {
        let path = self.path.clone();
        for name in [STAGED, HELPER, REQUEST, READY] {
            let _ = fs::remove_file(path.join(name));
        }
        drop(self);
        let _ = fs::remove_dir(&path);
    }

    pub fn handoff(&self, size: u64, hash: String) -> Result<(), String> {
        self.send_to_helper(size, hash).map_err(|e| e.to_string())
    }

    fn send_to_helper(&self, size: u64, hash: String) -> io::Result<()> {
        let current = RUNNING
            .get()
            .ok_or_else(|| io::Error::other("App identity unavailable."))?;
        let mut source = current.file.try_clone()?;
        let original = winfile::stamp(&source)?;
        let helper = self.path.join(HELPER);
        let mut copy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&helper)?;
        source.rewind()?;
        io::copy(&mut source, &mut copy)?;
        copy.sync_all()?;
        drop(copy);
        let mut executable = winfile::read_locked(&helper, 0)?;
        let old_hash = sha256(&mut source)?;
        if sha256(&mut executable)? != old_hash {
            return Err(io::Error::other("The update helper failed verification."));
        }
        let parent = running::Handle::open(std::process::id(), 0)?;
        let created = parent.times()?.0;
        let request = Request {
            target: current.path.clone(),
            original,
            old_hash,
            hash,
            size,
            elevated: system::is_admin(),
            parent: running::Identity {
                pid: std::process::id(),
                created,
            },
            token: self.token.clone(),
        };
        let data = serde_json::to_vec(&request)?;
        write_new(&self.path.join(REQUEST), &data)?;
        let digest = format!("{:x}", Sha256::digest(&data));
        let cwd = platform::system_directory().map_err(io::Error::other)?;
        let mut child = Command::new(helper)
            .current_dir(cwd)
            .args(["--pcsweep-update-helper", &digest])
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if read_small(&self.path.join(READY), 32).is_ok_and(|b| b == self.token.as_bytes()) {
                return Ok(());
            }
            if !matches!(child.try_wait(), Ok(None)) || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other(
                    "The update helper did not prepare the restart. Your current app is unchanged.",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn validate_layout(request: &Request, helper: &Path, job: &Path) -> io::Result<()> {
    let same = request
        .target
        .parent()
        .zip(job.parent())
        .is_some_and(|(a, b)| winfile::same_path(a, b));
    if helper.file_name().is_none_or(|s| s != HELPER)
        || !crate::updates::valid_hex(&request.token, 32)
        || !crate::updates::valid_hex(&request.old_hash, 64)
        || !crate::updates::valid_hex(&request.hash, 64)
        || job
            .file_name()
            .is_none_or(|s| s != format!("{PREFIX}{}", request.token).as_str())
        || !same
        || request
            .target
            .extension()
            .is_none_or(|s| !s.eq_ignore_ascii_case("exe"))
        || !(4096..=crate::updates::MAX_EXE).contains(&request.size)
        || request.elevated != system::is_admin()
    {
        return Err(io::Error::other(
            "Invalid update identity, location or permissions.",
        ));
    }
    Ok(())
}

fn security_descriptor(file: &File, information: u32) -> io::Result<Vec<usize>> {
    let mut len = 0u32;
    unsafe {
        GetKernelObjectSecurity(
            winfile::handle(file),
            information,
            std::ptr::null_mut(),
            0,
            &mut len,
        );
    }
    if len == 0 || len > 1_048_576 {
        return Err(io::Error::other(
            "The original app's permissions could not be read.",
        ));
    }
    let mut descriptor = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetKernelObjectSecurity(
            winfile::handle(file),
            information,
            descriptor.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(descriptor)
}

fn apply_dacl(file: &File, mut descriptor: Vec<usize>) -> io::Result<()> {
    if unsafe {
        SetKernelObjectSecurity(
            winfile::handle(file),
            DACL_SECURITY_INFORMATION,
            descriptor.as_mut_ptr().cast(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn verified_original(request: &Request, rights: u32) -> io::Result<File> {
    let mut original = winfile::read_locked(&request.target, rights)?;
    if winfile::stamp(&original)? != request.original || sha256(&mut original)? != request.old_hash
    {
        return Err(io::Error::other(
            "The installed executable changed. It will not be restarted or replaced.",
        ));
    }
    Ok(original)
}

fn verify_stage(file: &mut File, request: &Request) -> io::Result<()> {
    crate::updates::verify_executable(file, request.size, &request.hash).map_err(io::Error::other)
}

fn restore(
    request: &Request,
    original: &File,
    new_file: winfile::Stamp,
    job: &Path,
) -> io::Result<()> {
    // Move only the verified replacement aside. Preserve the backup if an
    // unrelated file now occupies the destination.
    let installed = winfile::read_locked(&request.target, DELETE)?;
    if winfile::stamp(&installed)? != new_file {
        return Err(io::Error::other(
            "A different executable occupies the app path.",
        ));
    }
    winfile::rename_locked(&installed, &job.join("failed.exe"))?;
    winfile::rename_locked(original, &request.target)
}

fn start_updated(
    request: &Request,
    job: &Path,
    stamp: winfile::Stamp,
    permissions: Vec<usize>,
) -> io::Result<()> {
    let mut updated = winfile::read_locked(&request.target, WRITE_DAC)?;
    if winfile::stamp(&updated)? != stamp {
        return Err(io::Error::other(
            "The replacement executable changed before launch.",
        ));
    }
    verify_stage(&mut updated, request)?;
    apply_dacl(&updated, permissions)?;
    let ack = job.join(STARTED);
    let mut child = Command::new(&request.target)
        .current_dir(platform::system_directory().map_err(io::Error::other)?)
        .arg("--pcsweep-update-ack")
        .arg(&ack)
        .arg(&request.token)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if read_small(&ack, 32).is_ok_and(|data| data == request.token.as_bytes()) {
            return Ok(());
        }
        if child.try_wait().ok().flatten().is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("The new version did not finish opening."));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn replace(
    request: &Request,
    job: &Path,
    start: impl FnOnce(&Request, &Path, winfile::Stamp, Vec<usize>) -> io::Result<()>,
) -> io::Result<()> {
    let original = verified_original(request, DELETE | READ_CONTROL)?;
    let permissions = security_descriptor(&original, DACL_SECURITY_INFORMATION)?;
    let mut staged = winfile::read_locked(&job.join(STAGED), DELETE)?;
    verify_stage(&mut staged, request)?;
    let stamp = winfile::stamp(&staged)?;
    let backup = job.join(BACKUP);
    winfile::rename_locked(&original, &backup)?;
    if let Err(error) = winfile::rename_locked(&staged, &request.target) {
        winfile::rename_locked(&original, &request.target)?;
        return Err(error);
    }
    drop(staged);
    if let Err(error) = start(request, job, stamp, permissions) {
        restore(request, &original, stamp, job)?;
        return Err(error);
    }
    drop(original);
    let _ = fs::remove_file(&backup);
    Ok(())
}

fn install(expected_digest: &str) -> io::Result<()> {
    let helper = std::env::current_exe()?;
    let job = helper
        .parent()
        .ok_or_else(|| io::Error::other("Missing update folder."))?;
    let _dirs = lock_job(job)?;
    let data = read_small(&job.join(REQUEST), REQUEST_LIMIT)?;
    if format!("{:x}", Sha256::digest(&data)) != expected_digest {
        return Err(io::Error::other("The update handoff changed."));
    }
    let request: Request = serde_json::from_slice(&data)?;
    validate_layout(&request, &helper, job)?;
    let parent = running::Handle::open(request.parent.pid, SYNCHRONIZE)?;
    if parent.times()?.0 != request.parent.created
        || !parent
            .image(&mut [0; 32_768])
            .is_some_and(|image| winfile::same_path(&image, &request.target))
    {
        return Err(io::Error::other("The running PCSweep process changed."));
    }
    verified_original(&request, 0)?;
    verify_stage(&mut winfile::read_locked(&job.join(STAGED), 0)?, &request)?;
    write_new(&job.join(READY), request.token.as_bytes())?;
    if unsafe { WaitForSingleObject(parent.0, 90_000) } != WAIT_OBJECT_0 {
        return Err(io::Error::other("PCSweep did not close in time."));
    }

    if let Err(error) = replace(&request, job, start_updated) {
        // Never launch an unrelated executable or skip verification on rollback.
        if let Ok(_original) = verified_original(&request, 0) {
            let _ = Command::new(&request.target)
                .current_dir(platform::system_directory().map_err(io::Error::other)?)
                .arg("--pcsweep-update-failed")
                .spawn();
        } else if job.join(BACKUP).is_file() {
            crate::instance::show_error(&format!(
                "PCSweep could not restore the previous executable. Its backup is at:\n{}\n\n{error}",
                job.join(BACKUP).display()
            ));
        }
        return Err(error);
    }
    for name in [STARTED, READY, REQUEST] {
        let _ = fs::remove_file(job.join(name));
    }
    Ok(())
}

pub fn run_helper_mode() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|a| a != "--pcsweep-update-helper") {
        return None;
    }
    let result = if args.len() == 3 {
        install(&args[2].to_string_lossy())
    } else {
        Err(io::Error::other("Invalid updater arguments."))
    };
    match result {
        Ok(()) => Some(0),
        Err(error) => {
            crate::util::log(format!("App update failed: {error}"));
            Some(1)
        }
    }
}

pub fn acknowledge(path: &Path, token: &str) -> bool {
    let Some(job) = path.parent() else {
        return false;
    };
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    if !crate::updates::valid_hex(token, 32)
        || path.file_name().is_none_or(|s| s != STARTED)
        || job
            .file_name()
            .is_none_or(|s| s != format!("{PREFIX}{token}").as_str())
        || !job
            .parent()
            .zip(exe.parent())
            .is_some_and(|(a, b)| winfile::same_path(a, b))
    {
        return false;
    }
    lock_job(job).is_ok_and(|_dirs| {
        read_small(&job.join(READY), 32).is_ok_and(|data| data == token.as_bytes())
            && write_new(path, token.as_bytes()).is_ok()
    })
}

pub fn clean_confirmed_job(ack: &Path) {
    let Some(path) = ack.parent() else {
        return;
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    if let Ok(_dirs) = lock_job(path) {
        while Instant::now() < deadline {
            // No recursive delete. Failed installs retain their backup.
            if !path.join(REQUEST).exists()
                && !path.join(BACKUP).exists()
                && fs::remove_file(path.join(HELPER)).is_ok()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    // Windows releases the directory lock at the end of the transaction.
    let _ = fs::remove_dir(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        parent: PathBuf,
        job: PathBuf,
        request: Request,
    }

    impl Fixture {
        fn create() -> Self {
            let token = nonce().unwrap();
            let parent = std::env::temp_dir().join(format!("pcsweep-install-test-{token}"));
            fs::create_dir(&parent).unwrap();
            let target = parent.join("PCSweep under test.exe");
            let job = parent.join(format!("{PREFIX}{token}"));
            let folders = winfile::lock_directories(&parent).unwrap();
            create_job(&job, folders.last().unwrap(), system::is_admin()).unwrap();
            let _job_guard = lock_job(&job).unwrap();
            let old = crate::updates::test_executable(1);
            let new = crate::updates::test_executable(2);
            fs::write(&target, &old).unwrap();
            fs::write(job.join(STAGED), &new).unwrap();
            let original = winfile::stamp(&winfile::read_locked(&target, 0).unwrap()).unwrap();
            let request = Request {
                target,
                original,
                old_hash: format!("{:x}", Sha256::digest(&old)),
                hash: format!("{:x}", Sha256::digest(&new)),
                size: new.len() as u64,
                parent: running::Identity::default(),
                token,
                elevated: system::is_admin(),
            };
            Self {
                parent,
                job,
                request,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }

    #[test]
    fn local_windows_transaction_verifies_and_restores_exact_previous_file() {
        let failed = Fixture::create();
        let error = replace(
            &failed.request,
            &failed.job,
            |request, folder, stamp, dacl| {
                let mut new = winfile::read_locked(&request.target, WRITE_DAC)?;
                assert_eq!(winfile::stamp(&new)?, stamp);
                verify_stage(&mut new, request)?;
                apply_dacl(&new, dacl)?;
                assert!(folder.join(BACKUP).exists());
                Err(io::Error::other("simulated missing first frame"))
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("first frame"), "{error}");
        verified_original(&failed.request, 0).unwrap();
        assert!(!failed.job.join(BACKUP).exists());

        let success = Fixture::create();
        replace(&success.request, &success.job, |request, _, _, dacl| {
            let mut executable = winfile::read_locked(&request.target, WRITE_DAC)?;
            verify_stage(&mut executable, request)?;
            apply_dacl(&executable, dacl)
        })
        .unwrap();
        assert!(!success.job.join(BACKUP).exists());
        assert_eq!(
            fs::read(&success.request.target).unwrap(),
            crate::updates::test_executable(2)
        );
    }

    #[test]
    fn link_and_changed_payload_cannot_replace_an_executable() {
        let fixture = Fixture::create();
        let stage = fixture.job.join(STAGED);
        let other = fixture.job.join("another.exe");
        fs::hard_link(&stage, &other).unwrap();
        assert!(replace(&fixture.request, &fixture.job, |_, _, _, _| panic!(
            "unsafe launch"
        ))
        .is_err());
        fs::remove_file(other).unwrap();
        fs::write(&stage, crate::updates::test_executable(5)).unwrap();
        assert!(replace(&fixture.request, &fixture.job, |_, _, _, _| panic!(
            "unsafe launch"
        ))
        .is_err());
        verified_original(&fixture.request, 0).unwrap();
        assert!(!fixture.job.join(BACKUP).exists());
    }

    #[test]
    fn rename_never_clobbers_an_unrelated_file() {
        let fixture = Fixture::create();
        let stage = fixture.job.join(STAGED);
        let source = winfile::read_locked(&stage, DELETE).unwrap();
        assert!(winfile::rename_locked(&source, &fixture.request.target).is_err());
        verified_original(&fixture.request, 0).unwrap();
    }

    #[test]
    fn elevated_helper_folder_permissions_are_verified() {
        let fixture = Fixture::create();
        println!(
            "Windows updater transaction uses administrator rights: {}",
            system::is_admin()
        );
        validate_layout(&fixture.request, &fixture.job.join(HELPER), &fixture.job).unwrap();
        if system::is_admin() {
            fs::remove_dir_all(&fixture.job).unwrap();
            fs::create_dir(&fixture.job).unwrap();
            assert!(lock_job(&fixture.job).is_err());
        }
    }
}
