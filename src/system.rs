use crate::util;
use std::collections::{HashMap, HashSet, VecDeque};
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

pub struct Proc {
    pub pid: u32,
    pub start_time: u64,
    pub name: String,
    pub mem: u64,
    pub private_bytes: Option<u64>,
    pub growth: i64,
    pub observed_secs: u64,
    pub cpu: f32,
}

#[derive(Clone)]
pub struct ProcGroup {
    pub name: String,
    pub count: usize,
    pub mem: u64,
    pub cpu: f32,
    pub private_bytes: u64,
    pub private_known: usize,
    pub growth: i64,
    pub observed_secs: u64,
    pub pids: Vec<u32>,
}

/// Processes that must never be ended from PCSweep.
pub const CRITICAL_PROCS: &[&str] = &[
    "system",
    "registry",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "services.exe",
    "lsass.exe",
    "lsaiso.exe",
    "winlogon.exe",
    "svchost.exe",
    "dwm.exe",
    "explorer.exe",
    "fontdrvhost.exe",
    "memory compression",
    "msmpeng.exe",
    "mssense.exe",
    "mpdefendercoreservice.exe",
    "securityhealthsystray.exe",
    "sihost.exe",
    "ctfmon.exe",
    "audiodg.exe",
    "system idle process",
    "secure system",
    "spoolsv.exe",
    "searchhost.exe",
    "startmenuexperiencehost.exe",
    "shellexperiencehost.exe",
    "taskhostw.exe",
    "runtimebroker.exe",
    "securityhealthservice.exe",
    "nissrv.exe",
    "wudfhost.exe",
    "conhost.exe",
    "dllhost.exe",
    "pcsweep.exe",
];

pub fn is_critical(name: &str) -> bool {
    let n = name.to_lowercase();
    CRITICAL_PROCS.iter().any(|c| *c == n)
}

fn safe_kill_target(pid: u32, expected_start: u64, actual_start: u64, name: &str) -> bool {
    pid != std::process::id()
        && expected_start != 0
        && expected_start == actual_start
        && !is_critical(name)
}

const HISTORY_SECONDS: u64 = 600;
const HISTORY_SAMPLES: usize = 201;
const HISTORY_PROCESSES: usize = 2048;

#[derive(Default)]
struct MemoryHistory {
    samples: HashMap<(u32, u64), VecDeque<(Instant, u64)>>,
}

impl MemoryHistory {
    fn observe(&mut self, processes: &mut [Proc], now: Instant) {
        let live: HashSet<_> = processes.iter().map(|p| (p.pid, p.start_time)).collect();
        self.samples.retain(|key, _| live.contains(key));
        for p in processes {
            let Some(private) = p.private_bytes else {
                continue;
            };
            let key = (p.pid, p.start_time);
            if !self.samples.contains_key(&key) && self.samples.len() >= HISTORY_PROCESSES {
                continue;
            }
            let samples = self.samples.entry(key).or_default();
            while samples
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t).as_secs() > HISTORY_SECONDS)
            {
                samples.pop_front();
            }
            samples.push_back((now, private));
            while samples.len() > HISTORY_SAMPLES {
                samples.pop_front();
            }
            if let Some((start, first)) = samples.front() {
                p.growth = (private as i128 - *first as i128)
                    .clamp(i64::MIN as i128, i64::MAX as i128) as i64;
                p.observed_secs = now.duration_since(*start).as_secs();
            }
        }
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

struct ProcSnapshot {
    procs: Vec<Proc>,
    groups: Vec<ProcGroup>,
}

fn filetime_to_unix(time: &windows_sys::Win32::Foundation::FILETIME) -> u64 {
    let ticks = ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
    (ticks / 10_000_000).saturating_sub(11_644_473_600)
}

