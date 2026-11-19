use crate::running::{self, Identity, Snapshot};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use sysinfo::{DiskKind, Disks, System};

pub struct Drive {
    pub mount: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
    pub kind: &'static str,
    pub removable: bool,
}

impl Drive {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
    pub fn used_frac(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f32 / self.total as f32
        }
    }
    pub fn letter(&self) -> String {
        self.mount.trim_end_matches('\\').to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryPressure {
    Normal,
    Elevated,
    Critical,
}

/// Cache is deliberately absent: standby cache is already available to Windows.
pub fn memory_pressure(total: u64, available: u64, commit: u64, limit: u64) -> MemoryPressure {
    let available_fraction = if total > 0 {
        available as f64 / total as f64
    } else {
        1.0
    };
    let commit_fraction = if limit > 0 {
        commit as f64 / limit as f64
    } else {
        0.0
    };
    if (total > 0 && available_fraction < 0.03) || commit_fraction >= 0.97 {
        MemoryPressure::Critical
    } else if (total > 0 && available_fraction < 0.10) || commit_fraction >= 0.85 {
        MemoryPressure::Elevated
    } else {
        MemoryPressure::Normal
    }
}

/// Runs a slow OS query on its own thread. Requests coalesce and the UI only polls
/// for a finished result, so a sleeping disk or a busy process table can't stall a frame.
struct Worker<T> {
    request: mpsc::Sender<()>,
    results: mpsc::Receiver<T>,
    pending: bool,
}

impl<T: Send + 'static> Worker<T> {
    fn spawn(name: &str, mut job: impl FnMut() -> T + Send + 'static) -> Option<Self> {
        let (request, requests) = mpsc::channel::<()>();
        let (sender, results) = mpsc::channel();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                while requests.recv().is_ok() {
                    while requests.try_recv().is_ok() {}
                    if sender.send(job()).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            request,
            results,
            pending: false,
        })
    }

    /// Asks for a fresh result. Returns false if the worker has stopped.
    fn request(&mut self) -> bool {
        if !self.pending {
            self.pending = self.request.send(()).is_ok();
        }
        self.pending
    }

    /// The newest finished result, if one arrived since the last poll.
    fn latest(&mut self) -> Option<T> {
        let mut latest = None;
        loop {
            match self.results.try_recv() {
                Ok(value) => latest = Some(value),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = false;
                    break;
                }
            }
        }
        if latest.is_some() {
            self.pending = false;
        }
        latest
    }
}

/// Drive discovery can block on sleeping or unreachable drives; it runs on a worker.
fn read_drives() -> Vec<Drive> {
    let disks = Disks::new_with_refreshed_list();
    let mut v: Vec<Drive> = disks
        .list()
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| Drive {
            mount: d.mount_point().to_string_lossy().to_string(),
            label: d.name().to_string_lossy().to_string(),
            total: d.total_space(),
            free: d.available_space(),
            kind: match d.kind() {
                DiskKind::SSD => "SSD",
                DiskKind::HDD => "HDD",
                _ => "Drive",
            },
            removable: d.is_removable(),
        })
        .collect();
    v.sort_by(|a, b| a.mount.cmp(&b.mount));
    v.dedup_by(|a, b| a.mount == b.mount);
    v
}

pub struct SysMon {
    sys: System,
    last_fast: Option<Instant>,
    last_disks: Option<Instant>,
    last_procs: Option<Instant>,
    pub processes: Snapshot,
    pub process_warning: Option<String>,
    proc_worker: Option<Worker<Result<Snapshot, String>>>,
    drive_worker: Option<Worker<Vec<Drive>>>,
    pub drives: Vec<Drive>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_avail: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub commit_total: u64,
    pub commit_limit: u64,
    pub commit_peak: u64,
    pub system_cache: u64,
    pub memory_info_available: bool,
    pub cpu: f32,
    pub host: String,
    pub os: String,
    pub cpu_name: String,
    pub cores: usize,
}

fn process_worker(cores: usize) -> Option<Worker<Result<Snapshot, String>>> {
    let mut sampler = running::Sampler::default();
    Worker::spawn("pcsweep-processes", move || sampler.sample(cores))
}

