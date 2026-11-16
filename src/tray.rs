//! Optional notification-area presence. A private hidden window and its message
//! loop belong to one helper thread; the eframe window procedure is untouched.
use eframe::egui;
use std::{
    cell::Cell,
    mem::{size_of, zeroed},
    sync::{
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread::JoinHandle,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::*, WindowsAndMessaging::*},
};

const CALLBACK: u32 = WM_APP + 79;
const STOP: u32 = WM_APP + 80;
const RETRY_TIMER: usize = 1;
// WinSDK defines NIN_KEYSELECT as a macro; windows-sys exposes its components.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
const ICON_ID: u32 = 1;
const SHOW: u32 = 101;
const CHECK_UPDATES: u32 = 102;
const QUIT: u32 = 103;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Show,
    CheckUpdates,
    Quit,
    Ready,
    Failed(String),
}
#[derive(Default)]
struct Shared {
    hwnd: AtomicIsize,
    main_hwnd: AtomicIsize,
    ready: AtomicBool,
    stop: AtomicBool,
}
#[derive(Clone)]
struct Notifier {
    events: Sender<TrayEvent>,
    ctx: egui::Context,
    shared: Arc<Shared>,
}
impl Notifier {
    fn send(&self, event: TrayEvent) {
        let wake_window = matches!(
            event,
            TrayEvent::Show | TrayEvent::CheckUpdates | TrayEvent::Quit | TrayEvent::Failed(_)
        );
        if self.events.send(event).is_ok() {
            if wake_window {
                // Hidden/occluded winit windows may not receive RedrawRequested.
                // Restore natively first; merely requesting an egui repaint can
                // leave every tray command waiting for a frame that never runs.
                wake_main(self.shared.main_hwnd.load(Ordering::Acquire));
            }
            self.ctx.request_repaint();
        }
    }
}

pub struct Tray {
    shared: Arc<Shared>,
    events: Receiver<TrayEvent>,
    finished: Receiver<()>,
    worker: Option<JoinHandle<()>>,
}
impl Tray {
    pub fn start(ctx: &egui::Context) -> Self {
        let shared = Arc::new(Shared::default());
        let (events, receiver) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let notifier = Notifier {
            events,
            ctx: ctx.clone(),
            shared: shared.clone(),
        };
        let thread_notifier = notifier.clone();
        let thread_shared = Arc::clone(&shared);
        let worker = match std::thread::Builder::new()
            .name("pcsweep-tray".into())
            .spawn(move || {
                let result = run_window(Arc::clone(&thread_shared), &thread_notifier, true);
                thread_shared.ready.store(false, Ordering::Release);
                thread_shared.hwnd.store(0, Ordering::Release);
                if let Err(e) = result {
                    if !thread_shared.stop.load(Ordering::Acquire) {
                        thread_notifier.send(TrayEvent::Failed(e));
                    }
                }
                let _ = done.send(());
            }) {
            Ok(worker) => Some(worker),
            Err(e) => {
                notifier.send(TrayEvent::Failed(format!(
                    "PCSweep could not start its notification-area helper: {e}"
                )));
                None
            }
        };
        Self {
            shared,
            events: receiver,
            finished,
            worker,
        }
    }
    pub fn bind_window(&self, hwnd: HWND) {
        self.shared.main_hwnd.store(hwnd, Ordering::Release);
    }
    pub fn available(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire) && self.shared.hwnd.load(Ordering::Acquire) != 0
    }
    pub fn poll(&mut self) -> Vec<TrayEvent> {
        self.events.try_iter().collect()
    }
    #[cfg(test)]
    pub fn inert() -> Self {
        let (_, events) = mpsc::channel();
        let (_, finished) = mpsc::channel();
        Self {
            shared: Arc::new(Shared::default()),
            events,
            finished,
            worker: None,
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let hwnd = self.shared.hwnd.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                PostMessageW(hwnd, STOP, 0, 0);
            }
        }
        // The normal path removes the icon before the process leaves main().
        // An unresponsive Shell must never hold the app shutdown indefinitely.
        if self.worker.is_some() {
            let _ = self.finished.recv_timeout(Duration::from_millis(250));
            if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
            }
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn failure(action: &str) -> String {
    format!("{action}: {}", std::io::Error::last_os_error())
}
struct Icon(isize);
impl Drop for Icon {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                DestroyIcon(self.0);
            }
        }
    }
}
struct Menu(isize);
impl Drop for Menu {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                DestroyMenu(self.0);
            }
        }
    }
}
struct Class {
    name: Vec<u16>,
    module: isize,
}
impl Drop for Class {
    fn drop(&mut self) {
        unsafe {
            UnregisterClassW(self.name.as_ptr(), self.module);
        }
    }
}