/// Private commit (PrivateUsage) for this exact process lifetime, or None when
/// Windows can't report it. sysinfo's `virtual_memory()` silently falls back to the
/// virtual address-space size for processes it can't open, which on 64-bit Windows
/// is often terabytes; mixing the two produced impossible "-2 TB" growth.
fn private_usage(pid: u32, start: u64) -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 || start == 0 {
        return None;
    }
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw == 0 {
        return None;
    }
    let handle = ProcessHandle(raw);
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return None;
    }
    if filetime_to_unix(&created) != start {
        return None;
    }
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = size;
    let pointer = (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast();
    if unsafe { GetProcessMemoryInfo(handle.0, pointer, size) } == 0 {
        return None;
    }
    Some(counters.PrivateUsage as u64)
}

/// One process sample, used only by the background worker.
fn sample_processes(sys: &mut System, history: &mut MemoryHistory, cores: f32) -> ProcSnapshot {
    sys.refresh_processes();
    let mut procs: Vec<Proc> = sys
        .processes()
        .iter()
        .filter(|(_, p)| p.memory() > 0)
        .map(|(pid, p)| Proc {
            pid: pid.as_u32(),
            start_time: p.start_time(),
            name: p.name().to_string(),
            mem: p.memory(),
            private_bytes: private_usage(pid.as_u32(), p.start_time()),
            growth: 0,
            observed_secs: 0,
            cpu: p.cpu_usage() / cores,
        })
        .collect();
    procs.sort_unstable_by_key(|p| std::cmp::Reverse(p.mem));
    history.observe(&mut procs, Instant::now());
    let groups = group_processes(&procs);
    ProcSnapshot { procs, groups }
}