fn due(t: &Option<Instant>, d: Duration) -> bool {
    t.map_or(true, |t| t.elapsed() >= d)
}

impl SysMon {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu();
        sys.refresh_memory();
        let cpu_name = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .unwrap_or_default();
        let cores = sys.cpus().len().max(1);
        let proc_worker = process_worker(cores);
        let drive_worker = Worker::spawn("pcsweep-drives", read_drives);
        let mut s = Self {
            sys,
            last_fast: None,
            last_disks: None,
            last_procs: None,
            processes: Snapshot::default(),
            process_warning: None,
            proc_worker,
            drive_worker,
            drives: Vec::new(),
            mem_total: 0,
            mem_used: 0,
            mem_avail: 0,
            swap_total: 0,
            swap_used: 0,
            commit_total: 0,
            commit_limit: 0,
            commit_peak: 0,
            system_cache: 0,
            memory_info_available: false,
            cpu: 0.0,
            host: System::host_name().unwrap_or_else(|| "This PC".into()),
            os: System::long_os_version().unwrap_or_else(|| "Windows".into()),
            cpu_name,
            cores,
        };
        s.tick(true);
        s
    }

    pub fn tick(&mut self, want_procs: bool) {
        if due(&self.last_fast, Duration::from_millis(1000)) {
            self.sys.refresh_cpu();
            self.sys.refresh_memory();
            self.cpu = self.sys.global_cpu_info().cpu_usage();
            self.mem_total = self.sys.total_memory();
            self.mem_used = self.sys.used_memory();
            self.mem_avail = self.sys.available_memory();
            self.swap_total = self.sys.total_swap();
            self.swap_used = self.sys.used_swap();
            self.refresh_memory_counters();
            self.last_fast = Some(Instant::now());
        }
        if let Some(drives) = self.drive_worker.as_mut().and_then(|w| w.latest()) {
            self.drives = drives;
        }
        if due(&self.last_disks, Duration::from_secs(5)) {
            self.last_disks = Some(Instant::now());
            if !self.drive_worker.as_mut().is_some_and(|w| w.request()) {
                // Never move a slow query back to the UI when a worker stops.
                // Retry on the next scheduled refresh while retaining the last
                // completed snapshot.
                self.drive_worker = Worker::spawn("pcsweep-drives", read_drives);
                if let Some(worker) = &mut self.drive_worker {
                    worker.request();
                }
            }
        }
        if let Some(snapshot) = self.proc_worker.as_mut().and_then(|w| w.latest()) {
            match snapshot {
                Ok(snapshot) => {
                    self.processes = snapshot;
                    self.process_warning = None;
                }
                Err(error) => self.process_warning = Some(error),
            }
        }
        if want_procs && due(&self.last_procs, Duration::from_secs(3)) {
            self.last_procs = Some(Instant::now());
            if !self.proc_worker.as_mut().is_some_and(|w| w.request()) {
                self.process_warning = Some(
                    "The process sampler is restarting. Displayed app values may be out of date."
                        .into(),
                );
                self.proc_worker = process_worker(self.cores);
                if let Some(worker) = &mut self.proc_worker {
                    worker.request();
                }
            }
        }
    }

    fn refresh_memory_counters(&mut self) {
        use windows_sys::Win32::System::ProcessStatus::{
            GetPerformanceInfo, PERFORMANCE_INFORMATION,
        };
        let mut info: PERFORMANCE_INFORMATION = unsafe { std::mem::zeroed() };
        info.cb = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
        self.memory_info_available = unsafe { GetPerformanceInfo(&mut info, info.cb) } != 0;
        if self.memory_info_available {
            let page = info.PageSize as u64;
            self.commit_total = (info.CommitTotal as u64).saturating_mul(page);
            self.commit_limit = (info.CommitLimit as u64).saturating_mul(page);
            self.commit_peak = (info.CommitPeak as u64).saturating_mul(page);
            self.system_cache = (info.SystemCache as u64).saturating_mul(page);
        }
    }

    pub fn refresh_processes_soon(&mut self) {
        self.last_procs = None;
    }

    #[cfg(test)]
    pub(crate) fn preview_fixture() -> Self {
        let gb = 1024u64.pow(3);
        let mb = 1024u64.pow(2);
        Self {
            sys: System::new(),
            last_fast: Some(Instant::now()),
            last_disks: Some(Instant::now()),
            last_procs: Some(Instant::now()),
            processes: running::preview_fixture(mb),
            process_warning: None,
            proc_worker: None,
            drive_worker: None,
            drives: vec![Drive {
                mount: r"C:\".into(),
                label: "Windows".into(),
                total: 512 * gb,
                free: 140 * gb,
                kind: "SSD",
                removable: false,
            }],
            mem_total: 16 * gb,
            mem_used: 11 * gb,
            mem_avail: 5 * gb,
            swap_total: 8 * gb,
            swap_used: 2 * gb,
            commit_total: 15 * gb,
            commit_limit: 24 * gb,
            commit_peak: 19 * gb,
            system_cache: 4 * gb,
            memory_info_available: true,
            cpu: 18.0,
            host: "Preview PC".into(),
            os: "Windows 11".into(),
            cpu_name: "8-core processor".into(),
            cores: 8,
        }
    }

    /// Requests fresh drive totals on the next frame without blocking this one.
    pub fn refresh_drives(&mut self) {
        self.last_disks = None;
    }

    pub fn system_drive(&self) -> Option<&Drive> {
        let sd = std::env::var("SystemDrive")
            .unwrap_or_else(|_| "C:".into())
            .to_uppercase();
        self.drives
            .iter()
            .find(|d| d.mount.to_uppercase().starts_with(&sd))
    }

    pub fn mem_frac(&self) -> f32 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f32 / self.mem_total as f32
        }
    }
}

