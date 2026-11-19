//! Windows desktop Shell services shared by the launcher and recovery.
use crate::{running, system, winfile};
use std::{ffi::OsStr, io, path::Path};
use windows::{
    core::{Interface, BSTR, GUID, PWSTR, VARIANT},
    Win32::{
        Foundation::HANDLE,
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IDispatch,
                IServiceProvider, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED,
            },
            TaskScheduler::*,
        },
        UI::{
            Shell::{
                BHID_EnumItems, IEnumShellItems, IShellBrowser, IShellDispatch2,
                IShellFolderViewDual, IShellItem, IShellWindows, SHGetKnownFolderItem,
                SID_STopLevelBrowser, ShellWindows, CSIDL_DESKTOP, KF_FLAG_DEFAULT,
                SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
            },
            WindowsAndMessaging::GetWindowThreadProcessId,
        },
    },
};

pub struct Apartment;

impl Apartment {
    pub fn enter() -> windows::core::Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn owned_string(raw: PWSTR) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    let text = unsafe { raw.to_string() }.ok();
    unsafe {
        CoTaskMemFree(Some(raw.0.cast()));
    }
    text
}

pub fn property_key(
    name: &str,
) -> Option<windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY> {
    use windows::{
        core::PCWSTR,
        Win32::UI::Shell::PropertiesSystem::{PSGetPropertyKeyFromName, PROPERTYKEY},
    };
    let mut key = PROPERTYKEY::default();
    unsafe { PSGetPropertyKeyFromName(PCWSTR(winfile::wide(name).as_ptr()), &mut key) }.ok()?;
    Some(key)
}

pub fn items(folder: &GUID, cap: usize) -> windows::core::Result<(Vec<IShellItem>, bool)> {
    let root: IShellItem =
        unsafe { SHGetKnownFolderItem(folder, KF_FLAG_DEFAULT, HANDLE::default())? };
    let iter: IEnumShellItems = unsafe { root.BindToHandler(None, &BHID_EnumItems)? };
    let mut result = Vec::new();
    loop {
        let mut chunk = [const { None }; 32];
        let mut count = 0;
        unsafe {
            iter.Next(&mut chunk, Some(&mut count))?;
        }
        for item in chunk.into_iter().flatten() {
            if result.len() == cap {
                return Ok((result, true));
            }
            result.push(item);
        }
        if count < 32 {
            return Ok((result, false));
        }
    }
}

fn bstr(value: &OsStr) -> windows::core::Result<BSTR> {
    use std::os::windows::ffi::OsStrExt;
    BSTR::from_wide(&value.encode_wide().collect::<Vec<_>>())
}

fn explorer_dispatch() -> Result<IShellDispatch2, String> {
    // Microsoft's Execute In Explorer pattern. Resolve the desktop's own
    // automation object, rather than creating a Shell object in our process.
    let shell = crate::recovery::shell()?
        .ok_or("The Windows desktop is unavailable. Restore the taskbar first.")?;
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)
            .map_err(|e| format!("Could not connect to Windows Explorer. {e}"))?;
        let mut hwnd = 0;
        let automation = windows
            .FindWindowSW(
                &VARIANT::from(CSIDL_DESKTOP as i32),
                &VARIANT::default(),
                SWC_DESKTOP,
                &mut hwnd,
                SWFO_NEEDDISPATCH,
            )
            .map_err(|e| format!("Could not find the Windows desktop. {e}"))?;
        let mut pid = 0;
        use windows::Win32::Foundation::HWND;
        GetWindowThreadProcessId(HWND((hwnd as u32 as usize) as *mut _), Some(&mut pid));
        if pid != shell.pid {
            return Err("The desktop's Windows Explorer process changed. Try again.".into());
        }
        let provider = automation
            .cast::<IServiceProvider>()
            .map_err(|e| e.to_string())?;
        let browser: IShellBrowser = provider
            .QueryService(&SID_STopLevelBrowser)
            .map_err(|e| e.to_string())?;
        let view = browser.QueryActiveShellView().map_err(|e| e.to_string())?;
        let dispatch: IDispatch = view
            .GetItemObject(SVGIO_BACKGROUND)
            .map_err(|e| e.to_string())?;
        let folder: IShellFolderViewDual = dispatch.cast().map_err(|e| e.to_string())?;
        folder
            .Application()
            .and_then(|app| app.cast())
            .map_err(|e| e.to_string())
    }
}

