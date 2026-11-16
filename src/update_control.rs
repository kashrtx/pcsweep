//! Explicit, reversible control of the Windows Update service only. This never
//! changes Medic, Orchestrator, tasks, security descriptors or update endpoints.
use crate::{platform, system};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    System::{Services::*, Threading::*},
};
use winreg::{enums::*, RegKey};

const JOURNAL: &str = r"SOFTWARE\PCSweep\WindowsUpdateServiceUndo";
const RECORD: &str = "StateV1";
const NAME: &str = "wuauserv";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceState {
    start: u32,
    delayed: bool,
    state: u32,
}
impl ServiceState {
    fn stable(&self) -> bool {
        matches!(self.state, SERVICE_STOPPED | SERVICE_RUNNING)
    }
    fn config_eq(&self, other: &Self) -> bool {
        self.start == other.start && self.delayed == other.delayed
    }
    fn config_during_change(&self, target: &Self) -> bool {
        // ChangeServiceConfig may clear delayed-auto before the separate
        // ChangeServiceConfig2 write restores it. Only prepared transactions
        // accept that intermediate state; completed records remain exact.
        self.start == target.start && (self.delayed == target.delayed || !self.delayed)
    }
    fn disabled(&self) -> Self {
        Self {
            start: SERVICE_DISABLED,
            state: SERVICE_STOPPED,
            ..self.clone()
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    DisablePrepared,
    Disabled,
    EnablePrepared,
    Enabled,
    RestorePrepared,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Undo {
    version: u32,
    original: ServiceState,
    phase: Phase,
    started: String,
    restore_from: Option<ServiceState>,
}
impl Undo {
    fn enables(&self) -> bool {
        self.original.start == SERVICE_DISABLED
    }
    fn target(&self) -> ServiceState {
        if self.enables() {
            ServiceState {
                start: SERVICE_DEMAND_START,
                delayed: false,
                state: SERVICE_STOPPED,
            }
        } else {
            self.original.disabled()
        }
    }
    fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || !matches!(
                self.original.start,
                SERVICE_AUTO_START | SERVICE_DEMAND_START | SERVICE_DISABLED
            )
            || !self.original.stable()
            || (self.enables() && self.original.state != SERVICE_STOPPED)
            || (matches!(self.phase, Phase::EnablePrepared | Phase::Enabled) && !self.enables())
            || (matches!(self.phase, Phase::DisablePrepared | Phase::Disabled) && self.enables())
            || chrono::DateTime::parse_from_rfc3339(&self.started).is_err()
            || (self.phase == Phase::RestorePrepared) != self.restore_from.is_some()
        {
            return Err(
                "The saved Windows Update service Undo is invalid. No service was changed.".into(),
            );
        }
        if let Some(from) = &self.restore_from {
            if !from.stable() || !from.config_during_change(&self.target()) {
                return Err("The saved Windows Update service recovery state is invalid.".into());
            }
        }
        Ok(())
    }
    fn owns(&self, current: &ServiceState) -> bool {
        if current == &self.original {
            return true;
        }
        let disabled = self.target();
        match self.phase {
            Phase::DisablePrepared => {
                current.config_during_change(&disabled)
                    && matches!(
                        current.state,
                        SERVICE_RUNNING | SERVICE_STOPPED | SERVICE_STOP_PENDING
                    )
            }
            Phase::Disabled => current == &disabled,
            Phase::EnablePrepared => {
                current.config_during_change(&disabled)
                    && matches!(
                        current.state,
                        SERVICE_STOPPED | SERVICE_RUNNING | SERVICE_START_PENDING
                    )
            }
            Phase::Enabled => current.config_eq(&disabled) && current.stable(),
            Phase::RestorePrepared => {
                let Some(from) = &self.restore_from else {
                    return false;
                };
                (current.config_eq(from) || current.config_during_change(&self.original))
                    && (current.state == from.state
                        || current.state == self.original.state
                        || current.state == SERVICE_START_PENDING
                        || current.state == SERVICE_STOP_PENDING)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub start_mode: String,
    pub state: String,
    pub delayed_auto: bool,
    pub disabled: bool,
    pub undo_available: bool,
    pub restore_available: bool,
    pub disable_available: bool,
    pub enable_available: bool,
    pub undo_restores_disabled: bool,
    pub since_disabled: Option<String>,
    pub warning: Option<String>,
    raw: ServiceState,
    undo: Result<Option<Undo>, String>,
}
impl Snapshot {
    pub fn fingerprint(&self) -> Result<String, String> {
        let undo = self.undo.as_ref().map_err(Clone::clone)?;
        fingerprint(&self.raw, undo)
    }
}
fn fingerprint(current: &ServiceState, undo: &Option<Undo>) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(current, undo)).map_err(|e| e.to_string())?)
    ))
}
fn snapshot(current: ServiceState, undo: Result<Option<Undo>, String>) -> Snapshot {
    let warning = match &undo {
        Err(e) => Some(e.clone()),
        Ok(Some(saved)) if !saved.owns(&current) => Some("Windows or another program changed the service after PCSweep. That change is preserved; automatic restoration is blocked.".into()),
        Ok(Some(saved)) if matches!(saved.phase, Phase::Disabled | Phase::Enabled) && current == saved.original => Some("Windows or another program has already restored the saved original service state. Review its current mode; Undo clears the completed saved change.".into()),
        Ok(Some(saved)) if !matches!(saved.phase, Phase::Disabled | Phase::Enabled) => Some("A previous service operation was interrupted or only partly completed. Restore the saved service before another change.".into()),
        _ => None,
    };
    let since_disabled = undo
        .as_ref()
        .ok()
        .and_then(|v| v.as_ref())
        .filter(|u| !u.enables() && current.start == SERVICE_DISABLED && u.owns(&current))
        .map(|u| u.started.clone());
    Snapshot {
        start_mode: match current.start {
            SERVICE_AUTO_START => "Automatic",
            SERVICE_DEMAND_START => "Manual / trigger start",
            SERVICE_DISABLED => "Disabled",
            SERVICE_BOOT_START => "Boot",
            SERVICE_SYSTEM_START => "System",
            _ => "Unknown",
        }
        .into(),
        state: match current.state {
            SERVICE_STOPPED => "Stopped",
            SERVICE_RUNNING => "Running",
            SERVICE_START_PENDING => "Starting",
            SERVICE_STOP_PENDING => "Stopping",
            SERVICE_CONTINUE_PENDING => "Continuing",
            SERVICE_PAUSE_PENDING => "Pausing",
            SERVICE_PAUSED => "Paused",
            _ => "Unknown",
        }
        .into(),
        delayed_auto: current.delayed,
        disabled: current.start == SERVICE_DISABLED,
        undo_available: undo.as_ref().is_ok_and(Option::is_some),
        restore_available: undo
            .as_ref()
            .ok()
            .and_then(|v| v.as_ref())
            .is_some_and(|u| u.owns(&current)),
        disable_available: matches!(&undo, Ok(None))
            && current.stable()
            && matches!(current.start, SERVICE_AUTO_START | SERVICE_DEMAND_START),
        enable_available: matches!(&undo, Ok(None))
            && current.start == SERVICE_DISABLED
            && current.state == SERVICE_STOPPED,
        undo_restores_disabled: undo
            .as_ref()
            .ok()
            .and_then(|v| v.as_ref())
            .is_some_and(Undo::enables),
        since_disabled,
        warning,
        raw: current,
        undo,
    }
}