/// Select a bounded ICO resource without allowing offsets into its directory or
/// outside the embedded data. Windows decodes PNG/DIB icon resources natively.
fn icon_resource(bytes: &[u8], desired: usize) -> Option<&[u8]> {
    if bytes.len() < 6 || bytes[..4] != [0, 0, 1, 0] {
        return None;
    }
    let count = u16::from_le_bytes(bytes[4..6].try_into().ok()?) as usize;
    if count == 0 || count > 64 {
        return None;
    }
    let directory_end = 6usize.checked_add(count.checked_mul(16)?)?;
    if directory_end > bytes.len() {
        return None;
    }
    let mut best = None;
    for entry in bytes[6..directory_end].chunks_exact(16) {
        let width = if entry[0] == 0 {
            256
        } else {
            entry[0] as usize
        };
        let height = if entry[1] == 0 {
            256
        } else {
            entry[1] as usize
        };
        let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
        let Some(end) = offset.checked_add(length) else {
            continue;
        };
        if offset < directory_end || length == 0 || end > bytes.len() {
            continue;
        }
        let depth = u16::from_le_bytes(entry[6..8].try_into().ok()?) as usize;
        let score = (
            width.abs_diff(desired) + height.abs_diff(desired),
            usize::MAX - depth,
        );
        if best.as_ref().is_none_or(|(old, _)| score < *old) {
            best = Some((score, &bytes[offset..end]));
        }
    }
    best.map(|(_, resource)| resource)
}
fn load_icon() -> Result<Icon, String> {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 64);
    let bytes = icon_resource(include_bytes!("../assets/icon.ico"), size as usize)
        .ok_or("PCSweep's embedded notification-area icon is invalid.")?;
    let icon = unsafe {
        CreateIconFromResourceEx(
            bytes.as_ptr(),
            bytes.len() as u32,
            1,
            0x0003_0000,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    };
    if icon == 0 {
        return Err(failure(
            "Windows could not decode PCSweep's notification-area icon",
        ));
    }
    Ok(Icon(icon))
}

