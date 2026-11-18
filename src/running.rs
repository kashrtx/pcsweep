//! Native Windows process inventory and lifetime-checked actions.
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, BOOL, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, FILETIME, HANDLE, HWND,
        INVALID_HANDLE_VALUE, LPARAM, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
        RemoteDesktop::ProcessIdToSessionId,
        Threading::{
            GetProcessTimes, IsProcessCritical, OpenProcess, QueryFullProcessImageNameW,
            TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_TERMINATE, SYNCHRONIZATION_SYNCHRONIZE,
        },
    },
    UI::WindowsAndMessaging::{
        EnumWindows, FlashWindowEx, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindowAsync, FLASHWINFO,
        FLASHW_TRAY, GWL_EXSTYLE, SW_RESTORE, WM_CLOSE, WS_EX_TOOLWINDOW,
    },
};

/// Creation time is the original Windows FILETIME, with 100-ns precision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identity {
    pub pid: u32,
    pub created: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Access {
    Available,
    Critical,
    Own,
    Idle,
    #[default]
    Restricted,
}

impl Access {
    pub fn restriction(self) -> Option<&'static str> {
        match self {
            Self::Available => None,
            Self::Critical => Some("Windows marks this process critical. Ending it can crash Windows."),
            Self::Own => Some("Use the window or tray menu to quit PCSweep."),
            Self::Idle => Some("System Idle represents unused processor time."),
            Self::Restricted => Some("Windows did not allow PCSweep to verify this process. Administrator mode may provide more access."),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Window {
    handle: HWND,
    thread: u32,
    pub title: String,
}

#[derive(Default)]
pub struct Process {
    pub identity: Identity,
    pub parent: u32,
    pub name: String,
    pub image: Option<PathBuf>,
    pub session: Option<u32>,
    pub memory: Option<u64>,
    pub private: Option<u64>,
    pub growth: i64,
    pub observed_secs: u64,
    pub cpu: Option<f32>,
    pub windows: Vec<Window>,
    pub access: Access,
    pub windows_component: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowId {
    Image(String, u32),
    Process(Identity),
}

pub struct Row {
    pub id: RowId,
    pub name: String,
    pub members: Vec<usize>,
    pub memory: u64,
    pub memory_known: usize,
    pub private: u64,
    pub private_known: usize,
    pub growth: i64,
    pub observed_secs: u64,
    pub cpu: f32,
    pub cpu_known: usize,
    pub window_count: usize,
    pub available: usize,
    pub search: String,
}

impl Row {
    fn new(id: RowId, name: &str) -> Self {
        Self {
            id,
            name: name.into(),
            members: Vec::new(),
            memory: 0,
            memory_known: 0,
            private: 0,
            private_known: 0,
            growth: 0,
            observed_secs: 0,
            cpu: 0.0,
            cpu_known: 0,
            window_count: 0,
            available: 0,
            search: String::new(),
        }
    }

    fn add(&mut self, index: usize, process: &Process) {
        self.members.push(index);
        if let Some(bytes) = process.memory {
            self.memory = self.memory.saturating_add(bytes);
            self.memory_known += 1;
        }
        if let Some(bytes) = process.private {
            self.private = self.private.saturating_add(bytes);
            self.private_known += 1;
        }
        self.growth = self.growth.saturating_add(process.growth);
        self.observed_secs = self.observed_secs.max(process.observed_secs);
        if let Some(cpu) = process.cpu {
            self.cpu += cpu;
            self.cpu_known += 1;
        }
        self.window_count += process.windows.len();
        self.available += usize::from(process.access == Access::Available);

        use std::fmt::Write;
        if self.members.len() == 1 {
            self.search.push_str(&process.name.to_lowercase());
            if let Some(path) = &process.image {
                let _ = write!(self.search, " {}", path.to_string_lossy().to_lowercase());
            }
        }
        let _ = write!(self.search, " {}", process.identity.pid);
        for window in &process.windows {
            let _ = write!(self.search, " {}", window.title.to_lowercase());
        }
    }

    pub fn title(&self) -> String {
        if self.members.len() > 1 {
            format!("{} ({})", self.name, self.members.len())
        } else {
            self.name.clone()
        }
    }
}

#[derive(Default)]
pub struct Snapshot {
    pub sampled_at: Option<Instant>,
    pub processes: Vec<Process>,
    pub groups: Vec<Row>,
    pub individuals: Vec<Row>,
    pub foreground: usize,
}

impl Snapshot {
    pub(crate) fn from_processes(processes: Vec<Process>, now: Instant) -> Self {
        let mut groups: HashMap<RowId, Row> = HashMap::new();
        let mut individuals = Vec::with_capacity(processes.len());
        for (index, process) in processes.iter().enumerate() {
            let instance = RowId::Process(process.identity);
            // Paths and sessions distinguish different installations and users.
            // An unreadable path is kept as its own PID instead of guessing.
            let group = match (&process.image, process.session) {
                (Some(image), Some(session)) => RowId::Image(crate::util::norm(image), session),
                _ => instance.clone(),
            };
            groups
                .entry(group.clone())
                .or_insert_with(|| Row::new(group, &process.name))
                .add(index, process);
            let mut row = Row::new(instance, &process.name);
            row.add(index, process);
            individuals.push(row);
        }
        let mut groups: Vec<_> = groups.into_values().collect();
        groups.sort_unstable_by(|a, b| b.memory.cmp(&a.memory).then_with(|| a.id.cmp(&b.id)));
        individuals.sort_unstable_by(|a, b| b.memory.cmp(&a.memory).then_with(|| a.id.cmp(&b.id)));
        let foreground = groups.iter().filter(|row| row.window_count > 0).count();
        Self {
            sampled_at: Some(now),
            processes,
            groups,
            individuals,
            foreground,
        }
    }
}

pub(crate) struct Handle(pub HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl Handle {
    fn open(pid: u32, rights: u32) -> io::Result<Self> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | rights, 0, pid) };
        if handle == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn times(&self) -> io::Result<(u64, u64)> {
        let [mut created, mut exited, mut kernel, mut user]: [FILETIME; 4] = unsafe { zeroed() };
        if unsafe { GetProcessTimes(self.0, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Ok((ticks(created), ticks(kernel).saturating_add(ticks(user))))
    }

    fn critical(&self) -> io::Result<bool> {
        let mut critical = 0;
        if unsafe { IsProcessCritical(self.0, &mut critical) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(critical != 0)
    }

    fn image(&self, buffer: &mut [u16]) -> Option<PathBuf> {
        let mut size = buffer.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(self.0, 0, buffer.as_mut_ptr(), &mut size) };
        (ok != 0 && size > 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..size as usize])))
    }

    fn memory(&self) -> Option<(u64, u64)> {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { zeroed() };
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ptr = (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast();
        (unsafe { GetProcessMemoryInfo(self.0, ptr, counters.cb) } != 0)
            .then_some((counters.WorkingSetSize as u64, counters.PrivateUsage as u64))
    }

    pub(crate) fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

fn session_id(pid: u32) -> Option<u32> {
    let mut id = 0;
    (unsafe { ProcessIdToSessionId(pid, &mut id) } != 0).then_some(id)
}

fn is_app_window(hwnd: HWND) -> bool {
    if unsafe { IsWindowVisible(hwnd) } == 0
        || unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW != 0
    {
        return false;
    }
    let mut cloaked = 0u32;
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        );
    }
    cloaked == 0
}

fn app_windows() -> HashMap<u32, Vec<Window>> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        if !is_app_window(hwnd) {
            return 1;
        }
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if thread == 0 || pid == 0 {
            return 1;
        }
        // GetWindowTextW retrieves the caption directly for other processes.
        // PCSweep's own window would send a synchronous message to the UI.
        let title = if pid == std::process::id() {
            "PCSweep".into()
        } else {
            let mut title = [0u16; 512];
            let count = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
            String::from_utf16_lossy(&title[..count.max(0) as usize])
        };
        if !title.is_empty() {
            let windows = unsafe { &mut *(data as *mut HashMap<u32, Vec<Window>>) };
            windows.entry(pid).or_default().push(Window {
                handle: hwnd,
                thread,
                title,
            });
        }
        1
    }
    let mut windows = HashMap::new();
    unsafe {
        EnumWindows(
            Some(collect),
            (&mut windows as *mut HashMap<u32, Vec<Window>>) as LPARAM,
        )
    };
    windows
}