trait Backend {
    fn read(&mut self) -> Result<ServiceState, String>;
    fn load(&mut self) -> Result<Option<Undo>, String>;
    fn save(&mut self, undo: &Option<Undo>) -> Result<(), String>;
    fn set_start(&mut self, start: u32, delayed: bool) -> Result<(), String>;
    fn stop(&mut self) -> Result<(), String>;
    fn start(&mut self) -> Result<(), String>;
    fn wait(&mut self, state: u32, config: &ServiceState) -> Result<ServiceState, String>;
}
fn read_reviewed(
    backend: &mut impl Backend,
    expected: &str,
) -> Result<(ServiceState, Option<Undo>), String> {
    let current = backend.read()?;
    let undo = backend.load()?;
    if let Some(u) = &undo {
        u.validate()?;
    }
    if fingerprint(&current, &undo)? != expected {
        return Err("The Windows Update service changed after review. Refresh and review its current state.".into());
    }
    Ok((current, undo))
}
fn disable_transaction(
    backend: &mut impl Backend,
    expected: &str,
    now: String,
) -> Result<String, String> {
    let (original, existing) = read_reviewed(backend, expected)?;
    if existing.is_some() {
        return Err("A saved Windows Update service change already exists. Restore it before another change.".into());
    }
    if !original.stable() || !matches!(original.start, SERVICE_AUTO_START | SERVICE_DEMAND_START) {
        return Err("Only a running or stopped service with its normal Automatic/Manual start mode can be disabled. An already disabled or transitioning service is preserved.".into());
    }
    let mut undo = Undo {
        version: 1,
        original: original.clone(),
        phase: Phase::DisablePrepared,
        started: now,
        restore_from: None,
    };
    undo.validate()?;
    backend.save(&Some(undo.clone()))?;
    if backend.read()? != original {
        return Err("The service changed before applying the reviewed action. The saved Undo remains available; no service write was attempted.".into());
    }
    backend.set_start(SERVICE_DISABLED, original.delayed)?;
    let current = backend.read()?;
    let target = original.disabled();
    if !current.config_eq(&target) {
        return Err(
            "Windows did not confirm the Disabled start mode. The original Undo was retained."
                .into(),
        );
    }
    match current.state {
        SERVICE_RUNNING => backend.stop()?,
        SERVICE_STOPPED | SERVICE_STOP_PENDING => {},
        _ => return Err("The service entered an unexpected state. Further changes stopped; Restore remains available.".into()),
    }
    let current = backend.wait(SERVICE_STOPPED, &target)?;
    if current != target {
        return Err("Windows did not confirm that the service stopped with the reviewed configuration. Undo was retained.".into());
    }
    undo.phase = Phase::Disabled;
    backend.save(&Some(undo))?;
    Ok("Windows Update service is Disabled and Stopped, confirmed by Windows. Windows may repair or re-enable it later. Restore returns the saved start mode and running state.".into())
}