fn group_processes(procs: &[Proc]) -> Vec<ProcGroup> {
    let mut map: HashMap<String, ProcGroup> = HashMap::new();
    for p in procs {
        let g = map
            .entry(p.name.to_lowercase())
            .or_insert_with(|| ProcGroup {
                name: p.name.clone(),
                count: 0,
                mem: 0,
                cpu: 0.0,
                private_bytes: 0,
                private_known: 0,
                growth: 0,
                observed_secs: 0,
                pids: Vec::new(),
            });
        g.count += 1;
        g.mem += p.mem;
        g.cpu += p.cpu;
        if let Some(private) = p.private_bytes {
            g.private_bytes = g.private_bytes.saturating_add(private);
            g.private_known += 1;
            g.growth = g.growth.saturating_add(p.growth);
            g.observed_secs = g.observed_secs.max(p.observed_secs);
        }
        g.pids.push(p.pid);
    }
    let mut v: Vec<ProcGroup> = map.into_values().collect();
    v.sort_by(|a, b| b.mem.cmp(&a.mem));
    v
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
    proc_groups: Vec<ProcGroup>,
    pub process_warning: Option<String>,
    proc_worker: Option<Worker<ProcSnapshot>>,
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
    pub procs: Vec<Proc>,
    pub host: String,
    pub os: String,
    pub cpu_name: String,
    pub cores: usize,
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
        let mut worker_sys = System::new();
        let mut worker_history = MemoryHistory::default();
        let proc_worker = Worker::spawn("pcsweep-processes", move || {
            sample_processes(&mut worker_sys, &mut worker_history, cores as f32)
        });
        let drive_worker = Worker::spawn("pcsweep-drives", read_drives);
        let mut s = Self {
            sys,
            last_fast: None,
            last_disks: None,
            last_procs: None,
            proc_groups: Vec::new(),
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
            procs: Vec::new(),
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
            self.procs = snapshot.procs;
            self.proc_groups = snapshot.groups;
            self.process_warning = None;
        }
        if want_procs && due(&self.last_procs, Duration::from_secs(3)) {
            self.last_procs = Some(Instant::now());
            if !self.proc_worker.as_mut().is_some_and(|w| w.request()) {
                self.process_warning = Some(
                    "The process sampler is restarting. Displayed app values may be out of date."
                        .into(),
                );
                let cores = self.cores as f32;
                let mut sys = System::new();
                let mut history = MemoryHistory::default();
                self.proc_worker = Worker::spawn("pcsweep-processes", move || {
                    sample_processes(&mut sys, &mut history, cores)
                });
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
        let mut monitor = Self {
            sys: System::new(),
            last_fast: Some(Instant::now()),
            last_disks: Some(Instant::now()),
            last_procs: Some(Instant::now()),
            proc_groups: Vec::new(),
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
            procs: vec![
                Proc {
                    pid: 7001,
                    start_time: 10,
                    name: "chrome.exe".into(),
                    mem: 1800 * mb,
                    private_bytes: Some(2400 * mb),
                    growth: 300 * mb as i64,
                    observed_secs: 300,
                    cpu: 9.4,
                },
                Proc {
                    pid: 7002,
                    start_time: 20,
                    name: "chrome.exe".into(),
                    mem: 900 * mb,
                    private_bytes: Some(1200 * mb),
                    growth: 70 * mb as i64,
                    observed_secs: 300,
                    cpu: 2.1,
                },
                Proc {
                    pid: 7003,
                    start_time: 30,
                    name: "Code.exe".into(),
                    mem: 1200 * mb,
                    private_bytes: Some(1600 * mb),
                    growth: 64 * mb as i64,
                    observed_secs: 240,
                    cpu: 4.2,
                },
                Proc {
                    pid: 7004,
                    start_time: 40,
                    name: "explorer.exe".into(),
                    mem: 180 * mb,
                    private_bytes: Some(120 * mb),
                    growth: -8 * mb as i64,
                    observed_secs: 300,
                    cpu: 0.5,
                },
            ],
            host: "Preview PC".into(),
            os: "Windows 11".into(),
            cpu_name: "8-core processor".into(),
            cores: 8,
        };
        monitor.proc_groups = monitor.build_groups();
        monitor
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

    /// Processes grouped by executable name, heaviest first.
    pub fn groups(&self) -> Vec<ProcGroup> {
        self.proc_groups.clone()
    }

    #[cfg(test)]
    fn build_groups(&self) -> Vec<ProcGroup> {
        group_processes(&self.procs)
    }

    pub fn kill(&mut self, targets: &[(u32, u64)]) -> usize {
        use windows_sys::Win32::System::Threading::{TerminateProcess, PROCESS_TERMINATE};
        let mut n = 0;
        for &(pid, expected_start) in targets {
            // Keep the verified native handle through termination, including the
            // Windows critical-process check used by the gentler app actions.
            if let Ok(process) = checked_process(pid, expected_start, PROCESS_TERMINATE) {
                if unsafe { TerminateProcess(process.0, 1) } != 0 {
                    n += 1;
                }
            }
        }
        self.last_procs = None;
        n
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
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let script = format!(
        "Start-Process -FilePath '{}' -ArgumentList '--wait-for-parent {}' -Verb RunAs",
        exe.display().to_string().replace('\'', "''"),
        std::process::id()
    );
    util::hidden_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Original Windows policy, retained only for this exact process lifetime.
#[derive(Clone, Copy)]
pub struct PowerPolicy {
    pub control: u32,
    pub state: u32,
}

struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn checked_process(pid: u32, expected_start: u64, access: u32) -> Result<ProcessHandle, String> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, IsProcessCritical, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == std::process::id() || expected_start == 0 {
        return Err("This process is protected or its identity is unavailable.".into());
    }
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | access, 0, pid) };
    if raw == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let handle = ProcessHandle(raw);
    let mut critical = 0;
    if unsafe { IsProcessCritical(handle.0, &mut critical) } == 0 || critical != 0 {
        return Err("Windows could not verify that this is a noncritical app process.".into());
    }
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err("The process identity could not be checked.".into());
    }
    let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
    let actual_start = (ticks / 10_000_000).saturating_sub(11_644_473_600);
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut length) } == 0 {
        return Err("The executable could not be checked. System processes are protected.".into());
    }
    let path = String::from_utf16_lossy(&path[..length as usize]);
    let name = path.rsplit('\\').next().unwrap_or(&path);
    if !safe_kill_target(pid, expected_start, actual_start, name) {
        return Err("The app changed or is protected. Refresh the process list.".into());
    }
    Ok(handle)
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

