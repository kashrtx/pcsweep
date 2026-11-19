//! Restore the desktop shell and reopen eligible desktop apps under the
//! interactive user's ordinary token. Never replay a process command line.
use crate::{platform, running, system, winfile};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::HANDLE,
    Security::{
        DuplicateTokenEx, GetLengthSid, GetTokenInformation, LookupAccountNameW,
        SecurityImpersonation, TokenElevation, TokenIsAppContainer, TokenLinkedToken, TokenPrimary,
        TokenSessionId, TokenUser, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::SYNCHRONIZE,
    System::{
        Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
        RemoteDesktop::{WTSDomainName, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName},
        Threading::{
            CreateProcessWithTokenW, GetCurrentProcess, OpenProcessToken,
            CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTUPINFOW,
        },
    },
    UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId},
};

#[derive(Clone)]
pub struct Reopen {
    pub name: String,
    pub path: PathBuf,
    stamp: winfile::Stamp,
    user: Vec<u8>,
}

struct Desktop {
    token: running::Handle,
    session: u32,
    user: Vec<u8>,
}

static DESKTOP: Mutex<Option<Arc<Desktop>>> = Mutex::new(None);

fn process_token(process: HANDLE, duplicate: bool) -> io::Result<running::Handle> {
    let mut token = 0;
    let access = TOKEN_QUERY | if duplicate { TOKEN_DUPLICATE } else { 0 };
    if unsafe { OpenProcessToken(process, access, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(running::Handle(token))
}

fn token_value<T: Default>(token: HANDLE, class: i32) -> io::Result<T> {
    let mut value = T::default();
    let mut length = 0;
    if unsafe {
        GetTokenInformation(
            token,
            class,
            (&mut value as *mut T).cast(),
            std::mem::size_of::<T>() as u32,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

fn token_user(token: HANDLE) -> io::Result<Vec<u8>> {
    let mut bytes = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut bytes);
    }
    if bytes == 0 || bytes > 4096 {
        return Err(io::Error::other(
            "The desktop user could not be identified.",
        ));
    }
    let mut data = [0usize; 512];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            data.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { (*data.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    Ok(unsafe { std::slice::from_raw_parts(sid.cast(), GetLengthSid(sid) as usize) }.to_vec())
}

fn same_session() -> io::Result<u32> {
    running::session_id(std::process::id())
        .filter(|session| *session != 0)
        .ok_or_else(|| io::Error::other("A signed-in Windows desktop is required."))
}

fn shell_path() -> io::Result<PathBuf> {
    Ok(platform::windows_directory()
        .map_err(io::Error::other)?
        .join("explorer.exe"))
}

fn shell() -> io::Result<Option<running::Identity>> {
    let window = unsafe { GetShellWindow() };
    if window == 0 {
        return Ok(None);
    }
    let mut pid = 0;
    if unsafe { GetWindowThreadProcessId(window, &mut pid) } == 0 {
        return Ok(None);
    }
    let process = running::Handle::open(pid, SYNCHRONIZE)?;
    if running::session_id(pid) != Some(same_session()?)
        || !process
            .image(&mut [0; 32_768])
            .is_some_and(|path| shell_path().is_ok_and(|shell| winfile::same_path(&path, &shell)))
    {
        return Err(io::Error::other(
            "The current desktop shell could not be verified as Windows Explorer.",
        ));
    }
    Ok(Some(running::Identity {
        pid,
        created: process.times()?.0,
    }))
}

fn ordinary_desktop(source: HANDLE, session: u32) -> io::Result<Desktop> {
    if token_value::<u32>(source, TokenElevation)? != 0
        || token_value::<u32>(source, TokenSessionId)? != session
    {
        return Err(io::Error::other(
            "Windows did not provide an ordinary token for this desktop.",
        ));
    }
    let user = token_user(source)?;
    let mut token = 0;
    if unsafe {
        DuplicateTokenEx(
            source,
            TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(Desktop {
        token: running::Handle(token),
        session,
        user,
    })
}

fn session_user(session: u32) -> io::Result<Vec<u8>> {
    let value = |kind| {
        let mut ptr = std::ptr::null_mut();
        let mut bytes = 0;
        if unsafe { WTSQuerySessionInformationW(0, session, kind, &mut ptr, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let text = if ptr.is_null() {
            String::new()
        } else {
            unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(ptr, bytes as usize / 2)) }
        };
        unsafe {
            WTSFreeMemory(ptr.cast());
        }
        Ok(text.trim_end_matches('\0').to_string())
    };
    let user = value(WTSUserName)?;
    if user.is_empty() {
        return Err(io::Error::other(
            "The signed-in desktop account is unavailable.",
        ));
    }
    let domain = value(WTSDomainName)?;
    let name = if domain.is_empty() {
        user
    } else {
        format!(r"{domain}\{user}")
    };
    let mut sid = [0usize; 16];
    let mut sid_size = std::mem::size_of_val(&sid) as u32;
    let mut domain = [0u16; 512];
    let mut domain_size = domain.len() as u32;
    let mut kind = 0;
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            winfile::wide(name).as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_size,
            domain.as_mut_ptr(),
            &mut domain_size,
            &mut kind,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { std::slice::from_raw_parts(sid.as_ptr().cast(), sid_size as usize) }.to_vec())
}

fn desktop() -> io::Result<Arc<Desktop>> {
    let session = same_session()?;
    let mut saved = DESKTOP
        .lock()
        .map_err(|_| io::Error::other("Desktop recovery unavailable."))?;
    if let Ok(Some(id)) = shell() {
        if let Ok(Some(process)) = running::checked_handle(id, 0) {
            if let Ok(token) = process_token(process.0, true) {
                if let Ok(value) = ordinary_desktop(token.0, session) {
                    let value = Arc::new(value);
                    *saved = Some(value.clone());
                    return Ok(value);
                }
            }
        }
    }
    if let Some(value) = saved.as_ref().filter(|value| value.session == session) {
        return Ok(value.clone());
    }
    let current = process_token(unsafe { GetCurrentProcess() }, true)?;
    let linked;
    let token = if system::is_admin() {
        linked = running::Handle(token_value::<HANDLE>(current.0, TokenLinkedToken)?);
        linked.0
    } else {
        current.0
    };
    // A UAC credential prompt can be for a different administrator. A linked
    // token is eligible only for the account actually signed into this desktop.
    if token_user(token)? != session_user(session)? {
        return Err(io::Error::other(
            "The signed-in desktop user's token is unavailable.",
        ));
    }
    let value = Arc::new(ordinary_desktop(token, session)?);
    *saved = Some(value.clone());
    Ok(value)
}

pub fn warm_desktop() {
    if system::is_admin() {
        std::thread::spawn(|| {
            let _ = desktop();
        });
    }
}

fn launch(path: &Path, desktop: &Desktop) -> io::Result<()> {
    let directory = platform::system_directory().map_err(io::Error::other)?;
    let current = process_token(unsafe { GetCurrentProcess() }, false)?;
    if !system::is_admin() && token_user(current.0)? == desktop.user {
        return std::process::Command::new(path)
            .current_dir(directory)
            .spawn()
            .map(|_| ());
    }
    let application = winfile::wide(path);
    use std::os::windows::ffi::OsStrExt;
    let mut arguments: Vec<_> = std::iter::once(34)
        .chain(path.as_os_str().encode_wide())
        .chain([34, 0])
        .collect();
    let mut environment = std::ptr::null_mut();
    if unsafe { CreateEnvironmentBlock(&mut environment, desktop.token.0, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut desktop_name = winfile::wide(r"winsta0\default");
    let start = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpDesktop: desktop_name.as_mut_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        CreateProcessWithTokenW(
            desktop.token.0,
            0,
            application.as_ptr(),
            arguments.as_mut_ptr(),
            CREATE_UNICODE_ENVIRONMENT,
            environment,
            winfile::wide(directory).as_ptr(),
            &start,
            &mut process,
        )
    };
    let result = if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        let _handles = (
            running::Handle(process.hProcess),
            running::Handle(process.hThread),
        );
        Ok(())
    };
    unsafe {
        DestroyEnvironmentBlock(environment);
    }
    result
}

pub fn remember(id: running::Identity, name: &str) -> Option<Reopen> {
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
    let desktop = desktop().ok()?;
    if running::session_id(id.pid) != Some(desktop.session) {
        return None;
    }
    let token = process_token(process.0, false).ok()?;
    if token_user(token.0).ok()? != desktop.user
        || token_value::<u32>(token.0, TokenIsAppContainer).ok()? != 0
    {
        return None;
    }
    let _dirs = winfile::lock_directories(file.parent()?).ok()?;
    let executable = winfile::read_locked(&file, 0).ok()?;
    Some(Reopen {
        name: name.to_string(),
        path: file,
        stamp: winfile::stamp(&executable).ok()?,
        user: desktop.user.clone(),
    })
}

/// Keep the desktop token even if the shell is ended through the process list.
pub fn before_end(id: running::Identity) {
    if shell().is_ok_and(|shell| shell == Some(id)) {
        let _ = desktop();
    }
}

pub enum RecoveryAction {
    Explorer { restart: bool },
    Reopen(Reopen),
}

pub fn recover(action: RecoveryAction) -> Result<String, String> {
    match action {
        RecoveryAction::Explorer { restart } => {
            explorer(restart).map_err(|error| error.to_string())
        }
        RecoveryAction::Reopen(app) => reopen(app).map_err(|error| error.to_string()),
    }
}

fn reopen(app: Reopen) -> io::Result<String> {
    let desktop = desktop()?;
    if desktop.user != app.user {
        return Err(io::Error::other("The active desktop account changed."));
    }
    let dir = app
        .path
        .parent()
        .ok_or_else(|| io::Error::other("The app's folder is unavailable."))?;
    let _dirs = winfile::lock_directories(dir)?;
    let file = winfile::read_locked(&app.path, 0)?;
    if winfile::stamp(&file)? != app.stamp {
        return Err(io::Error::other(
            "The app executable changed. Use its Start menu shortcut to open the current version.",
        ));
    }
    launch(&app.path, &desktop)?;
    Ok(format!(
        "Windows accepted the request to reopen {}.",
        app.name
    ))
}

fn explorer(restart: bool) -> io::Result<String> {
    let old = shell()?;
    if !restart && old.is_some() {
        return Ok("Windows Explorer is already running.".into());
    }
    let desktop = desktop()?;
    if let Some(id) = old {
        if let Some(process) = running::request_end(id).map_err(io::Error::other)? {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !process.exited() {
                if Instant::now() >= deadline {
                    return Err(io::Error::other(
                        "Windows Explorer is still closing. Try Restore taskbar in a moment.",
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        // Windows may be configured to restart its own shell.
        std::thread::sleep(Duration::from_millis(600));
    }
    if !shell().is_ok_and(|id| id.is_some() && id != old) {
        launch(&shell_path()?, &desktop)?;
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if shell().is_ok_and(|id| id.is_some() && id != old) {
            return Ok("Windows Explorer and the taskbar are back.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok("Windows accepted the Explorer launch. The desktop shell is still starting.".into())
}
