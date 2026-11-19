//! Recover Explorer in the signed-in session, leaving the elevated app
//! separate from the user's desktop. Reopen only verified desktop apps.
use crate::{desktop, platform, running, system, winfile};
use std::{
    io,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HANDLE,
    Security::{
        GetLengthSid, GetTokenInformation, TokenElevationType, TokenElevationTypeFull,
        TokenIsAppContainer, TokenUser, TOKEN_QUERY, TOKEN_USER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
    UI::WindowsAndMessaging::{FindWindowW, GetShellWindow, GetWindowThreadProcessId},
};

const HELPER_ARG: &str = "--pcsweep-explorer-recovery";

#[derive(Clone)]
pub struct Reopen {
    pub name: String,
    pub path: PathBuf,
    stamp: winfile::Stamp,
    user: Vec<u8>,
}

fn token(process: HANDLE) -> io::Result<running::Handle> {
    let mut token = 0;
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(running::Handle(token))
}

fn token_number(token: HANDLE, class: i32) -> io::Result<u32> {
    let mut value = 0u32;
    let mut length = 0;
    if unsafe {
        GetTokenInformation(
            token,
            class,
            (&mut value as *mut u32).cast(),
            4,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

fn token_user(token: HANDLE) -> io::Result<Vec<u8>> {
    let mut data = [0usize; 512];
    let mut bytes = 0;
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            data.as_mut_ptr().cast(),
            std::mem::size_of_val(&data) as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { (*data.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    Ok(unsafe { std::slice::from_raw_parts(sid.cast(), GetLengthSid(sid) as usize) }.to_vec())
}

fn shell_path() -> Result<PathBuf, String> {
    Ok(platform::windows_directory()?.join("explorer.exe"))
}

pub fn shell() -> Result<Option<running::Identity>, String> {
    let window = unsafe { GetShellWindow() };
    if window == 0 {
        return Ok(None);
    }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(window, &mut pid) } == 0 {
        return Ok(None);
    }
    let path = shell_path()?;
    let process = match running::Handle::open(pid, 0) {
        Ok(process) => process,
        Err(error) if error.raw_os_error() == Some(87) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Windows could not inspect its desktop shell. {error}"
            ))
        }
    };
    if running::session_id(pid) != Some(desktop::session()?)
        || !process
            .image(&mut [0; 32_768])
            .is_some_and(|image| winfile::same_path(&image, &path))
    {
        return Err("The current desktop is using a different shell.".into());
    }
    let created = process
        .times()
        .map_err(|e| format!("Could not identify this Explorer process. {e}"))?
        .0;
    Ok(Some(running::Identity { pid, created }))
}

fn tray_for(id: running::Identity) -> bool {
    let class = winfile::wide("Shell_TrayWnd");
    let window = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
    let mut pid = 0;
    if window != 0 {
        unsafe {
            GetWindowThreadProcessId(window, &mut pid);
        }
    }
    pid == id.pid
}

fn desktop_user() -> Option<Vec<u8>> {
    let id = shell().ok()??;
    let handle = running::checked_handle(id, 0).ok()??;
    token_user(token(handle.0).ok()?.0).ok()
}

pub fn remember(id: running::Identity, name: &str) -> Option<Reopen> {
    let session = desktop::session().ok()?;
    if running::session_id(id.pid) != Some(session) {
        return None;
    }
    let process = running::checked_handle(id, 0).ok()??;
    let file = process.image(&mut [0; 32_768])?;
    if file
        .extension()
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("exe"))
        || file
            .components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case("WindowsApps"))
    {
        return None;
    }
    let user = desktop_user()?;
    let process_token = token(process.0).ok()?;
    if token_user(process_token.0).ok()? != user
        || token_number(process_token.0, TokenIsAppContainer).ok()? != 0
    {
        return None;
    }
    let _dirs = winfile::lock_directories(file.parent()?).ok()?;
    let executable = winfile::read_locked(&file, 0).ok()?;
    Some(Reopen {
        name: name.to_string(),
        path: file,
        stamp: winfile::stamp(&executable).ok()?,
        user,
    })
}

pub enum RecoveryAction {
    Explorer { restart: bool },
    Reopen(Reopen),
}

pub fn recover(action: RecoveryAction) -> Result<String, String> {
    match action {
        RecoveryAction::Explorer { restart } => explorer(restart),
        RecoveryAction::Reopen(app) => reopen(app),
    }
}

fn reopen(app: Reopen) -> Result<String, String> {
    if desktop_user().as_ref() != Some(&app.user) {
        return Err("Open the signed-in Windows desktop before reopening this app.".into());
    }
    let dir = app
        .path
        .parent()
        .ok_or("The app’s folder is unavailable.")?;
    let _dirs = winfile::lock_directories(dir).map_err(|e| e.to_string())?;
    let file = winfile::read_locked(&app.path, 0).map_err(|e| e.to_string())?;
    if winfile::stamp(&file).map_err(|e| e.to_string())? != app.stamp {
        return Err("The executable changed. Use Run to open its current version.".into());
    }
    desktop::open(app.path.as_os_str(), "")?;
    Ok(format!(
        "Windows accepted the request to reopen {}.",
        app.name
    ))
}