/// Posts the normal close-window request; never terminates or waits on the UI.
pub fn request_app_close(pid: u32, start: u64) -> Result<usize, String> {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };
    struct Request {
        pid: u32,
        handle: windows_sys::Win32::Foundation::HANDLE,
        sent: usize,
    }
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        let request = &mut *(data as *mut Request);
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        if pid == request.pid && IsWindowVisible(window) != 0 {
            // Holding the old handle and checking its lifetime avoids sending a
            // close request to a window whose PID was reused after confirmation.
            if WaitForSingleObject(request.handle, 0) == 258
                && PostMessageW(window, WM_CLOSE, 0, 0) != 0
            {
                request.sent += 1;
            }
        }
        1
    }
    let handle = checked_process(pid, start, 0x0010_0000)?;
    let mut request = Request {
        pid,
        handle: handle.0,
        sent: 0,
    };
    if unsafe { EnumWindows(Some(visit), &mut request as *mut Request as isize) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if request.sent == 0 {
        Err("No closeable app windows were found. Background services do not have a normal Close action.".into())
    } else {
        Ok(request.sent)
    }
}

/// True once this exact process lifetime has ended, including when its PID has
/// since been reused by another process. A protected process that can't be opened
/// is never reported as exited.
pub fn process_exited(pid: u32, start: u64) -> bool {
    if pid == 0 || start == 0 {
        return false;
    }
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | 0x0010_0000, 0, pid) };
    if raw == 0 {
        // ERROR_INVALID_PARAMETER: no process has this ID any more.
        return std::io::Error::last_os_error().raw_os_error() == Some(87);
    }
    let handle = ProcessHandle(raw);
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exited = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return false;
    }
    if filetime_to_unix(&created) != start {
        return true;
    }
    // A signaled process handle proves termination, even if its actual exit
    // code happens to equal STILL_ACTIVE (259).
    unsafe { WaitForSingleObject(handle.0, 0) == 0 }
}

fn resident_memory(handle: &ProcessHandle) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    (unsafe { GetProcessMemoryInfo(handle.0, &mut memory, memory.cb) } != 0)
        .then_some(memory.WorkingSetSize as u64)
}