fn enable_transaction(
    backend: &mut impl Backend,
    expected: &str,
    now: String,
) -> Result<String, String> {
    let (original, existing) = read_reviewed(backend, expected)?;
    if existing.is_some() || original.start != SERVICE_DISABLED || original.state != SERVICE_STOPPED
    {
        return Err("Only a stopped, already-disabled service without an existing PCSweep Undo can use this explicit enabling action. Refresh its state first.".into());
    }
    let mut undo = Undo {
        version: 1,
        original: original.clone(),
        phase: Phase::EnablePrepared,
        started: now,
        restore_from: None,
    };
    undo.validate()?;
    backend.save(&Some(undo.clone()))?;
    if backend.read()? != original {
        return Err(
            "The service changed while saving Undo. No service write was attempted.".into(),
        );
    }
    backend.set_start(SERVICE_DEMAND_START, false)?;
    let current = backend.read()?;
    if !current.config_eq(&undo.target()) || !current.stable() {
        return Err("Windows has not confirmed the Manual/trigger-start configuration. The saved original disabled state remains recoverable through Undo.".into());
    }
    undo.phase = Phase::Enabled;
    backend.save(&Some(undo))?;
    Ok(format!("Windows Update service is enabled for Manual/trigger start and currently {}. PCSweep did not start or stop it. Your previous Disabled state is saved for Undo; update policies and pauses are unchanged.", if current.state == SERVICE_RUNNING { "Running" } else { "Stopped" }))
}
fn restore_transaction(backend: &mut impl Backend, expected: &str) -> Result<String, String> {
    let (mut current, undo) = read_reviewed(backend, expected)?;
    let mut undo = undo.ok_or("There is no PCSweep service change to restore.")?;
    if !undo.owns(&current) {
        return Err("The service changed outside PCSweep. Its current configuration was preserved; automatic restore was blocked.".into());
    }
    if current == undo.original {
        backend.save(&None)?;
        return Ok("The service already matches its saved original state. The completed Undo record was cleared.".into());
    }
    if !current.stable() {
        let desired_state = if current.state == SERVICE_STOP_PENDING {
            SERVICE_STOPPED
        } else {
            SERVICE_RUNNING
        };
        current = backend.wait(desired_state, &current)?;
        if !undo.owns(&current) {
            return Err("The service changed while waiting for its pending operation. Its state was preserved.".into());
        }
    }
    if undo.phase != Phase::RestorePrepared {
        // An original config with a different runtime state was never a result
        // of our disable sequence, whose first write always disables starts.
        if !current.config_during_change(&undo.target()) {
            return Err(
                "The service configuration changed outside PCSweep. It was preserved.".into(),
            );
        }
        undo.restore_from = Some(current.clone());
        undo.phase = Phase::RestorePrepared;
        backend.save(&Some(undo.clone()))?;
    }
    if backend.read()? != current {
        return Err(
            "The service changed before restoration. Further writes stopped; Undo was retained."
                .into(),
        );
    }
    if !current.config_eq(&undo.original) {
        backend.set_start(undo.original.start, undo.original.delayed)?;
        current = backend.read()?;
        if !current.config_eq(&undo.original) {
            return Err("The original service configuration was not confirmed. Undo was retained for recovery.".into());
        }
    }
    if current.state != undo.original.state {
        match (current.state, undo.original.state) {
            (SERVICE_STOPPED, SERVICE_RUNNING) => backend.start()?,
            (SERVICE_RUNNING, SERVICE_STOPPED) => backend.stop()?,
            _ => return Err("The service state changed during restoration. Further changes stopped; Undo was retained.".into()),
        }
    }
    current = backend.wait(undo.original.state, &undo.original)?;
    if current != undo.original {
        return Err("Windows did not confirm the original service state. A trigger-start service may stop itself when idle. Undo remains available; refresh before retrying.".into());
    }
    backend.save(&None)?;
    Ok("Restored the exact saved Windows Update service start mode, delayed-start flag and running/stopped state, and read them back from Windows.".into())
}
fn forget_transaction(backend: &mut impl Backend, expected: &str) -> Result<String, String> {
    let (current, undo) = read_reviewed(backend, expected)?;
    let undo = undo.ok_or("There is no saved service change to forget.")?;
    if undo.owns(&current) {
        return Err("The saved change is still restorable. Use Restore instead of discarding its original state.".into());
    }
    backend.save(&None)?;
    Ok("Forgot the stale PCSweep service Undo. The current Windows service configuration was not changed.".into())
}