fn wait_for_taskbar(old: Option<running::Identity>, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if shell()?.is_some_and(|id| Some(id) != old && tray_for(id)) {
            return Ok("Windows Explorer and the taskbar are ready.".into());
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    Err("Windows has not confirmed the taskbar is ready. Try Restore taskbar again.".into())
}

/// This code runs either at ordinary rights in the UI worker or in a helper
/// launched by Explorer/Task Scheduler before the old shell is stopped.
fn repair(old: Option<running::Identity>) -> Result<(), String> {
    desktop::session()?;
    let current_token = token(unsafe { GetCurrentProcess() }).map_err(|e| e.to_string())?;
    if token_number(current_token.0, TokenElevationType).map_err(|e| e.to_string())?
        == TokenElevationTypeFull as u32
    {
        return Err("Recovery must run under the signed-in desktop token.".into());
    }

    let current = shell()?;
    if current.is_some() && current != old {
        return Ok(());
    }
    let executable = shell_path()?;
    // Check the replacement is accessible before stopping anything.
    std::fs::File::open(&executable)
        .map_err(|e| format!("Could not read Windows Explorer. {e}"))?;
    if let Some(id) = current {
        let owner = desktop_user().ok_or("Could not verify Explorer's desktop account.")?;
        if owner != token_user(current_token.0).map_err(|e| e.to_string())? {
            return Err("The recovery helper is running under a different desktop account.".into());
        }
        if let Some(process) =
            running::request_end(id).map_err(|e| format!("Windows could not stop Explorer. {e}"))?
        {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !process.exited() {
                if Instant::now() >= deadline {
                    return Err(
                        "Explorer is still closing. Try Restore taskbar after it exits.".into(),
                    );
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        }
        // Windows may restart Explorer on its own.
        std::thread::sleep(Duration::from_millis(550));
    }
    if shell()?.is_none() {
        Command::new(&executable)
            .current_dir(platform::windows_directory()?)
            .spawn()
            .map_err(|e| format!("Windows could not start Explorer. {e}"))?;
    }
    Ok(())
}

fn explorer(restart: bool) -> Result<String, String> {
    let old = shell()?;
    if !restart {
        if let Some(id) = old {
            return if tray_for(id) {
                Ok("Windows Explorer and the taskbar are already running.".into())
            } else {
                Err(
                    "Explorer is open. Choose Restart Explorer to rebuild its missing taskbar."
                        .into(),
                )
            };
        }
    }
    let briefly = Duration::from_secs(10);
    if !system::is_admin() {
        repair(old)?;
        return wait_for_taskbar(old, briefly);
    }

    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let arguments = match old {
        Some(id) => format!("{HELPER_ARG} restart {} {}", id.pid, id.created),
        None => format!("{HELPER_ARG} restore"),
    };
    if old.is_some() && desktop::open(executable.as_os_str(), &arguments).is_ok() {
        if let Ok(message) = wait_for_taskbar(old, briefly) {
            return Ok(message);
        }
    }
    let session = desktop::session()?;
    desktop::repair_task(
        &executable,
        &arguments,
        &desktop::account(session)?,
        session,
        || wait_for_taskbar(old, briefly),
    )
}

fn helper_target(args: &[String]) -> Result<Option<running::Identity>, String> {
    match args {
        [action] if action == "restore" => Ok(None),
        [action, pid, created] if action == "restart" => Ok(Some(running::Identity {
            pid: pid
                .parse::<u32>()
                .ok()
                .filter(|n| *n > 4)
                .ok_or("Invalid Explorer PID.")?,
            created: created
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or("Invalid Explorer process identity.")?,
        })),
        _ => Err("Invalid Explorer recovery arguments.".into()),
    }
}

pub fn run_helper_mode() -> Option<i32> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_none_or(|arg| arg != HELPER_ARG) {
        return None;
    }
    match helper_target(&args[1..]).and_then(repair) {
        Ok(()) => Some(0),
        Err(error) => {
            crate::util::log(format!("Explorer recovery failed. {error}"));
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_can_only_request_the_fixed_shell_or_its_exact_previous_identity() {
        let args = |input: &[&str]| input.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(helper_target(&args(&["restore"])), Ok(None));
        assert_eq!(
            helper_target(&args(&["restart", "412", "781"])).unwrap(),
            Some(running::Identity {
                pid: 412,
                created: 781
            })
        );
        assert!(helper_target(&args(&["restart", "4", "781"])).is_err());
        assert!(helper_target(&args(&["restore", "cmd.exe"])).is_err());
    }
}
