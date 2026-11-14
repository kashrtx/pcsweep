use crate::util;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysinfo::{DiskKind, Disks, Pid, System};

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
    pub name: String,
    pub mem: u64,
    pub cpu: f32,
}

pub struct ProcGroup {
    pub name: String,
    pub count: usize,
    pub mem: u64,
    pub cpu: f32,
    pub pids: Vec<u32>,
}

/// Processes that must never be ended from PCSweep.
pub const CRITICAL_PROCS: &[&str] = &[
    "system", "registry", "smss.exe", "csrss.exe", "wininit.exe", "services.exe", "lsass.exe",
    "lsaiso.exe", "winlogon.exe", "svchost.exe", "dwm.exe", "explorer.exe", "fontdrvhost.exe",
    "memory compression", "msmpeng.exe", "sihost.exe", "ctfmon.exe", "audiodg.exe",
    "system idle process", "secure system", "spoolsv.exe", "searchhost.exe", "startmenuexperiencehost.exe",
    "shellexperiencehost.exe", "taskhostw.exe", "runtimebroker.exe", "securityhealthservice.exe",
    "nissrv.exe", "wudfhost.exe", "conhost.exe", "dllhost.exe", "pcsweep.exe",
];

pub fn is_critical(name: &str) -> bool {
    let n = name.to_lowercase();
    CRITICAL_PROCS.iter().any(|c| *c == n)
}

pub struct SysMon {
    sys: System,
    last_fast: Option<Instant>,
    last_disks: Option<Instant>,
    last_procs: Option<Instant>,
    pub drives: Vec<Drive>,
    pub mem_total: u64,
    pub mem_used: u64,
    pub mem_avail: u64,
    pub swap_total: u64,
    pub swap_used: u64,
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
        let mut s = Self {
            sys,
            last_fast: None,
            last_disks: None,
            last_procs: None,
            drives: Vec::new(),
            mem_total: 0,
            mem_used: 0,
            mem_avail: 0,
            swap_total: 0,
            swap_used: 0,
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
            self.last_fast = Some(Instant::now());
        }
        if due(&self.last_disks, Duration::from_secs(5)) {
            self.refresh_drives();
        }
        if want_procs && due(&self.last_procs, Duration::from_secs(3)) {
            self.sys.refresh_processes();
            let cores = self.cores as f32;
            self.procs = self
                .sys
                .processes()
                .iter()
                .filter(|(_, p)| p.memory() > 0)
                .map(|(pid, p)| Proc {
                    pid: pid.as_u32(),
                    name: p.name().to_string(),
                    mem: p.memory(),
                    cpu: p.cpu_usage() / cores,
                })
                .collect();
            self.last_procs = Some(Instant::now());
        }
    }

    pub fn refresh_drives(&mut self) {
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
        self.drives = v;
        self.last_disks = Some(Instant::now());
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
        let mut map: HashMap<String, ProcGroup> = HashMap::new();
        for p in &self.procs {
            let g = map.entry(p.name.to_lowercase()).or_insert_with(|| ProcGroup {
                name: p.name.clone(),
                count: 0,
                mem: 0,
                cpu: 0.0,
                pids: Vec::new(),
            });
            g.count += 1;
            g.mem += p.mem;
            g.cpu += p.cpu;
            g.pids.push(p.pid);
        }
        let mut v: Vec<ProcGroup> = map.into_values().collect();
        v.sort_by(|a, b| b.mem.cmp(&a.mem));
        v
    }

    pub fn kill(&mut self, pids: &[u32]) -> usize {
        let mut n = 0;
        for pid in pids {
            if let Some(p) = self.sys.process(Pid::from_u32(*pid)) {
                if is_critical(p.name()) {
                    continue;
                }
                if p.kill() {
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
        "Start-Process -FilePath '{}' -Verb RunAs",
        exe.display().to_string().replace('\'', "''")
    );
    util::hidden_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