struct ServiceHandle(isize);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
struct NativeBackend {
    _manager: ServiceHandle,
    service: ServiceHandle,
}
fn error(action: &str) -> String {
    format!("{action}: {}", std::io::Error::last_os_error())
}
impl NativeBackend {
    fn open(mutate: bool) -> Result<Self, String> {
        Self::open_impl(NAME, mutate)
    }
    #[cfg(test)]
    fn open_name(name: &str) -> Result<Self, String> {
        assert!(name.starts_with("PCSweepTest-"));
        Self::open_impl(name, true)
    }
    fn open_impl(service_name: &str, mutate: bool) -> Result<Self, String> {
        let manager =
            unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if manager == 0 {
            return Err(error("Cannot read the Windows service manager"));
        }
        let manager = ServiceHandle(manager);
        let name: Vec<u16> = service_name.encode_utf16().chain(Some(0)).collect();
        let rights = SERVICE_QUERY_CONFIG
            | SERVICE_QUERY_STATUS
            | if mutate {
                SERVICE_CHANGE_CONFIG | SERVICE_STOP | SERVICE_START
            } else {
                0
            };
        let service = unsafe { OpenServiceW(manager.0, name.as_ptr(), rights) };
        if service == 0 {
            return Err(error("Cannot access the Windows Update service"));
        }
        Ok(Self {
            _manager: manager,
            service: ServiceHandle(service),
        })
    }
    fn state(&self) -> Result<ServiceState, String> {
        let mut bytes = 0;
        unsafe {
            QueryServiceConfigW(self.service.0, std::ptr::null_mut(), 0, &mut bytes);
        }
        if bytes == 0 || bytes > 64 * 1024 {
            return Err(error(
                "Windows did not report the service configuration size",
            ));
        }
        let mut memory = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            QueryServiceConfigW(
                self.service.0,
                memory.as_mut_ptr().cast(),
                (memory.len() * std::mem::size_of::<usize>()) as u32,
                &mut bytes,
            )
        } == 0
        {
            return Err(error("Cannot read the Windows Update start mode"));
        }
        let config = unsafe { &*memory.as_ptr().cast::<QUERY_SERVICE_CONFIGW>() };
        let mut delayed: SERVICE_DELAYED_AUTO_START_INFO = unsafe { std::mem::zeroed() };
        if unsafe {
            QueryServiceConfig2W(
                self.service.0,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                (&mut delayed as *mut SERVICE_DELAYED_AUTO_START_INFO).cast(),
                std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>() as u32,
                &mut bytes,
            )
        } == 0
        {
            return Err(error("Cannot read the service delayed-start preference"));
        }
        let mut status: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
        if unsafe {
            QueryServiceStatusEx(
                self.service.0,
                SC_STATUS_PROCESS_INFO,
                (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut bytes,
            )
        } == 0
        {
            return Err(error("Cannot read the Windows Update service state"));
        }
        if config.dwServiceType & (SERVICE_WIN32_OWN_PROCESS | SERVICE_WIN32_SHARE_PROCESS) == 0 {
            return Err("Windows Update was not reported as a normal Win32 service. No change is available.".into());
        }
        Ok(ServiceState {
            start: config.dwStartType,
            delayed: delayed.fDelayedAutostart != 0,
            state: status.dwCurrentState,
        })
    }
}
impl Backend for NativeBackend {
    fn read(&mut self) -> Result<ServiceState, String> {
        self.state()
    }
    fn load(&mut self) -> Result<Option<Undo>, String> {
        load_undo()
    }
    fn save(&mut self, undo: &Option<Undo>) -> Result<(), String> {
        save_undo(undo)
    }
    fn set_start(&mut self, start: u32, delayed: bool) -> Result<(), String> {
        if !matches!(
            start,
            SERVICE_AUTO_START | SERVICE_DEMAND_START | SERVICE_DISABLED
        ) {
            return Err("Unsupported service start mode.".into());
        }
        let before = self.state()?;
        if before.start != start
            && unsafe {
                ChangeServiceConfigW(
                    self.service.0,
                    SERVICE_NO_CHANGE,
                    start,
                    SERVICE_NO_CHANGE,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            } == 0
        {
            return Err(error(
                "Windows refused the service start-mode change; Undo was retained",
            ));
        }
        let current = self.state()?;
        if current.start != start || (current.delayed != before.delayed && current.delayed) {
            return Err("The service configuration changed during the start-mode write. Further writes stopped and Undo was retained.".into());
        }
        if current.delayed != delayed {
            let info = SERVICE_DELAYED_AUTO_START_INFO {
                fDelayedAutostart: i32::from(delayed),
            };
            if unsafe {
                ChangeServiceConfig2W(
                    self.service.0,
                    SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                    (&info as *const SERVICE_DELAYED_AUTO_START_INFO).cast(),
                )
            } == 0
            {
                return Err(error("Windows refused the saved delayed-start preference; the partial change remains recoverable through Undo"));
            }
        }
        Ok(())
    }
    fn stop(&mut self) -> Result<(), String> {
        let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
        if unsafe { ControlService(self.service.0, SERVICE_CONTROL_STOP, &mut status) } == 0 {
            let failure = error("Windows did not stop the Update service; its start mode may already be Disabled and Undo was retained");
            if self.state()?.state != SERVICE_STOPPED {
                return Err(failure);
            }
        }
        Ok(())
    }
    fn start(&mut self) -> Result<(), String> {
        if unsafe { StartServiceW(self.service.0, 0, std::ptr::null()) } == 0 {
            let failure = error("Windows did not restart the Update service; Undo was retained");
            if self.state()?.state != SERVICE_RUNNING {
                return Err(failure);
            }
        }
        Ok(())
    }
    fn wait(&mut self, state: u32, config: &ServiceState) -> Result<ServiceState, String> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let now = self.state()?;
            if !now.config_eq(config) {
                return Err("The Windows Update service configuration changed while waiting. Further work stopped and Undo was retained.".into());
            }
            if now.state == state {
                return Ok(now);
            }
            if Instant::now() >= deadline {
                return Err("Windows has not reached the requested service state after 20 seconds. Undo was retained; refresh before retrying.".into());
            }
            if !matches!(
                now.state,
                SERVICE_START_PENDING | SERVICE_STOP_PENDING | SERVICE_RUNNING | SERVICE_STOPPED
            ) {
                return Err("The service entered an unexpected state. Undo was retained.".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn load_undo() -> Result<Option<Undo>, String> {
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(JOURNAL) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Windows Update service Undo cannot be read: {e}")),
    };
    let text: String = match key.get_value(RECORD) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Windows Update service Undo cannot be read: {e}")),
    };
    if text.len() > 8192 {
        return Err(
            "Windows Update service Undo is too large. Service changes are blocked.".into(),
        );
    }
    let undo: Undo = serde_json::from_str(&text)
        .map_err(|_| "Windows Update service Undo is damaged. Service changes are blocked.")?;
    undo.validate()?;
    Ok(Some(undo))
}
fn protect(key: &RegKey) -> Result<(), String> {
    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            version: u32,
            descriptor: *mut *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
        fn RegSetKeySecurity(
            key: isize,
            information: u32,
            descriptor: *const std::ffi::c_void,
        ) -> i32;
    }
    let text: Vec<u16> = "D:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;BU)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(error("Cannot protect service Undo"));
    }
    let code = unsafe { RegSetKeySecurity(key.raw_handle(), 4 | 0x8000_0000, descriptor) };
    unsafe {
        LocalFree(descriptor);
    }
    if code != 0 {
        return Err(format!(
            "Cannot secure service Undo (code {code}). No service change was attempted."
        ));
    }
    Ok(())
}
fn save_undo(undo: &Option<Undo>) -> Result<(), String> {
    if !system::is_admin() {
        return Err("Administrator rights are required to save service Undo.".into());
    }
    if let Some(v) = undo {
        v.validate()?;
    }
    let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey_with_flags(JOURNAL, KEY_ALL_ACCESS)
        .map_err(|e| e.to_string())?;
    protect(&key)?;
    if let Some(undo) = undo {
        key.set_value(
            RECORD,
            &serde_json::to_string(undo).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    } else {
        match key.delete_value(RECORD) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if unsafe { windows_sys::Win32::System::Registry::RegFlushKey(key.raw_handle()) } != 0 {
        return Err(
            "Service Undo could not be durably saved. Further service changes stopped.".into(),
        );
    }
    Ok(())
}
struct Lock(isize);
impl Lock {
    fn take() -> Result<Self, String> {
        let name: Vec<u16> = "Global\\PCSweep.WindowsControls.v1"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle == 0 {
            return Err(error("Windows controls are in use or unavailable"));
        }
        match unsafe { WaitForSingleObject(handle, 0) } {
            0 | 0x80 => Ok(Self(handle)),
            _ => {
                unsafe {
                    CloseHandle(handle);
                }
                Err("Another Windows controls operation is running.".into())
            }
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}
fn source_is_empty(value: &winreg::RegValue) -> bool {
    if !matches!(value.vtype, REG_SZ | REG_EXPAND_SZ)
        || value.bytes.len() % 2 != 0
        || value.bytes.len() > 8192
    {
        return false;
    }
    let units: Vec<u16> = value
        .bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16(&units).is_ok_and(|v| v.trim_matches('\0').trim().is_empty())
}
fn check_sources(
    use_server: Option<&winreg::RegValue>,
    servers: &[winreg::RegValue],
) -> Result<(), String> {
    if let Some(value) = use_server {
        if value.vtype != REG_DWORD || value.bytes.as_slice() != 0u32.to_le_bytes().as_slice() {
            return Err("Windows Update has an enabled or unrecognized managed update-server preference. Its service was preserved.".into());
        }
    }
    if servers.iter().any(|v| !source_is_empty(v)) {
        return Err("Windows Update has a configured or unrecognized organization update-server source. Its service was preserved.".into());
    }
    Ok(())
}
/// Registry management checks may also be used by the background UI inventory.
/// Callers separately check client build, domain membership and administrator
/// rights. Unknown/invalid source types fail closed; an explicit DWORD zero is
/// a recognized disabled WSUS flag, not evidence that the device is managed.
pub fn managed_guard() -> Result<(), String> {
    match RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Provisioning\OMADM\Accounts")
    {
        Ok(key) => {
            for entry in key.enum_keys() {
                entry.map_err(|e| e.to_string())?;
                return Err("This PC has organization-management enrollment. Its Windows Update service was preserved.".into());
            }
            for entry in key.enum_values() {
                entry.map_err(|e| e.to_string())?;
                return Err("This PC has organization-management enrollment. Its Windows Update service was preserved.".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "Organization-management state could not be checked: {e}"
            ))
        }
    }
    let root = RegKey::predef(HKEY_LOCAL_MACHINE);
    let mut use_server = None;
    let mut servers = Vec::new();
    for (path, names) in [
        (
            r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU",
            &["UseWUServer"][..],
        ),
        (
            r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate",
            &["WUServer", "WUStatusServer", "UpdateServiceUrlAlternate"][..],
        ),
    ] {
        let key = match root.open_subkey(path) {
            Ok(key) => key,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!(
                    "Managed Windows Update sources could not be checked: {e}"
                ))
            }
        };
        for name in names {
            match key.get_raw_value(name) {
                Ok(value) if *name == "UseWUServer" => use_server = Some(value),
                Ok(value) => servers.push(value),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(format!(
                        "A managed Windows Update source could not be checked: {e}"
                    ))
                }
            }
        }
    }
    check_sources(use_server.as_ref(), &servers)
}
fn mutation_guard() -> Result<(), String> {
    if !system::is_admin() {
        return Err(
            "Restart PCSweep as administrator to change the Windows Update service.".into(),
        );
    }
    let version = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .map_err(|e| e.to_string())?;
    let kind: String = version
        .get_value("InstallationType")
        .map_err(|e| e.to_string())?;
    let build: String = version
        .get_value("CurrentBuildNumber")
        .map_err(|e| e.to_string())?;
    if !kind.eq_ignore_ascii_case("Client") || build.parse::<u32>().unwrap_or(0) < 19045 {
        return Err("Direct service control is available only on supported Windows 10/11 client installations.".into());
    }
    managed_guard()?;
    let managed = platform::powershell("$d=Get-CimInstance Win32_ComputerSystem -Property PartOfDomain -ErrorAction Stop; if($null -eq $d){throw 'Computer management state is unavailable'}; if($d.PartOfDomain){'managed'}else{'local'}", Duration::from_secs(20))?;
    if managed.trim() != "local" {
        return Err("This PC may be organization-managed. PCSweep preserves its Windows Update service configuration.".into());
    }
    Ok(())
}
pub fn query() -> Result<Snapshot, String> {
    let mut backend = NativeBackend::open(false)?;
    Ok(snapshot(backend.read()?, backend.load()))
}
pub fn disable(expected: String) -> Result<String, String> {
    mutation_guard()?;
    let _lock = Lock::take()?;
    disable_transaction(
        &mut NativeBackend::open(true)?,
        &expected,
        chrono::Utc::now().to_rfc3339(),
    )
}
pub fn enable(expected: String) -> Result<String, String> {
    mutation_guard()?;
    let _lock = Lock::take()?;
    enable_transaction(
        &mut NativeBackend::open(true)?,
        &expected,
        chrono::Utc::now().to_rfc3339(),
    )
}
pub fn restore(expected: String) -> Result<String, String> {
    mutation_guard()?;
    let _lock = Lock::take()?;
    restore_transaction(&mut NativeBackend::open(true)?, &expected)
}
pub fn forget(expected: String) -> Result<String, String> {
    if !system::is_admin() {
        return Err("Administrator rights are required to forget machine service Undo.".into());
    }
    let _lock = Lock::take()?;
    forget_transaction(&mut NativeBackend::open(false)?, &expected)
}

#[cfg(test)]
pub fn preview_fixture() -> Snapshot {
    snapshot(
        ServiceState {
            start: SERVICE_DEMAND_START,
            delayed: false,
            state: SERVICE_RUNNING,
        },
        Ok(None),
    )
}
#[cfg(test)]
pub fn preview_disabled_fixture() -> Snapshot {
    let original = ServiceState {
        start: SERVICE_DEMAND_START,
        delayed: false,
        state: SERVICE_RUNNING,
    };
    snapshot(
        original.disabled(),
        Ok(Some(Undo {
            version: 1,
            original,
            phase: Phase::Disabled,
            started: "2026-10-01T12:00:00Z".into(),
            restore_from: None,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry_string(text: &str) -> winreg::RegValue {
        winreg::RegValue {
            vtype: REG_SZ,
            bytes: text
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect(),
        }
    }
    #[test]
    fn only_known_disabled_wsus_flag_and_empty_server_values_are_unmanaged() {
        let flag = |n: u32| winreg::RegValue {
            vtype: REG_DWORD,
            bytes: n.to_le_bytes().to_vec(),
        };
        assert!(check_sources(None, &[]).is_ok());
        assert!(check_sources(
            Some(&flag(0)),
            &[registry_string(""), registry_string("  ")]
        )
        .is_ok());
        assert!(check_sources(Some(&flag(1)), &[]).is_err());
        assert!(check_sources(Some(&flag(2)), &[]).is_err());
        assert!(check_sources(Some(&registry_string("0")), &[]).is_err());
        assert!(check_sources(
            Some(&flag(0)),
            &[registry_string("https://managed.example")]
        )
        .is_err());
        assert!(check_sources(
            None,
            &[winreg::RegValue {
                vtype: REG_BINARY,
                bytes: Vec::new()
            }]
        )
        .is_err());
        assert!(check_sources(
            None,
            &[winreg::RegValue {
                vtype: REG_SZ,
                bytes: vec![0]
            }]
        )
        .is_err());
    }
    struct Fake {
        current: ServiceState,
        undo: Option<Undo>,
        writes: usize,
        saves: usize,
        fail_write: Option<usize>,
        fail_save: Option<usize>,
        clear_delayed_on_mode: bool,
        fail_delayed: bool,
    }
    impl Fake {
        fn new(start: u32, state: u32, delayed: bool) -> Self {
            Self {
                current: ServiceState {
                    start,
                    state,
                    delayed,
                },
                undo: None,
                writes: 0,
                saves: 0,
                fail_write: None,
                fail_save: None,
                clear_delayed_on_mode: false,
                fail_delayed: false,
            }
        }
        fn expected(&self) -> String {
            fingerprint(&self.current, &self.undo).unwrap()
        }
        fn write(&mut self) -> Result<(), String> {
            self.writes += 1;
            if self.fail_write == Some(self.writes) {
                Err("simulated SCM refusal".into())
            } else {
                Ok(())
            }
        }
    }
    impl Backend for Fake {
        fn read(&mut self) -> Result<ServiceState, String> {
            Ok(self.current.clone())
        }
        fn load(&mut self) -> Result<Option<Undo>, String> {
            Ok(self.undo.clone())
        }
        fn save(&mut self, undo: &Option<Undo>) -> Result<(), String> {
            self.saves += 1;
            if self.fail_save == Some(self.saves) {
                return Err("simulated journal refusal".into());
            }
            self.undo = undo.clone();
            Ok(())
        }
        fn set_start(&mut self, start: u32, delayed: bool) -> Result<(), String> {
            self.write()?;
            assert!(self.undo.is_some(), "write-ahead Undo is required");
            self.current.start = start;
            if self.clear_delayed_on_mode {
                self.current.delayed = false;
            }
            if self.fail_delayed && delayed {
                return Err("simulated delayed-start refusal after changing start mode".into());
            }
            self.current.delayed = delayed;
            Ok(())
        }
        fn stop(&mut self) -> Result<(), String> {
            self.write()?;
            self.current.state = SERVICE_STOPPED;
            Ok(())
        }
        fn start(&mut self) -> Result<(), String> {
            self.write()?;
            self.current.state = SERVICE_RUNNING;
            Ok(())
        }
        fn wait(&mut self, state: u32, config: &ServiceState) -> Result<ServiceState, String> {
            if !self.current.config_eq(config) || self.current.state != state {
                Err("state did not settle".into())
            } else {
                Ok(self.current.clone())
            }
        }
    }
    fn disable_fake(f: &mut Fake) -> Result<String, String> {
        let expected = f.expected();
        disable_transaction(f, &expected, "2026-11-15T12:00:00Z".into())
    }
    fn restore_fake(f: &mut Fake) -> Result<String, String> {
        let expected = f.expected();
        restore_transaction(f, &expected)
    }
    #[test]
    fn exact_start_mode_delayed_flag_and_runtime_roundtrip() {
        for start in [SERVICE_AUTO_START, SERVICE_DEMAND_START] {
            for state in [SERVICE_RUNNING, SERVICE_STOPPED] {
                for delayed in [false, true] {
                    let mut f = Fake::new(start, state, delayed);
                    let original = f.current.clone();
                    disable_fake(&mut f).unwrap();
                    assert_eq!(f.current, original.disabled());
                    restore_fake(&mut f).unwrap();
                    assert_eq!(f.current, original);
                    assert!(f.undo.is_none());
                }
            }
        }
    }
    #[test]
    fn journal_failure_blocks_all_service_writes() {
        let mut f = Fake::new(SERVICE_DEMAND_START, SERVICE_RUNNING, false);
        f.fail_save = Some(1);
        assert!(disable_fake(&mut f).is_err());
        assert_eq!(f.writes, 0);
    }
    #[test]
    fn explicitly_enabling_a_preexisting_block_preserves_it_for_exact_undo() {
        for delayed in [false, true] {
            let mut f = Fake::new(SERVICE_DISABLED, SERVICE_STOPPED, delayed);
            let original = f.current.clone();
            assert!(snapshot(f.current.clone(), Ok(None)).enable_available);
            let fp = f.expected();
            enable_transaction(&mut f, &fp, "2026-11-16T00:00:00Z".into()).unwrap();
            assert_eq!(f.current.start, SERVICE_DEMAND_START);
            assert_eq!(f.current.state, SERVICE_STOPPED);
            assert!(!f.current.delayed);
            // Windows can legitimately trigger-start an enabled Manual service.
            f.current.state = SERVICE_RUNNING;
            assert!(snapshot(f.current.clone(), Ok(f.undo.clone())).restore_available);
            restore_fake(&mut f).unwrap();
            assert_eq!(f.current, original);
            assert!(f.undo.is_none());
        }
    }
    #[test]
    fn enabling_requires_review_and_durable_undo_and_legacy_records_still_decode() {
        let mut f = Fake::new(SERVICE_DISABLED, SERVICE_STOPPED, false);
        let fp = f.expected();
        f.fail_save = Some(1);
        assert!(enable_transaction(&mut f, &fp, "2026-11-16T00:00:00Z".into()).is_err());
        assert_eq!(f.writes, 0);
        f.fail_save = None;
        assert!(enable_transaction(&mut f, "stale", "2026-11-16T00:00:00Z".into()).is_err());
        assert_eq!(f.writes, 0);
        let old = Undo {
            version: 1,
            original: ServiceState {
                start: SERVICE_DEMAND_START,
                delayed: false,
                state: SERVICE_RUNNING,
            },
            phase: Phase::Disabled,
            started: "2026-11-15T00:00:00Z".into(),
            restore_from: None,
        };
        let saved = serde_json::to_vec(&old).unwrap();
        let decoded: Undo = serde_json::from_slice(&saved).unwrap();
        decoded.validate().unwrap();
        assert!(!decoded.enables());
    }
    #[test]
    fn failed_stop_preserves_undo_and_restores_partial_disable() {
        let mut f = Fake::new(SERVICE_AUTO_START, SERVICE_RUNNING, true);
        let original = f.current.clone();
        f.fail_write = Some(2);
        assert!(disable_fake(&mut f).is_err());
        assert_eq!(f.current.start, SERVICE_DISABLED);
        assert!(f.undo.is_some());
        f.fail_write = None;
        restore_fake(&mut f).unwrap();
        assert_eq!(f.current, original);
        assert!(f.undo.is_none());
    }
    #[test]
    fn delayed_start_partial_write_is_recoverable_during_disable_and_restore() {
        for fail_during_restore in [false, true] {
            let mut f = Fake::new(SERVICE_AUTO_START, SERVICE_RUNNING, true);
            let original = f.current.clone();
            f.clear_delayed_on_mode = true;
            if fail_during_restore {
                disable_fake(&mut f).unwrap();
                f.fail_delayed = true;
                assert!(restore_fake(&mut f).is_err());
                assert_eq!(f.current.start, SERVICE_AUTO_START);
            } else {
                f.fail_delayed = true;
                assert!(disable_fake(&mut f).is_err());
                assert_eq!(f.current.start, SERVICE_DISABLED);
            }
            assert!(!f.current.delayed);
            assert!(snapshot(f.current.clone(), Ok(f.undo.clone())).restore_available);
            f.fail_delayed = false;
            restore_fake(&mut f).unwrap();
            assert_eq!(f.current, original);
            assert!(f.undo.is_none());
        }
    }
    #[test]
    fn failed_completion_record_is_recoverable() {
        let mut f = Fake::new(SERVICE_DEMAND_START, SERVICE_RUNNING, false);
        let original = f.current.clone();
        f.fail_save = Some(2);
        assert!(disable_fake(&mut f).is_err());
        assert_eq!(f.undo.as_ref().unwrap().phase, Phase::DisablePrepared);
        f.fail_save = None;
        restore_fake(&mut f).unwrap();
        assert_eq!(f.current, original);
    }
    #[test]
    fn restore_can_resume_after_start_refusal() {
        let mut f = Fake::new(SERVICE_AUTO_START, SERVICE_RUNNING, true);
        let original = f.current.clone();
        disable_fake(&mut f).unwrap();
        f.fail_write = Some(f.writes + 2);
        assert!(restore_fake(&mut f).is_err());
        assert_eq!(f.current.start, original.start);
        assert!(f.undo.is_some());
        f.fail_write = None;
        restore_fake(&mut f).unwrap();
        assert_eq!(f.current, original);
        assert!(f.undo.is_none());
    }
    #[test]
    fn outside_configuration_and_completed_runtime_changes_are_preserved() {
        for change_start in [true, false] {
            let mut f = Fake::new(SERVICE_AUTO_START, SERVICE_RUNNING, false);
            disable_fake(&mut f).unwrap();
            if change_start {
                f.current.start = SERVICE_DEMAND_START;
            } else {
                f.current.state = SERVICE_RUNNING;
            }
            let outside = f.current.clone();
            let writes = f.writes;
            assert!(restore_fake(&mut f).is_err());
            assert_eq!(f.current, outside);
            assert_eq!(f.writes, writes);
        }
    }
    #[test]
    fn outside_delayed_start_change_is_preserved() {
        let mut f = Fake::new(SERVICE_AUTO_START, SERVICE_RUNNING, true);
        disable_fake(&mut f).unwrap();
        f.current.delayed = false;
        let writes = f.writes;
        assert!(restore_fake(&mut f).is_err());
        assert_eq!(f.writes, writes);
    }
    #[test]
    fn stale_review_and_unowned_disabled_service_block_mutations() {
        let mut f = Fake::new(SERVICE_DEMAND_START, SERVICE_RUNNING, false);
        let old = f.expected();
        f.current.state = SERVICE_STOPPED;
        assert!(disable_transaction(&mut f, &old, "2026-11-15T12:00:00Z".into()).is_err());
        assert_eq!(f.writes, 0);
        let mut disabled = Fake::new(SERVICE_DISABLED, SERVICE_STOPPED, false);
        assert!(disable_fake(&mut disabled).is_err());
        assert!(disabled.undo.is_none());
    }
    #[test]
    fn already_restored_service_only_clears_record() {
        let mut f = Fake::new(SERVICE_DEMAND_START, SERVICE_STOPPED, false);
        let original = f.current.clone();
        disable_fake(&mut f).unwrap();
        f.current = original;
        let report = snapshot(f.current.clone(), Ok(f.undo.clone()));
        assert!(!report.disabled);
        assert!(report
            .warning
            .is_some_and(|text| text.contains("already restored")));
        let writes = f.writes;
        restore_fake(&mut f).unwrap();
        assert_eq!(f.writes, writes);
        assert!(f.undo.is_none());
    }
    #[test]
    fn forgetting_stale_undo_preserves_all_service_state() {
        let mut f = Fake::new(SERVICE_AUTO_START, SERVICE_RUNNING, false);
        disable_fake(&mut f).unwrap();
        f.current.start = SERVICE_DEMAND_START;
        let outside = f.current.clone();
        let writes = f.writes;
        let expected = f.expected();
        forget_transaction(&mut f, &expected).unwrap();
        assert_eq!(f.current, outside);
        assert_eq!(f.writes, writes);
        assert!(f.undo.is_none());
    }
    #[test]
    fn forgetting_owned_undo_is_blocked_without_service_writes() {
        let mut f = Fake::new(SERVICE_DEMAND_START, SERVICE_STOPPED, false);
        disable_fake(&mut f).unwrap();
        let writes = f.writes;
        let expected = f.expected();
        assert!(forget_transaction(&mut f, &expected).is_err());
        assert_eq!(f.writes, writes);
        assert!(f.undo.is_some());
    }
    #[test]
    fn reminder_timestamp_requires_an_owned_disabled_start_mode() {
        let mut f = Fake::new(SERVICE_AUTO_START, SERVICE_RUNNING, false);
        disable_fake(&mut f).unwrap();
        let value = snapshot(f.current.clone(), Ok(f.undo.clone()));
        assert!(value.disabled && value.since_disabled.is_some());
        f.current.start = SERVICE_DEMAND_START;
        let outside = snapshot(f.current.clone(), Ok(f.undo.clone()));
        assert!(!outside.disabled && outside.since_disabled.is_none());
        assert!(!outside.restore_available && outside.undo_available);
    }
    #[test]
    fn native_windows_service_status_read_is_non_mutating() {
        let value = query().expect("Windows Update service inventory is available");
        assert!(!value.start_mode.is_empty());
        assert!(!value.state.is_empty());
    }

    /// A registered fixture has a harmless fixed Windows command as its binary,
    /// but it is NEVER started. Production open() still hardcodes wuauserv.
    struct TemporaryService {
        _manager: ServiceHandle,
        service: ServiceHandle,
        name: String,
        deleted: bool,
    }
    impl TemporaryService {
        fn create() -> Result<Self, String> {
            let manager = unsafe {
                OpenSCManagerW(
                    std::ptr::null(),
                    std::ptr::null(),
                    SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
                )
            };
            if manager == 0 {
                return Err(error("Cannot create disposable SCM fixture"));
            }
            let manager = ServiceHandle(manager);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos();
            let name = format!("PCSweepTest-{}-{stamp}", std::process::id());
            let name_wide: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
            let binary = format!(
                "\"{}\" /d /c exit 0",
                platform::system_directory()?.join("cmd.exe").display()
            );
            let binary: Vec<_> = binary.encode_utf16().chain(Some(0)).collect();
            let service = unsafe {
                CreateServiceW(
                    manager.0,
                    name_wide.as_ptr(),
                    name_wide.as_ptr(),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_DEMAND_START,
                    SERVICE_ERROR_NORMAL,
                    binary.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            };
            if service == 0 {
                return Err(error("Cannot register disposable SCM fixture"));
            }
            Ok(Self {
                _manager: manager,
                service: ServiceHandle(service),
                name,
                deleted: false,
            })
        }
        fn remove(&mut self) -> Result<(), String> {
            if !self.deleted {
                if unsafe { DeleteService(self.service.0) } == 0 {
                    return Err(error("Cannot remove disposable SCM fixture"));
                }
                self.deleted = true;
            }
            Ok(())
        }
    }
    impl Drop for TemporaryService {
        fn drop(&mut self) {
            if let Err(e) = self.remove() {
                eprintln!("{e}");
            }
        }
    }
    struct NativeFixture {
        native: NativeBackend,
        undo: Option<Undo>,
    }
    impl Backend for NativeFixture {
        fn read(&mut self) -> Result<ServiceState, String> {
            self.native.read()
        }
        fn load(&mut self) -> Result<Option<Undo>, String> {
            Ok(self.undo.clone())
        }
        fn save(&mut self, undo: &Option<Undo>) -> Result<(), String> {
            self.undo = undo.clone();
            Ok(())
        }
        fn set_start(&mut self, start: u32, delayed: bool) -> Result<(), String> {
            assert!(
                self.undo.is_some(),
                "SCM transaction writes need an in-memory write-ahead record"
            );
            self.native.set_start(start, delayed)
        }
        fn stop(&mut self) -> Result<(), String> {
            panic!("the disposable service is never running or stopped through a control request")
        }
        fn start(&mut self) -> Result<(), String> {
            panic!("the disposable service must never be started")
        }
        fn wait(&mut self, state: u32, config: &ServiceState) -> Result<ServiceState, String> {
            self.native.wait(state, config)
        }
    }
    #[test]
    fn native_disposable_service_roundtrips_start_mode_and_delayed_flag_without_starting() {
        if !system::is_admin() {
            assert!(
                std::env::var_os("GITHUB_ACTIONS").is_none(),
                "the Windows CI runner must permit disposable SCM tests"
            );
            eprintln!("Skipping disposable SCM fixture: this local test process is not elevated.");
            return;
        }
        let mut fixture =
            TemporaryService::create().expect("create isolated disposable Windows service");
        {
            let native = NativeBackend::open_name(&fixture.name).unwrap();
            let mut backend = NativeFixture { native, undo: None };
            for (start, delayed) in [
                (SERVICE_DEMAND_START, false),
                (SERVICE_AUTO_START, false),
                (SERVICE_AUTO_START, true),
                (SERVICE_DEMAND_START, true),
            ] {
                // Fixture setup writes only our unique, stopped test service.
                backend.native.set_start(start, delayed).unwrap();
                let original = backend.read().unwrap();
                assert_eq!(
                    original,
                    ServiceState {
                        start,
                        delayed,
                        state: SERVICE_STOPPED
                    }
                );
                let reviewed = fingerprint(&original, &backend.undo).unwrap();
                disable_transaction(&mut backend, &reviewed, "2026-11-15T12:00:00Z".into())
                    .unwrap();
                assert_eq!(backend.read().unwrap(), original.disabled());
                let reviewed = fingerprint(&backend.read().unwrap(), &backend.undo).unwrap();
                restore_transaction(&mut backend, &reviewed).unwrap();
                assert_eq!(backend.read().unwrap(), original);
                assert!(backend.undo.is_none());
            }
            backend.native.set_start(SERVICE_DISABLED, false).unwrap();
            let original = backend.read().unwrap();
            let reviewed = fingerprint(&original, &None).unwrap();
            enable_transaction(&mut backend, &reviewed, "2026-11-16T00:00:00Z".into()).unwrap();
            assert_eq!(backend.read().unwrap().start, SERVICE_DEMAND_START);
            assert_eq!(backend.read().unwrap().state, SERVICE_STOPPED);
            let reviewed = fingerprint(&backend.read().unwrap(), &backend.undo).unwrap();
            restore_transaction(&mut backend, &reviewed).unwrap();
            assert_eq!(backend.read().unwrap(), original);
            assert!(backend.undo.is_none());
        }
        fixture
            .remove()
            .expect("remove disposable SCM service after test");
    }
}
