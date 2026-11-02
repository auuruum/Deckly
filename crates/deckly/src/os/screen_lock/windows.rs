//! Windows: session changes arrive as a message to a window, so a window that
//! is never shown waits for them on its own thread.

use crate::lock_gate::Gate;
use std::sync::OnceLock;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK,
    WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW, WTSRegisterSessionNotification,
    WTSSessionInfoEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_SESSION_LOCK,
    WTS_SESSION_UNLOCK,
};
use windows::core::{Error, PCWSTR, PWSTR, Result, w};

const CLASS: PCWSTR = w!("DecklySessionWatcher");

/// The window procedure takes no context of its own.
static GATE: OnceLock<Gate> = OnceLock::new();

pub fn watch(gate: Gate) {
    let undetected = gate.clone();
    let run = move || {
        let gate = GATE.get_or_init(|| gate);
        if let Err(e) = listen(gate) {
            eprintln!("deckly: cannot watch the lock screen: {e}");
            gate.set_undetected();
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name("deckly-screen-lock".into())
        .spawn(run)
    {
        eprintln!("deckly: cannot watch the lock screen: {e}");
        undetected.set_undetected();
    }
}

/// Creates the window and waits for its messages, for as long as Deckly runs.
fn listen(gate: &Gate) -> Result<()> {
    // SAFETY: the class and window use static strings and a procedure that
    // lives as long as the program. The window stays on this thread, which
    // keeps pumping its messages, and is never destroyed, so it needs no
    // unregistering. `msg` outlives the calls that use it.
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(Error::from_thread());
        }
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Deckly"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?;
        WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION)?;
        // Read after registering, so a change between the two is not lost:
        // its message waits below.
        if let Some(locked) = session_locked() {
            gate.set_locked(locked);
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_WTSSESSION_CHANGE
        && let (Some(locked), Some(gate)) = (session_event(wparam.0), GATE.get())
    {
        gate.set_locked(locked);
    }
    // SAFETY: passes on the arguments Windows gave this procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// What a session change event says about the lock.
fn session_event(event: usize) -> Option<bool> {
    match u32::try_from(event).ok()? {
        WTS_SESSION_LOCK => Some(true),
        WTS_SESSION_UNLOCK => Some(false),
        _ => None,
    }
}

/// Whether this session is locked now, if Windows says.
fn session_locked() -> Option<bool> {
    let mut buffer = PWSTR::null();
    let mut size = 0;
    // SAFETY: the out-pointers live through the call. Windows returns a
    // buffer of `size` bytes, read only after checking that it holds level 1
    // of the structure, and freed once.
    unsafe {
        WTSQuerySessionInformationW(
            None,
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut size,
        )
        .ok()?;
        let info = buffer.0.cast::<WTSINFOEXW>();
        let flags =
            (size as usize >= size_of::<WTSINFOEXW>() && !info.is_null() && (*info).Level == 1)
                .then(|| (*info).Data.WTSInfoExLevel1.SessionFlags);
        WTSFreeMemory(buffer.0.cast());
        session_flags(flags?)
    }
}

fn session_flags(flags: i32) -> Option<bool> {
    match flags as u32 {
        WTS_SESSIONSTATE_LOCK => Some(true),
        WTS_SESSIONSTATE_UNLOCK => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions;
    use std::time::{Duration, Instant};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};

    #[test]
    fn only_lock_events_change_the_state() {
        assert_eq!(session_event(WTS_SESSION_LOCK as usize), Some(true));
        assert_eq!(session_event(WTS_SESSION_UNLOCK as usize), Some(false));
        assert_eq!(session_event(5), None);
        assert_eq!(session_event(usize::MAX), None);
    }

    #[test]
    fn session_flags_read_as_documented() {
        assert_eq!(session_flags(0), Some(true));
        assert_eq!(session_flags(1), Some(false));
        assert_eq!(session_flags(-1), None);
    }

    #[test]
    fn this_session_reports_its_lock_state() {
        assert!(session_locked().is_some());
    }

    /// Runs the real hidden window; messages are posted to it directly, so
    /// nothing locks.
    #[test]
    fn the_hidden_window_passes_session_events_to_the_gate() {
        let gate = Gate::new(true);
        watch(gate.clone());
        let started = Instant::now();
        let window = loop {
            // SAFETY: the class name is a static string.
            match unsafe { FindWindowW(CLASS, None) } {
                Ok(window) => break window,
                Err(e) => assert!(started.elapsed() < Duration::from_secs(5), "{e}"),
            }
            std::thread::yield_now();
        };
        let post = |event: u32| {
            let event = WPARAM(event as usize);
            // SAFETY: the window lives until the program ends.
            unsafe { PostMessageW(Some(window), WM_WTSSESSION_CHANGE, event, LPARAM(0)) }.unwrap();
        };
        let became = |blocked: bool| {
            let started = Instant::now();
            while gate.blocks(actions::TEXT) != blocked {
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::yield_now();
            }
        };
        post(WTS_SESSION_LOCK);
        became(true);
        post(WTS_SESSION_UNLOCK);
        became(false);
        assert!(!gate.undetected());
    }
}