const HISTORY_SECONDS: u64 = 600;
const HISTORY_SAMPLES: usize = 201;
const HISTORY_PROCESSES: usize = 2048;
#[derive(Default)]
struct MemoryHistory {
    samples: HashMap<Identity, VecDeque<(Instant, u64)>>,
}

impl MemoryHistory {
    fn observe(&mut self, processes: &mut [Process], now: Instant) {
        let live: HashSet<_> = processes.iter().map(|process| process.identity).collect();
        self.samples.retain(|id, _| live.contains(id));
        for process in processes {
            let Some(private) = process.private else {
                continue;
            };
            if process.identity.created == 0
                || (self.samples.len() >= HISTORY_PROCESSES
                    && !self.samples.contains_key(&process.identity))
            {
                continue;
            }
            let samples = self.samples.entry(process.identity).or_default();
            while samples
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at).as_secs() > HISTORY_SECONDS)
            {
                samples.pop_front();
            }
            samples.push_back((now, private));
            while samples.len() > HISTORY_SAMPLES {
                samples.pop_front();
            }
            if let Some((start, first)) = samples.front() {
                process.growth = (i128::from(private) - i128::from(*first))
                    .clamp(i128::from(i64::MIN), i128::from(i64::MAX))
                    as i64;
                process.observed_secs = now.duration_since(*start).as_secs();
            }
        }
    }
}