pub fn open(file: &OsStr, arguments: &str) -> Result<(), String> {
    let _apartment =
        Apartment::enter().map_err(|e| format!("Windows Shell initialization failed. {e}"))?;
    if system::is_admin() {
        let shell = explorer_dispatch()?;
        let file = bstr(file).map_err(|e| e.to_string())?;
        unsafe {
            shell.ShellExecute(
                &file,
                &VARIANT::from(arguments),
                &VARIANT::default(),
                &VARIANT::from("open"),
                &VARIANT::from(1i32),
            )
        }
        .map_err(|e| format!("Windows could not open this item from the desktop. {e}"))
    } else {
        use windows_sys::Win32::UI::{
            Shell::{
                ShellExecuteExW, SEE_MASK_DOENVSUBST, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
                SHELLEXECUTEINFOW,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        };
        let file = winfile::wide(file);
        let arguments = winfile::wide(arguments);
        let mut launch = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI | SEE_MASK_DOENVSUBST,
            lpFile: file.as_ptr(),
            lpParameters: arguments.as_ptr(),
            nShow: SW_SHOWNORMAL,
            ..unsafe { std::mem::zeroed() }
        };
        if unsafe { ShellExecuteExW(&mut launch) } == 0 {
            Err(format!(
                "Windows could not open this item. {}",
                io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }
}

/// Create a triggerless, limited task for a single Explorer repair. The
/// payload is always PCSweep's fixed recovery helper. General Run commands
/// never use Task Scheduler or a privileged launch path.
pub fn repair_task(
    executable: &Path,
    arguments: &str,
    account: &str,
    session: u32,
    verify: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    use windows::Win32::{
        Foundation::{VARIANT_FALSE, VARIANT_TRUE},
        System::Com::CLSCTX_INPROC_SERVER,
    };
    let _apartment = Apartment::enter().map_err(|e| e.to_string())?;
    let name = BSTR::from(format!(
        "PCSweep-Explorer-{}",
        crate::update_install::nonce()?
    ));
    let account = BSTR::from(account);
    let (folder, task) = (|| -> windows::core::Result<_> {
        unsafe {
            let service: ITaskService =
                CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
            let empty = VARIANT::default();
            service.Connect(&empty, &empty, &empty, &empty)?;
            let folder = service.GetFolder(&BSTR::from("\\"))?;
            let definition = service.NewTask(0)?;
            let principal = definition.Principal()?;
            principal.SetUserId(&account)?;
            principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)?;
            principal.SetRunLevel(TASK_RUNLEVEL_LUA)?;
            let settings = definition.Settings()?;
            settings.SetAllowDemandStart(VARIANT_TRUE)?;
            settings.SetDisallowStartIfOnBatteries(VARIANT_FALSE)?;
            settings.SetStopIfGoingOnBatteries(VARIANT_FALSE)?;
            settings.SetExecutionTimeLimit(&BSTR::from("PT25S"))?;
            let action: IExecAction = definition.Actions()?.Create(TASK_ACTION_EXEC)?.cast()?;
            action.SetPath(&bstr(executable.as_os_str())?)?;
            action.SetArguments(&BSTR::from(arguments))?;
            let task = folder.RegisterTaskDefinition(
                &name,
                &definition,
                TASK_CREATE.0,
                &VARIANT::from(account.clone()),
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )?;
            Ok((folder, task))
        }
    })()
    .map_err(|e| format!("Could not prepare Explorer recovery in the signed-in session. {e}"))?;
    let result = unsafe {
        task.RunEx(
            &VARIANT::default(),
            TASK_RUN_USE_SESSION_ID.0,
            session as i32,
            &BSTR::new(),
        )
    }
    .map_err(|e| format!("Windows could not start the recovery helper. {e}"))
    .and_then(|_| verify());
    if let Err(error) = unsafe { folder.DeleteTask(&name, 0) } {
        crate::util::log(format!(
            "Windows could not remove the one-time Explorer recovery task. {error}"
        ));
    }
    result
}

pub fn session() -> Result<u32, String> {
    running::session_id(std::process::id())
        .filter(|session| *session != 0)
        .ok_or_else(|| "A signed-in Windows desktop is required.".into())
}

pub fn account(session: u32) -> Result<String, String> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSDomainName, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName,
    };
    let value = |kind| {
        let mut ptr = std::ptr::null_mut();
        let mut bytes = 0;
        if unsafe { WTSQuerySessionInformationW(0, session, kind, &mut ptr, &mut bytes) } == 0 {
            return Err(format!(
                "Windows could not identify the signed-in account. {}",
                io::Error::last_os_error()
            ));
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
    let domain = value(WTSDomainName)?;
    if user.is_empty() {
        return Err("The signed-in desktop account is unavailable.".into());
    }
    Ok(if domain.is_empty() {
        user
    } else {
        format!(r"{domain}\{user}")
    })
}