struct Window {
    notifier: Notifier,
    shared: Arc<Shared>,
    icon: Icon,
    taskbar_created: u32,
    installed: Cell<bool>,
    version4: Cell<bool>,
    publish_icon: bool,
    closing: Cell<bool>,
}
impl Window {
    fn data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = unsafe { zeroed() };
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = ICON_ID;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = CALLBACK;
        data.hIcon = self.icon.0;
        for (slot, ch) in data
            .szTip
            .iter_mut()
            .zip("PCSweep — keep your PC clean and quick".encode_utf16())
        {
            *slot = ch;
        }
        data
    }
    fn add(&self, hwnd: HWND) -> Result<(), String> {
        let mut data = self.data(hwnd);
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
            self.shared.ready.store(false, Ordering::Release);
            return Err(failure("Windows notification area is unavailable; PCSweep will stay accessible as a window"));
        }
        self.installed.set(true);
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        self.version4
            .set(unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) } != 0);
        self.shared.ready.store(true, Ordering::Release);
        self.notifier.send(TrayEvent::Ready);
        Ok(())
    }
    fn remove(&self, hwnd: HWND) {
        self.shared.ready.store(false, Ordering::Release);
        if self.installed.replace(false) {
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &self.data(hwnd));
            }
        }
    }
    fn menu(&self, hwnd: HWND) {
        let menu = Menu(unsafe { CreatePopupMenu() });
        if menu.0 == 0 {
            self.notifier.send(TrayEvent::Failed(failure(
                "Cannot open PCSweep's notification-area menu",
            )));
            return;
        }
        for (id, label) in [
            (SHOW, "Show PCSweep"),
            (CHECK_UPDATES, "Check for updates"),
            (QUIT, "Quit PCSweep"),
        ] {
            let label = wide(label);
            if unsafe { AppendMenuW(menu.0, MF_STRING, id as usize, label.as_ptr()) } == 0 {
                return;
            }
        }
        unsafe {
            SetMenuDefaultItem(menu.0, SHOW, 0);
        }
        let mut point: POINT = unsafe { zeroed() };
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return;
        }
        unsafe {
            SetForegroundWindow(hwnd);
        }
        let command = unsafe {
            TrackPopupMenu(
                menu.0,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null(),
            )
        } as u32;
        if let Some(event) = menu_event(command) {
            self.notifier.send(event);
        }
        // Required for reliable dismissal when the user clicks outside a tray
        // menu; this helper owns no visible or taskbar window.
        if unsafe { IsWindow(hwnd) } != 0 {
            unsafe {
                PostMessageW(hwnd, WM_NULL, 0, 0);
            }
        }
    }
}
fn menu_event(id: u32) -> Option<TrayEvent> {
    match id {
        SHOW => Some(TrayEvent::Show),
        CHECK_UPDATES => Some(TrayEvent::CheckUpdates),
        QUIT => Some(TrayEvent::Quit),
        _ => None,
    }
}
fn is_show_message(message: u32) -> bool {
    matches!(
        message,
        WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | NIN_KEYSELECT
    )
}
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_message(hwnd, message, wparam, lparam)
    }))
    .unwrap_or_else(|_| DefWindowProcW(hwnd, message, wparam, lparam))
}
unsafe fn handle_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Window;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let state = &*pointer;
    if message == crate::instance::activate_message() {
        state.notifier.send(TrayEvent::Show);
        return 0;
    }
    if message == state.taskbar_created && state.publish_icon {
        // Explorer has discarded all old icons. Re-add this instance with the
        // same callback protocol, or signal failure so a hidden app can re-open.
        state.installed.set(false);
        state.shared.ready.store(false, Ordering::Release);
        if let Err(e) = state.add(hwnd) {
            state.notifier.send(TrayEvent::Failed(e));
            SetTimer(hwnd, RETRY_TIMER, 2000, None);
        }
        return 0;
    }
    match message {
        CALLBACK => {
            let callback = if state.version4.get() {
                (lparam as u32) & 0xffff
            } else {
                lparam as u32
            };
            let id = if state.version4.get() {
                ((lparam as u32) >> 16) & 0xffff
            } else {
                wparam as u32
            };
            if id != ICON_ID {
                return 0;
            }
            if is_show_message(callback) {
                state.notifier.send(TrayEvent::Show);
            } else if (state.version4.get() && callback == WM_CONTEXTMENU)
                || (!state.version4.get() && callback == WM_RBUTTONUP)
            {
                state.menu(hwnd);
            }
            0
        }
        WM_CLOSE | STOP => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            state.remove(hwnd);
            state.shared.hwnd.store(0, Ordering::Release);
            if !state.shared.stop.load(Ordering::Acquire) && !state.closing.get() {
                state.notifier.send(TrayEvent::Failed("PCSweep's notification-area helper closed. The main window must remain available.".into()));
            }
            PostQuitMessage(0);
            0
        }
        WM_TIMER if wparam == RETRY_TIMER => {
            if state.publish_icon
                && !state.installed.get()
                && !state.shared.stop.load(Ordering::Acquire)
                && state.add(hwnd).is_ok()
            {
                KillTimer(hwnd, RETRY_TIMER);
            }
            0
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn run_window(shared: Arc<Shared>, notifier: &Notifier, publish_icon: bool) -> Result<(), String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if shared.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let icon = load_icon()?;
    let taskbar_name = wide("TaskbarCreated");
    let taskbar_created = unsafe { RegisterWindowMessageW(taskbar_name.as_ptr()) };
    if taskbar_created == 0 {
        return Err(failure(
            "Cannot register the notification-area recovery message",
        ));
    }
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    if module == 0 {
        return Err(failure("Cannot read PCSweep's module identity"));
    }
    let name = wide(&format!(
        "PCSweep.NotificationArea.{}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: module,
        lpszClassName: name.as_ptr(),
        ..unsafe { zeroed() }
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(failure(
            "Cannot register PCSweep's notification-area window",
        ));
    }
    let class = Class { name, module };
    let state = Box::new(Window {
        notifier: notifier.clone(),
        shared: Arc::clone(&shared),
        icon,
        taskbar_created,
        installed: Cell::new(false),
        version4: Cell::new(false),
        publish_icon,
        closing: Cell::new(false),
    });
    // A hidden top-level window receives TaskbarCreated broadcasts. A
    // message-only HWND would silently lose that Explorer recovery path.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.name.as_ptr(),
            wide(&crate::instance::activation_title()).as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            0,
            0,
            module,
            (&*state as *const Window).cast(),
        )
    };
    if hwnd == 0 {
        return Err(failure("Cannot create PCSweep's notification-area window"));
    }
    shared.hwnd.store(hwnd, Ordering::Release);
    // Explorer normally runs at medium integrity, while PCSweep may be
    // elevated. Permit only these data-free tray/recovery messages through
    // UIPI; the private stop message remains restricted to this process.
    if unsafe {
        ChangeWindowMessageFilterEx(hwnd, taskbar_created, MSGFLT_ALLOW, std::ptr::null_mut())
    } == 0
        || unsafe {
            ChangeWindowMessageFilterEx(hwnd, CALLBACK, MSGFLT_ALLOW, std::ptr::null_mut())
        } == 0
    {
        let error = failure("Windows did not allow notification-area recovery messages");
        state.closing.set(true);
        unsafe {
            DestroyWindow(hwnd);
        }
        shared.hwnd.store(0, Ordering::Release);
        return Err(error);
    }
    unsafe {
        ChangeWindowMessageFilterEx(
            hwnd,
            crate::instance::activate_message(),
            MSGFLT_ALLOW,
            std::ptr::null_mut(),
        );
    }
    if !shared.stop.load(Ordering::Acquire) && publish_icon {
        if let Err(e) = state.add(hwnd) {
            state.closing.set(true);
            unsafe {
                DestroyWindow(hwnd);
            }
            shared.hwnd.store(0, Ordering::Release);
            return Err(e);
        }
    }
    if shared.stop.load(Ordering::Acquire) {
        unsafe {
            PostMessageW(hwnd, STOP, 0, 0);
        }
    }
    let mut message: MSG = unsafe { zeroed() };
    let mut result = Ok(());
    loop {
        let read = unsafe { GetMessageW(&mut message, 0, 0, 0) };
        if read == 0 {
            if !shared.stop.load(Ordering::Acquire) && unsafe { IsWindow(hwnd) } != 0 {
                result = Err("PCSweep's notification-area message loop ended unexpectedly. The main window must remain available.".into());
            }
            break;
        }
        if read < 0 {
            result = Err(failure("PCSweep's notification-area message loop stopped"));
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    if unsafe { IsWindow(hwnd) } != 0 {
        state.closing.set(true);
        unsafe {
            DestroyWindow(hwnd);
        }
    }
    shared.hwnd.store(0, Ordering::Release);
    // state (and HICON) outlives all window callbacks; Class drops last.
    drop(state);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_ico_resources_are_bounded_and_invalid_offsets_are_rejected() {
        assert!(icon_resource(include_bytes!("../assets/icon.ico"), 16).is_some());
        assert!(icon_resource(&[0, 0, 1, 0, 0, 0], 16).is_none());
        let mut bad = vec![0, 0, 1, 0, 1, 0];
        bad.extend([16, 16, 0, 0, 1, 0, 32, 0]);
        bad.extend(4u32.to_le_bytes());
        bad.extend(8u32.to_le_bytes());
        bad.extend([1, 2, 3, 4]);
        assert!(
            icon_resource(&bad, 16).is_none(),
            "resources cannot point back into the directory"
        );
        bad[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(icon_resource(&bad, 16).is_none());
    }
    #[test]
    fn menu_and_activation_protocol_does_not_confuse_quit_with_show() {
        assert_eq!(menu_event(SHOW), Some(TrayEvent::Show));
        assert_eq!(menu_event(CHECK_UPDATES), Some(TrayEvent::CheckUpdates));
        assert_eq!(menu_event(QUIT), Some(TrayEvent::Quit));
        assert_eq!(menu_event(0), None);
        assert!(is_show_message(NIN_KEYSELECT));
        assert!(is_show_message(WM_LBUTTONUP));
        assert!(!is_show_message(WM_RBUTTONUP));
        let mut tray = Tray::inert();
        assert!(!tray.available());
        assert!(tray.poll().is_empty());
    }
    #[test]
    fn native_icon_and_hidden_window_cleanup_without_a_real_tray_icon() {
        let ctx = egui::Context::default();
        let shared = Arc::new(Shared::default());
        let (events, receiver) = mpsc::channel();
        let notifier = Notifier {
            events,
            ctx,
            shared: shared.clone(),
        };
        let thread_shared = Arc::clone(&shared);
        let (done, finished) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = run_window(thread_shared, &notifier, false);
            let _ = done.send(result);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let hwnd = loop {
            let hwnd = shared.hwnd.load(Ordering::Acquire);
            if hwnd != 0 {
                break hwnd;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "temporary hidden tray helper window did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            !shared.ready.load(Ordering::Acquire),
            "no actual notification-area icon was registered"
        );
        assert_ne!(
            unsafe { PostMessageW(hwnd, CALLBACK, ICON_ID as usize, WM_LBUTTONUP as isize) },
            0
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            TrayEvent::Show
        );
        shared.stop.store(true, Ordering::Release);
        assert_ne!(unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) }, 0);
        finished
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        assert_eq!(unsafe { IsWindow(hwnd) }, 0);
        assert_eq!(shared.hwnd.load(Ordering::Acquire), 0);
    }
}

/// Data-free activation only. A stale/recycled HWND must never target another app.
fn wake_main(hwnd: HWND) {
    let mut pid = 0;
    unsafe {
        if hwnd != 0 && GetWindowThreadProcessId(hwnd, &mut pid) != 0 && pid == std::process::id() {
            ShowWindowAsync(
                hwnd,
                if IsIconic(hwnd) != 0 {
                    SW_RESTORE
                } else {
                    SW_SHOW
                },
            );
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(test)]
mod wake_tests {
    use super::*;
    #[test]
    fn tray_actions_restore_a_hidden_native_window_before_requesting_a_frame() {
        // No real tray icon or eframe loop needed: exercise the native wake
        // against an actual hidden HWND with the same cross-thread boundary.
        let name = wide(&format!("PCSweep.WakeTest.{}", std::process::id()));
        let module = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            hInstance: module,
            lpszClassName: name.as_ptr(),
            ..unsafe { zeroed() }
        };
        assert_ne!(unsafe { RegisterClassW(&class) }, 0);
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                name.as_ptr(),
                name.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                200,
                100,
                0,
                0,
                module,
                std::ptr::null(),
            )
        };
        assert_ne!(hwnd, 0);
        let shared = Arc::new(Shared::default());
        shared.main_hwnd.store(hwnd, Ordering::Release);
        let (events, rx) = mpsc::channel();
        let notifier = Notifier {
            events,
            ctx: egui::Context::default(),
            shared,
        };
        for event in [TrayEvent::Show, TrayEvent::CheckUpdates, TrayEvent::Quit] {
            unsafe {
                ShowWindow(hwnd, SW_HIDE);
            };
            assert_eq!(unsafe { IsWindowVisible(hwnd) }, 0);
            let sender = notifier.clone();
            let expected = event.clone();
            std::thread::spawn(move || sender.send(event))
                .join()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while unsafe { IsWindowVisible(hwnd) } == 0 {
                let mut msg: MSG = unsafe { zeroed() };
                unsafe {
                    while PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "a hidden window must wake without a UI redraw"
                );
                std::thread::yield_now();
            }
            assert_eq!(rx.try_recv().unwrap(), expected);
        }
        unsafe {
            DestroyWindow(hwnd);
            UnregisterClassW(name.as_ptr(), module);
        }
    }
}