#[derive(Default)]
pub struct Sampler {
    previous: HashMap<Identity, Sample>,
    history: MemoryHistory,
}

struct Sample {
    cpu_ticks: u64,
    at: Instant,
    image: Option<PathBuf>,
}

impl Sampler {
    pub fn sample(&mut self, cores: usize) -> Result<Snapshot, String> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(format!(
                "Windows could not list processes. {}",
                io::Error::last_os_error()
            ));
        }
        let snapshot = Handle(snapshot);
        let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
            return Err(format!(
                "The process list could not be read. {}",
                io::Error::last_os_error()
            ));
        }
        let now = Instant::now();
        let windows_root = crate::util::env("SystemRoot").map(|path| crate::util::norm(&path));
        let mut windows = app_windows();
        let mut processes = Vec::new();
        let mut next = HashMap::new();
        let mut image_buffer = vec![0u16; 32768];
        loop {
            let pid = entry.th32ProcessID;
            let end = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let mut process = Process {
                identity: Identity { pid, created: 0 },
                parent: entry.th32ParentProcessID,
                name: String::from_utf16_lossy(&entry.szExeFile[..end]),
                session: session_id(pid),
                windows: windows.remove(&pid).unwrap_or_default(),
                ..Process::default()
            };
            if process.name.is_empty() {
                process.name = format!("PID {pid}");
            }
            process.access = if pid == 0 {
                process.name = "System Idle Process".into();
                Access::Idle
            } else if pid == std::process::id() {
                Access::Own
            } else {
                Access::Restricted
            };
            if let Ok(handle) = Handle::open(pid, 0) {
                if let Ok((created, ticks)) = handle.times() {
                    process.identity.created = created;
                    let previous = self.previous.get(&process.identity);
                    process.cpu = previous.and_then(|old| {
                        let elapsed = now.duration_since(old.at).as_secs_f64();
                        // A long gap occurs when neither process page is being viewed.
                        if !(0.1..=15.0).contains(&elapsed) {
                            return None;
                        }
                        let used = ticks.checked_sub(old.cpu_ticks)? as f64 / 10_000_000.0;
                        Some(
                            (100.0 * used / elapsed / cores.max(1) as f64).clamp(0.0, 100.0) as f32,
                        )
                    });
                    process.image = previous
                        .and_then(|old| old.image.clone())
                        .or_else(|| handle.image(&mut image_buffer));
                    if let Some(image) = &process.image {
                        if let Some(name) = image.file_name() {
                            process.name = name.to_string_lossy().into_owned();
                        }
                        process.windows_component = windows_root.as_ref().is_some_and(|root| {
                            crate::util::is_within(&crate::util::norm(image), root)
                        });
                    }
                    if process.access == Access::Restricted && created != 0 {
                        process.access = match handle.critical() {
                            Ok(true) => Access::Critical,
                            Ok(false) => Access::Available,
                            Err(_) => Access::Restricted,
                        };
                    }
                    if let Some((resident, private)) = handle.memory() {
                        process.memory = Some(resident);
                        process.private = Some(private);
                    }
                    next.insert(
                        process.identity,
                        Sample {
                            cpu_ticks: ticks,
                            at: now,
                            image: process.image.clone(),
                        },
                    );
                }
            }
            processes.push(process);
            if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    break;
                }
                return Err(format!(
                    "Windows returned an incomplete process list. {error}"
                ));
            }
        }
        self.previous = next;
        self.history.observe(&mut processes, now);
        Ok(Snapshot::from_processes(processes, now))
    }
}