pub fn trim_app_memory(pid: u32, start: u64) -> Result<(u64, u64), String> {
    use windows_sys::Win32::System::{
        ProcessStatus::EmptyWorkingSet, Threading::PROCESS_SET_QUOTA,
    };
    let handle = checked_process(pid, start, PROCESS_SET_QUOTA)?;
    ensure_background(pid)?;
    let before =
        resident_memory(&handle).ok_or("The app's resident memory could not be measured.")?;
    if unsafe { EmptyWorkingSet(handle.0) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let after = resident_memory(&handle).unwrap_or(before);
    Ok((before, after))
}

pub fn enable_app_efficiency(pid: u32, start: u64) -> Result<PowerPolicy, String> {
    use windows_sys::Win32::System::Threading::{
        GetProcessInformation, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_SET_INFORMATION,
    };
    let handle = checked_process(pid, start, PROCESS_SET_INFORMATION)?;
    ensure_background(pid)?;
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

pub fn restore_app_efficiency(pid: u32, start: u64, original: PowerPolicy) -> Result<(), String> {
    use windows_sys::Win32::System::Threading::{
        GetProcessInformation, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE, PROCESS_SET_INFORMATION,
    };
    let handle = checked_process(pid, start, PROCESS_SET_INFORMATION)?;
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
    fn native_private_counter_is_for_the_exact_current_process() {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{GetCurrentProcess, GetProcessTimes},
        };
        let mut created: FILETIME = unsafe { std::mem::zeroed() };
        let mut exit = created;
        let mut kernel = created;
        let mut user = created;
        assert_ne!(
            unsafe {
                GetProcessTimes(
                    GetCurrentProcess(),
                    &mut created,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )
            },
            0
        );
        let start = filetime_to_unix(&created);
        assert!(private_usage(std::process::id(), start).is_some_and(|n| n > 0));
        assert_eq!(
            private_usage(std::process::id(), start.saturating_sub(1)),
            None
        );
        assert!(!process_exited(std::process::id(), start));
    }

    #[test]
    fn ending_apps_rejects_reused_pids_and_protected_processes() {
        let unrelated_pid = std::process::id().wrapping_add(1);
        assert!(safe_kill_target(unrelated_pid, 10, 10, "ordinary-app.exe"));
        assert!(!safe_kill_target(unrelated_pid, 10, 11, "ordinary-app.exe"));
        assert!(!safe_kill_target(unrelated_pid, 10, 10, "LSASS.EXE"));
        assert!(!safe_kill_target(std::process::id(), 10, 10, "renamed.exe"));
        assert!(!safe_kill_target(unrelated_pid, 0, 0, "ordinary-app.exe"));
        assert!(!safe_kill_target(
            unrelated_pid,
            10,
            10,
            "MpDefenderCoreService.exe"
        ));
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

    fn sample_process(pid: u32, start: u64, private: u64) -> Proc {
        Proc {
            pid,
            start_time: start,
            name: "test.exe".into(),
            mem: private,
            private_bytes: Some(private),
            growth: 0,
            observed_secs: 0,
            cpu: 0.0,
        }
    }

    #[test]
    fn history_tracks_private_growth_and_resets_for_reused_pid() {
        let now = Instant::now();
        let mut history = MemoryHistory::default();
        let mut processes = vec![sample_process(9000, 10, 100)];
        history.observe(&mut processes, now);
        processes[0].private_bytes = Some(500);
        history.observe(&mut processes, now + Duration::from_secs(3));
        assert_eq!(processes[0].growth, 400);
        assert_eq!(processes[0].observed_secs, 3);
        processes[0].start_time = 11;
        history.observe(&mut processes, now + Duration::from_secs(6));
        assert_eq!(processes[0].growth, 0);
        assert_eq!(history.samples.len(), 1);
        assert!(!history.samples.contains_key(&(9000, 10)));
    }

    #[test]
    fn history_bounds_samples_duration_and_processes() {
        let now = Instant::now();
        let mut history = MemoryHistory::default();
        let mut processes: Vec<_> = (1..=HISTORY_PROCESSES as u32 + 2)
            .map(|pid| sample_process(pid, 1, 100))
            .collect();
        history.observe(&mut processes, now);
        assert_eq!(history.samples.len(), HISTORY_PROCESSES);
        processes.truncate(1);
        for tick in 1..=HISTORY_SAMPLES + 10 {
            history.observe(&mut processes, now + Duration::from_secs(tick as u64 * 3));
        }
        assert_eq!(history.samples.len(), 1);
        assert!(history.samples[&(1, 1)].len() <= HISTORY_SAMPLES);
        assert!(processes[0].observed_secs <= HISTORY_SECONDS);
        history.observe(&mut [], now + Duration::from_secs(1000));
        assert!(history.samples.is_empty());
    }

    #[test]
    fn unavailable_private_memory_is_not_reported_as_zero_growth_measurement() {
        let mut process = sample_process(9000, 10, 100);
        process.private_bytes = None;
        let mut history = MemoryHistory::default();
        history.observe(&mut [process], Instant::now());
        assert!(history.samples.is_empty());
    }
}
