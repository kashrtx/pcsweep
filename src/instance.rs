//! One UI process per Windows user/session. Activation carries no executable
//! path, commands, or pointers across integrity boundaries.
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*, Security::*, System::Threading::*, UI::WindowsAndMessaging::*,
};
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn identity() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| unsafe {
        let mut token = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return "unavailable".into();
        }
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        let mut storage = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        if ok == 0 {
            return "unavailable".into();
        }
        let user = &*(storage.as_ptr() as *const TOKEN_USER);
        std::slice::from_raw_parts(
            user.User.Sid.cast::<u8>(),
            GetLengthSid(user.User.Sid) as usize,
        )
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
    })
}
pub fn activation_title() -> String {
    format!("PCSweep activation {}", identity())
}
pub fn activate_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(wide("PCSweep.Activate.v1").as_ptr()) })
}
pub struct Guard(HANDLE);
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub fn acquire() -> Result<Option<Guard>, String> {
    if identity() == "unavailable" || activate_message() == 0 {
        return Err(
            "Windows could not identify this PCSweep session. Please try launching again.".into(),
        );
    }

    acquire_named(&format!("Local\\PCSweep.UI.{}", identity()), true)
}
fn acquire_named(name: &str, activate: bool) -> Result<Option<Guard>, String> {
    let name = wide(name);
    // SYNCHRONIZE is enough to detect an elevated instance from a normal one.
    let existing = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    let (handle, duplicate) = if existing != 0 {
        (existing, true)
    } else {
        let handle = unsafe {
            SetLastError(0);
            CreateMutexW(std::ptr::null(), 0, name.as_ptr())
        };
        let error = unsafe { GetLastError() };
        if handle == 0 {
            return Err(format!(
                "Could not establish PCSweep's single-instance lock: {}",
                std::io::Error::from_raw_os_error(error as i32)
            ));
        }
        (handle, error == ERROR_ALREADY_EXISTS)
    };
    if !duplicate {
        return Ok(Some(Guard(handle)));
    }
    unsafe {
        CloseHandle(handle);
    }
    if activate {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let hwnd = unsafe { FindWindowW(std::ptr::null(), wide(&activation_title()).as_ptr()) };
            if hwnd != 0 {
                unsafe {
                    let mut pid = 0;
                    GetWindowThreadProcessId(hwnd, &mut pid);
                    AllowSetForegroundWindow(pid);
                    PostMessageW(hwnd, activate_message(), 0, 0);
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(
                    "PCSweep is already starting or closing. Please try opening it again shortly."
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(None)
}
fn is_handoff_arg(arg: &str) -> bool {
    matches!(arg, "--wait-for-parent" | "--pcsweep-update-ack")
}
/// Elevation is an opt-in preference, always using Windows' normal UAC prompt.
pub fn elevate_on_start() -> bool {
    if crate::system::is_admin() || std::env::args().any(|s| is_handoff_arg(&s)) {
        return false;
    }
    let enabled = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\PCSweep\Session")
        .and_then(|k| k.get_value::<u32, _>("StartElevated"))
        .ok()
        == Some(1);
    enabled && crate::system::relaunch_as_admin()
}
pub fn wait_for_parent() {
    let args: Vec<_> = std::env::args().collect();
    let Some(at) = args.iter().position(|s| s == "--wait-for-parent") else {
        return;
    };
    let Some(pid) = args
        .get(at + 1)
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
    else {
        return;
    };
    unsafe {
        let process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZATION_SYNCHRONIZE,
            0,
            pid,
        );
        if process == 0 {
            return;
        }
        let mut path = vec![0u16; 32768];
        let mut size = path.len() as u32;
        let same = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut size) != 0
            && std::env::current_exe().ok().is_some_and(|exe| {
                exe.to_string_lossy()
                    .eq_ignore_ascii_case(&String::from_utf16_lossy(&path[..size as usize]))
            });
        if same {
            WaitForSingleObject(process, 30_000);
        }
        CloseHandle(process);
    }
}
pub fn show_error(error: &str) {
    unsafe {
        MessageBoxW(
            0,
            wide(error).as_ptr(),
            wide("PCSweep").as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_lock_excludes_duplicates_and_releases_after_exit() {
        let name = format!("Local\\PCSweep.Test.{}", std::process::id());
        let first = acquire_named(&name, false).unwrap().unwrap();
        assert!(acquire_named(&name, false).unwrap().is_none());
        drop(first);
        assert!(acquire_named(&name, false).unwrap().is_some());
        assert_eq!(activation_title(), activation_title());
        assert_ne!(activate_message(), 0);
    }
    #[test]
    fn update_acknowledgment_must_not_be_lost_to_an_elevation_relaunch() {
        assert!(is_handoff_arg("--pcsweep-update-ack"));
        assert!(is_handoff_arg("--wait-for-parent"));
        assert!(!is_handoff_arg("PCSweep.exe"));
    }
}