/// A verified handle stays open through the operation. A different creation
/// timestamp means the reviewed instance is gone, even if the PID is in use.
pub(crate) fn checked_handle(id: Identity, rights: u32) -> Result<Option<Handle>, String> {
    if id.pid == std::process::id() {
        return Err(Access::Own.restriction().unwrap_or_default().into());
    }
    if id.pid == 0 || id.created == 0 {
        return Err("This process has no verifiable Windows identity.".into());
    }
    let handle = match Handle::open(id.pid, rights | SYNCHRONIZATION_SYNCHRONIZE) {
        Ok(handle) => handle,
        Err(error) if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) => {
            return Ok(None)
        }
        Err(error) => {
            return Err(format!(
                "Windows denied or could not open PID {}. {error}",
                id.pid
            ))
        }
    };
    if handle.exited()
        || handle
            .times()
            .map_err(|e| format!("Could not verify PID {}. {e}", id.pid))?
            .0
            != id.created
    {
        return Ok(None);
    }
    match handle.critical() {
        Ok(false) => Ok(Some(handle)),
        Ok(true) => Err(Access::Critical.restriction().unwrap_or_default().into()),
        Err(error) => Err(format!(
            "Could not verify whether PID {} is critical. {error}",
            id.pid
        )),
    }
}

pub fn request_end(id: Identity) -> Result<Option<Handle>, String> {
    let Some(handle) = checked_handle(id, PROCESS_TERMINATE)? else {
        return Ok(None);
    };
    // Recheck just before requesting termination. Windows still enforces the
    // access rights of the current token and the target's protection level.
    if unsafe { TerminateProcess(handle.0, 1) } == 0 {
        let error = io::Error::last_os_error();
        if handle.exited() {
            return Ok(None);
        }
        return Err(format!("Windows could not end PID {}. {error}", id.pid));
    }
    Ok(Some(handle))
}

fn matches_window(id: Identity, window: &Window, handle: &Handle) -> bool {
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(window.handle, &mut pid) };
    pid == id.pid
        && thread == window.thread
        && is_app_window(window.handle)
        && unsafe { WaitForSingleObject(handle.0, 0) } == WAIT_TIMEOUT
}

/// Send the standard window-close request and let the application offer to save.
pub fn request_close(id: Identity, windows: &[Window]) -> Result<Option<Handle>, String> {
    let Some(handle) = checked_handle(id, 0)? else {
        return Ok(None);
    };
    let mut sent = 0;
    let mut error = None;
    for window in windows
        .iter()
        .filter(|window| matches_window(id, window, &handle))
    {
        if unsafe { PostMessageW(window.handle, WM_CLOSE, 0, 0) } == 0 {
            error = Some(io::Error::last_os_error());
        } else {
            sent += 1;
        }
    }
    if sent > 0 {
        Ok(Some(handle))
    } else if let Some(error) = error {
        Err(format!("Windows blocked the close request. {error}"))
    } else {
        Err("This process has no reviewed window to close. End task is available for background processes.".into())
    }
}

pub fn show_window(id: Identity, window: &Window) -> Result<(), String> {
    let handle = Handle::open(id.pid, SYNCHRONIZATION_SYNCHRONIZE).map_err(|e| e.to_string())?;
    if handle.times().map_err(|e| e.to_string())?.0 != id.created
        || !matches_window(id, window, &handle)
    {
        return Err("That window has closed or changed. Select a current window.".into());
    }
    unsafe {
        if IsIconic(window.handle) != 0 {
            ShowWindowAsync(window.handle, SW_RESTORE);
        }
        if SetForegroundWindow(window.handle) == 0 {
            let flash = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd: window.handle,
                dwFlags: FLASHW_TRAY,
                uCount: 3,
                dwTimeout: 0,
            };
            FlashWindowEx(&flash);
        }
    }
    Ok(())
}

pub fn local_image_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    path.is_absolute()
        && matches!(path.components().next(), Some(Component::Prefix(p))
            if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
}