pub fn uptime_secs() -> u64 {
    System::uptime()
}

pub fn is_admin() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

/// (bytes, items) currently in the Recycle Bin across all drives.
pub fn recycle_bin_info() -> (u64, u64) {
    use windows_sys::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    unsafe {
        let mut info: SHQUERYRBINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
        let hr = SHQueryRecycleBinW(std::ptr::null(), &mut info);
        if hr == 0 {
            let size = { info.i64Size };
            let items = { info.i64NumItems };
            (size.max(0) as u64, items.max(0) as u64)
        } else {
            (0, 0)
        }
    }
}

pub fn empty_recycle_bin() -> bool {
    use windows_sys::Win32::UI::Shell::{
        SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND,
    };
    unsafe {
        let hr = SHEmptyRecycleBinW(
            0,
            std::ptr::null(),
            SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND,
        );
        hr == 0
    }
}

/// Starts a new elevated copy of PCSweep (UAC prompt). Returns true if it launched.
pub fn relaunch_as_admin() -> bool {
    use crate::{platform, winfile};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        UI::{
            Shell::{
                ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    };
    let (Ok(exe), Ok(system)) = (std::env::current_exe(), platform::system_directory()) else {
        return false;
    };
    let verb = winfile::wide("runas");
    let exe = winfile::wide(&exe);
    let cwd = winfile::wide(&system);
    let args = winfile::wide(format!("--wait-for-parent {}", std::process::id()));
    let mut launch = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: exe.as_ptr(),
        lpParameters: args.as_ptr(),
        lpDirectory: cwd.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..unsafe { std::mem::zeroed() }
    };
    let ok = unsafe { ShellExecuteExW(&mut launch) } != 0;
    if launch.hProcess != 0 {
        unsafe {
            CloseHandle(launch.hProcess);
        }
    }
    ok
}

/// Original Windows policy, retained only for this exact process lifetime.
#[derive(Clone, Copy)]
pub struct PowerPolicy {
    pub control: u32,
    pub state: u32,
}

fn app_process(id: Identity, rights: u32) -> Result<running::Handle, String> {
    running::checked_handle(id, rights)?.ok_or_else(|| "That process has already exited.".into())
}

fn ensure_background(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId,
    };
    let mut foreground_pid = 0;
    unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), &mut foreground_pid);
    }
    if foreground_pid == pid {
        Err("The app is in the foreground. Switch to PCSweep before changing it.".into())
    } else {
        Ok(())
    }
}

