//! Windows: a hook reports each new foreground window to the thread that set
//! it, which only needs to pump messages. The program comes from the window's
//! process. Hosted packaged apps (`ApplicationFrameHost.exe`) tell themselves
//! apart by the window's app id, and packaged desktop apps by the process's.

use super::Report;
use std::sync::OnceLock;
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HWND};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW,
    GetWindowThreadProcessId, MSG, WINEVENT_OUTOFCONTEXT,
};
use windows::core::PWSTR;

/// Longest path Windows can return, in characters.
const MAX_PATH_CHARS: usize = 32_768;
/// `APPLICATION_USER_MODEL_ID_MAX_LENGTH`, with room to spare.
const MAX_APP_ID_CHARS: usize = 256;

/// The hook procedure takes no context of its own.
static REPORT: OnceLock<Report> = OnceLock::new();

pub fn watch(report: Report) {
    let failed = report.clone();
    let run = move || {
        let report = REPORT.get_or_init(|| report);
        if let Err(e) = listen(report) {
            report(Err(format!("Cannot follow the active window: {e}")));
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("deckly-active-window".into())
        .spawn(run)
    {
        failed(Err(format!("Cannot follow the active window: {e}")));
    }
}

/// Sets the hook and pumps its events, for as long as Deckly runs.
fn listen(report: &Report) -> Result<(), String> {
    // SAFETY: this new thread owns its apartment and never leaves it, so it
    // needs no uninitializing. Reading a window's properties wants COM.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.map_err(|e| e.to_string())?;
    // SAFETY: the procedure lives as long as the program. An out-of-context
    // hook needs no module, and calls it on this thread, which keeps pumping
    // messages; the hook stays until the program ends. `msg` outlives the
    // calls that use it.
    unsafe {
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        if hook.is_invalid() {
            return Err("the system refused the event hook".into());
        }
        // Read after hooking, so a change between the two is not lost.
        let window = GetForegroundWindow();
        if !window.is_invalid() {
            report(Ok(identify(window)));
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    // No window is in the foreground for a moment during some switches.
    if let (false, Some(report)) = (window.is_invalid(), REPORT.get()) {
        report(Ok(identify(window)));
    }
}

/// The names of a window's application. A window that is gone, and a process
/// Deckly may not look into, give what is left, which may be nothing.
fn identify(window: HWND) -> Vec<String> {
    let mut names = Vec::new();
    let mut id = 0;
    // SAFETY: `id` outlives the call; a stale window leaves it at 0.
    unsafe { GetWindowThreadProcessId(window, Some(&mut id)) };
    // SAFETY: this access only reads the process's name; the guard closes it.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) };
    if let Ok(process) = process.map(Process) {
        names.extend(program(&process));
        names.extend(process_app_id(&process));
    }
    names.extend(window_app_id(window));
    names
}

struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: OpenProcess gave this handle to this guard alone.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn program(process: &Process) -> Option<String> {
    let mut path = vec![0; MAX_PATH_CHARS];
    let mut len = path.len() as u32;
    // SAFETY: `path` holds `len` characters and both outlive the call, which
    // sets `len` to the characters written.
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut len,
        )
    }
    .ok()?;
    Some(String::from_utf16_lossy(path.get(..len as usize)?))
}

/// A packaged process's app id; an ordinary process has none.
fn process_app_id(process: &Process) -> Option<String> {
    let mut id = [0; MAX_APP_ID_CHARS];
    let mut len = id.len() as u32;
    // SAFETY: `id` holds `len` characters and both outlive the call, which
    // sets `len` to the characters written, with the final null.
    let result =
        unsafe { GetApplicationUserModelId(process.0, &mut len, Some(PWSTR(id.as_mut_ptr()))) };
    if result != ERROR_SUCCESS {
        return None;
    }
    Some(String::from_utf16_lossy(
        id.get(..(len as usize).checked_sub(1)?)?,
    ))
}

/// The app id a window itself carries, as hosted packaged apps do.
fn window_app_id(window: HWND) -> Option<String> {
    // SAFETY: COM is initialized on this thread and the store is released by
    // its wrapper. The key is a static, and a stale window gives an error.
    let id = unsafe {
        SHGetPropertyStoreForWindow::<IPropertyStore>(window)
            .and_then(|store| store.GetValue(&PKEY_AppUserModel_ID))
    };
    Some(id.ok()?.to_string()).filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::channel;
    use std::time::Duration;
    use windows::Win32::UI::Accessibility::NotifyWinEvent;
    use windows::Win32::UI::WindowsAndMessaging::{
        CHILDID_SELF, FindWindowW, GetDesktopWindow, OBJID_WINDOW,
    };
    use windows::core::w;

    #[test]
    fn the_taskbar_is_named_by_its_program() {
        // SAFETY: the class name is static.
        let taskbar = unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.unwrap();
        let names = identify(taskbar);
        assert!(
            names[0].to_lowercase().ends_with("\\explorer.exe"),
            "{names:?}"
        );
    }

    /// The desktop belongs to a protected system process.
    #[test]
    fn a_window_of_a_closed_process_gives_no_error() {
        // SAFETY: takes no arguments.
        identify(unsafe { GetDesktopWindow() });
    }

    #[test]
    fn a_window_that_is_gone_has_no_names() {
        assert!(identify(HWND(std::ptr::null_mut())).is_empty());
        assert!(identify(HWND(0x7fff_fff0 as _)).is_empty());
    }

    #[test]
    fn an_ordinary_process_has_no_app_id() {
        // SAFETY: the access only reads the name of this very process.
        let process =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, std::process::id()) }
                .map(Process)
                .unwrap();
        assert_eq!(process_app_id(&process), None);
        let path = program(&process).unwrap();
        assert_eq!(
            std::path::Path::new(&path),
            std::env::current_exe().unwrap()
        );
    }

    /// A real event through the hook, with no change of focus. The event goes
    /// to every hook on this desktop, a running Deckly's too.
    #[test]
    #[ignore = "a running Deckly would hear this"]
    fn a_foreground_event_reaches_the_report() {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        super::super::watch(move |names| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(names);
            }
        });
        // The current foreground comes after the hook is set, if there is one.
        let _ = rx.recv_timeout(Duration::from_secs(2));
        // SAFETY: the class name is static and the event carries no pointer.
        unsafe {
            let taskbar = FindWindowW(w!("Shell_TrayWnd"), None).unwrap();
            NotifyWinEvent(
                EVENT_SYSTEM_FOREGROUND,
                taskbar,
                OBJID_WINDOW.0,
                CHILDID_SELF as i32,
            );
        }
        loop {
            let names = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
            if names
                .iter()
                .any(|n| n.to_lowercase().ends_with("explorer.exe"))
            {
                return;
            }
        }
    }

    /// The real hook on the real desktop, watched for the given time: it
    /// reports what the owner does anyway and touches nothing.
    /// `DECKLY_WATCH_SECONDS=120 cargo test -p deckly watches_the_desktop -- --ignored --nocapture`
    #[test]
    #[ignore = "waits for the user to change windows"]
    fn watches_the_desktop() {
        let seconds = std::env::var("DECKLY_WATCH_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        super::super::watch(move |names| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(names);
            }
        });
        let mut reports = 0;
        while let Ok(names) = rx.recv_timeout(Duration::from_secs(seconds)) {
            println!("{:?}", names.unwrap());
            reports += 1;
        }
        println!("{reports} reports");
    }
}