#[cfg(test)]
pub(crate) fn preview_fixture(mb: u64) -> Snapshot {
    let process = |pid,
                   name: &str,
                   directory: &str,
                   (ram, commit, growth, cpu): (u64, u64, i64, f32),
                   window: Option<&str>| Process {
        identity: Identity {
            pid,
            created: u64::from(pid) + 1,
        },
        parent: 7004,
        name: name.into(),
        image: Some(Path::new(directory).join(name)),
        session: Some(1),
        memory: Some(ram * mb),
        private: Some(commit * mb),
        growth: growth * mb as i64,
        observed_secs: 300,
        cpu: Some(cpu),
        windows: window
            .into_iter()
            .map(|title| Window {
                handle: pid as isize,
                thread: pid,
                title: title.into(),
            })
            .collect(),
        access: Access::Available,
        windows_component: name.eq_ignore_ascii_case("explorer.exe"),
    };
    Snapshot::from_processes(
        vec![
            process(
                7001,
                "chrome.exe",
                r"C:\Program Files\Google\Chrome\Application",
                (1800, 2400, 300, 9.4),
                Some("Research notes"),
            ),
            process(
                7002,
                "chrome.exe",
                r"C:\Program Files\Google\Chrome\Application",
                (900, 1200, 70, 2.1),
                None,
            ),
            process(
                7003,
                "Code.exe",
                r"D:\Tools\VS Code",
                (1200, 1600, 64, 4.2),
                Some("Visual Studio Code"),
            ),
            process(
                7004,
                "explorer.exe",
                r"C:\Windows",
                (180, 120, -8, 0.5),
                Some("Downloads"),
            ),
            process(
                7011,
                "sync-agent.exe",
                r"D:\Tools\Sync",
                (32, 45, 2, 0.1),
                None,
            ),
            Process {
                name: "System".into(),
                identity: Identity { pid: 4, created: 0 },
                ..Process::default()
            },
        ],
        Instant::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    struct Fixture(Child);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn verify_exit(handle: &Handle) {
        let until = Instant::now() + Duration::from_secs(5);
        while !handle.exited() {
            assert!(
                Instant::now() < until,
                "Windows did not signal the process exit"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn windows_can_list_and_end_a_disposable_workload_with_no_window() {
        use std::os::windows::process::CommandExt;
        let mut child = Fixture(
            Command::new(crate::platform::system_directory().unwrap().join("cmd.exe"))
                .args(["/D", "/Q"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(crate::util::CREATE_NO_WINDOW)
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        let mut sampler = Sampler::default();
        let snapshot = sampler.sample(4).unwrap();
        assert!(snapshot
            .processes
            .iter()
            .any(|p| p.identity.pid == std::process::id() && p.access == Access::Own));
        let process = snapshot
            .processes
            .iter()
            .find(|p| p.identity.pid == pid)
            .expect("a windowless child must appear");
        assert!(process.windows.is_empty());
        assert_eq!(process.access, Access::Available);
        assert!(process.image.as_ref().unwrap().ends_with("cmd.exe"));
        assert!(process.private.is_some_and(|bytes| bytes > 0));
        let stale = Identity {
            pid,
            created: process.identity.created - 1,
        };
        assert!(
            request_end(stale).unwrap().is_none(),
            "even a one-tick mismatch must reject the stale target"
        );
        assert!(child.0.try_wait().unwrap().is_none());
        let handle = request_end(process.identity).unwrap().unwrap();
        verify_exit(&handle);
    }

    // The parent launches this exact test in a disposable test executable.
    // Running the suite normally returns immediately.
    #[test]
    fn window_fixture_child() {
        let Ok(title) = std::env::var("PCSWEEP_TEST_PROCESS_WINDOW") else {
            return;
        };
        use windows_sys::Win32::{
            System::LibraryLoader::GetModuleHandleW,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
                RegisterClassW, TranslateMessage, MSG, WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW,
                WS_VISIBLE,
            },
        };
        unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: usize, l: isize) -> isize {
            if msg == WM_DESTROY {
                PostQuitMessage(0);
                return 0;
            }
            DefWindowProcW(hwnd, msg, w, l)
        }
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let class = wide("PCSweep.Test.DisposableProcess");
        let title = wide(&title);
        let module = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class_info = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            lpszClassName: class.as_ptr(),
            hInstance: module,
            ..unsafe { zeroed() }
        };
        assert_ne!(unsafe { RegisterClassW(&class_info) }, 0);
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                240,
                160,
                0,
                0,
                module,
                std::ptr::null(),
            )
        };
        assert_ne!(hwnd, 0);
        let mut message: MSG = unsafe { zeroed() };
        while unsafe { GetMessageW(&mut message, 0, 0, 0) } > 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    #[test]
    fn native_window_discovery_and_normal_close_verify_the_exit() {
        let title = format!("PCSweep disposable window {}", std::process::id());
        let child = Fixture(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "running::tests::window_fixture_child"])
                .env("PCSWEEP_TEST_PROCESS_WINDOW", &title)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut sampler = Sampler::default();
        let until = Instant::now() + Duration::from_secs(12);
        let (id, windows) = loop {
            let snapshot = sampler.sample(4).unwrap();
            if let Some(process) = snapshot.processes.into_iter().find(|p| {
                p.identity.pid == child.0.id() && p.windows.iter().any(|w| w.title == title)
            }) {
                break (process.identity, process.windows);
            }
            assert!(
                Instant::now() < until,
                "the fixture window never became visible"
            );
            std::thread::sleep(Duration::from_millis(40));
        };
        let handle = request_close(id, &windows).unwrap().unwrap();
        verify_exit(&handle);
    }

    #[test]
    fn inventory_retains_zero_unknown_and_same_name_processes() {
        let make = |pid, image: Option<&str>, session, memory| Process {
            identity: Identity {
                pid,
                created: u64::from(pid),
            },
            image: image.map(PathBuf::from),
            name: "example.exe".into(),
            session,
            memory,
            ..Process::default()
        };
        let snapshot = Snapshot::from_processes(
            vec![
                make(1001, Some(r"C:\Apps\example.exe"), Some(1), Some(0)),
                make(1002, Some(r"C:\Apps\example.exe"), Some(1), Some(100)),
                make(1003, Some(r"D:\Apps\example.exe"), Some(1), None),
                make(1004, Some(r"C:\Apps\example.exe"), Some(2), None),
                make(1005, None, Some(1), None),
            ],
            Instant::now(),
        );
        assert_eq!(snapshot.individuals.len(), 5);
        assert_eq!(snapshot.groups.len(), 4);
        assert_eq!(snapshot.groups[0].members.len(), 2);
        assert_eq!(snapshot.groups[0].memory_known, 2);
        assert_eq!(snapshot.groups[0].memory, 100);
        assert_eq!(snapshot.groups[1].memory_known, 0);
    }

    #[test]
    fn history_tracks_private_growth_and_resets_for_reused_pid() {
        let now = Instant::now();
        let mut history = MemoryHistory::default();
        let old = Identity {
            pid: 9000,
            created: 10,
        };
        let mut processes = [Process {
            identity: old,
            private: Some(100),
            ..Process::default()
        }];
        history.observe(&mut processes, now);
        processes[0].private = Some(500);
        history.observe(&mut processes, now + Duration::from_secs(3));
        assert_eq!(processes[0].growth, 400);
        assert_eq!(processes[0].observed_secs, 3);
        processes[0].identity.created += 1;
        history.observe(&mut processes, now + Duration::from_secs(6));
        assert_eq!(processes[0].growth, 0);
        assert_eq!(history.samples.len(), 1);
        assert!(!history.samples.contains_key(&old));
    }

    #[test]
    fn history_bounds_samples_and_keeps_unavailable_memory_unknown() {
        let now = Instant::now();
        let mut history = MemoryHistory::default();
        let mut processes: Vec<_> = (1..=HISTORY_PROCESSES as u32 + 3)
            .map(|pid| Process {
                identity: Identity { pid, created: 1 },
                private: (pid != 2).then_some(100),
                ..Process::default()
            })
            .collect();
        history.observe(&mut processes, now);
        assert_eq!(history.samples.len(), HISTORY_PROCESSES);
        assert!(!history.samples.contains_key(&processes[1].identity));
        processes.truncate(1);
        for tick in 1..=HISTORY_SAMPLES + 10 {
            history.observe(&mut processes, now + Duration::from_secs(tick as u64 * 3));
        }
        assert_eq!(history.samples.len(), 1);
        assert!(history.samples[&processes[0].identity].len() <= HISTORY_SAMPLES);
        assert!(processes[0].observed_secs <= HISTORY_SECONDS);
    }
}