fn resident_memory(handle: &running::Handle) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    (unsafe { GetProcessMemoryInfo(handle.0, &mut memory, memory.cb) } != 0)
        .then_some(memory.WorkingSetSize as u64)
}

pub fn trim_app_memory(id: Identity) -> Result<(u64, u64), String> {
    use windows_sys::Win32::System::{
        ProcessStatus::EmptyWorkingSet, Threading::PROCESS_SET_QUOTA,
    };
    let handle = app_process(id, PROCESS_SET_QUOTA)?;
    ensure_background(id.pid)?;
    let before =
        resident_memory(&handle).ok_or("The app's resident memory could not be measured.")?;
    if unsafe { EmptyWorkingSet(handle.0) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let after = resident_memory(&handle).unwrap_or(before);
    Ok((before, after))
}

pub fn enable_app_efficiency(id: Identity) -> Result<PowerPolicy, String> {
    use windows_sys::Win32::System::Threading::{
        GetProcessInformation, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_SET_INFORMATION,
    };
    let handle = app_process(id, PROCESS_SET_INFORMATION)?;
    ensure_background(id.pid)?;
    let mut policy = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    let bytes = std::mem::size_of_val(&policy) as u32;
    if unsafe {
        GetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            &mut policy as *mut _ as _,
            bytes,
        )
    } == 0
    {
        return Err("Windows could not read this app's efficiency policy. It may be protected or unsupported.".into());
    }
    let original = PowerPolicy {
        control: policy.ControlMask,
        state: policy.StateMask,
    };
    policy.ControlMask |= PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
    policy.StateMask |= PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
    if unsafe {
        SetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            &policy as *const _ as _,
            bytes,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(original)
}

pub fn restore_app_efficiency(id: Identity, original: PowerPolicy) -> Result<(), String> {
    use windows_sys::Win32::System::Threading::{
        GetProcessInformation, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_SET_INFORMATION,
    };
    let handle = app_process(id, PROCESS_SET_INFORMATION)?;
    let mut policy = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    let bytes = std::mem::size_of_val(&policy) as u32;
    if unsafe {
        GetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            &mut policy as *mut _ as _,
            bytes,
        )
    } == 0
    {
        return Err("Windows could not read this app's current efficiency policy.".into());
    }
    // Undo only the execution-speed bit that PCSweep changed. Preserve timer and
    // any other policy flags changed by the application since it was enabled.
    policy.ControlMask = (policy.ControlMask & !PROCESS_POWER_THROTTLING_EXECUTION_SPEED)
        | (original.control & PROCESS_POWER_THROTTLING_EXECUTION_SPEED);
    policy.StateMask = (policy.StateMask & !PROCESS_POWER_THROTTLING_EXECUTION_SPEED)
        | (original.state & PROCESS_POWER_THROTTLING_EXECUTION_SPEED);
    if unsafe {
        SetProcessInformation(
            handle.0,
            ProcessPowerThrottling,
            &policy as *const _ as _,
            bytes,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_worker_requests_coalesce_without_waiting_for_results() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (release, wait) = mpsc::channel();
        let (started, start) = mpsc::channel();
        let mut worker = Worker::spawn("pcsweep-test-query", move || {
            observed.fetch_add(1, Ordering::SeqCst);
            started.send(()).unwrap();
            wait.recv().unwrap();
            42u32
        })
        .unwrap();
        assert!(worker.request());
        start.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(worker.request());
        assert!(worker.latest().is_none());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = worker.latest() {
                assert_eq!(value, 42);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!worker.pending);
    }

    #[test]
    fn pressure_uses_available_and_commit_instead_of_cache_totals() {
        assert_eq!(
            memory_pressure(1000, 400, 1000, 2000),
            MemoryPressure::Normal
        );
        assert_eq!(
            memory_pressure(1000, 80, 1000, 2000),
            MemoryPressure::Elevated
        );
        assert_eq!(
            memory_pressure(1000, 20, 1000, 2000),
            MemoryPressure::Critical
        );
        assert_eq!(
            memory_pressure(1000, 500, 1950, 2000),
            MemoryPressure::Critical
        );
        assert_eq!(memory_pressure(0, 0, 0, 0), MemoryPressure::Normal);
    }
}
