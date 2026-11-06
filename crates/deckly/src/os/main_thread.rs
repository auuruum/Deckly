//! Waiting on the main thread while no window is open.
//!
//! On Windows the tray reaches Deckly through window messages to the thread
//! that added it, so that thread keeps handling messages while it waits for
//! a command; other threads wake it with an empty thread message. On Linux
//! the tray has its own D-Bus thread and waiting is a plain receive. macOS
//! never waits here: its tray needs the event loop, so Deckly keeps a hidden
//! window there instead.

pub use imp::{Waker, recv};

#[cfg(windows)]
mod imp {
    use std::sync::mpsc::{Receiver, RecvError, TryRecvError};
    use windows::Win32::Foundation::{LPARAM, WAIT_FAILED, WPARAM};
    use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_NOREMOVE,
        PM_REMOVE, PeekMessageW, PostThreadMessageW, QS_ALLINPUT, TranslateMessage, WM_NULL,
        WM_USER,
    };

    /// Wakes the thread that made it from [`recv`].
    #[derive(Clone, Copy)]
    pub struct Waker(u32);

    impl Waker {
        pub fn for_this_thread() -> Self {
            let mut msg = MSG::default();
            // SAFETY: `msg` is a valid, writable MSG. Peeking without
            // removing anything gives this thread its message queue now,
            // so a later `wake` cannot be lost for want of one.
            let _ = unsafe { PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE) };
            // SAFETY: takes no arguments and only reads the calling thread's id.
            Self(unsafe { GetCurrentThreadId() })
        }

        /// Call after sending on the channel [`recv`] waits on.
        pub fn wake(&self) {
            // SAFETY: posts an empty message with no pointers to a thread
            // id; an id that no longer exists only makes the call fail.
            let _ = unsafe { PostThreadMessageW(self.0, WM_NULL, WPARAM(0), LPARAM(0)) };
        }
    }

    /// Receives the next value, handling this thread's window messages,
    /// such as the tray's, until one comes.
    pub fn recv<T>(rx: &Receiver<T>) -> Result<T, RecvError> {
        loop {
            match rx.try_recv() {
                Ok(value) => return Ok(value),
                Err(TryRecvError::Disconnected) => return Err(RecvError),
                Err(TryRecvError::Empty) => {}
            }
            // Handling a message may have sent a value: look again first.
            if dispatch_pending() {
                continue;
            }
            // SAFETY: no handles are passed; the call only blocks until this
            // thread's queue has a message, which `wake` also posts.
            let woke = unsafe {
                MsgWaitForMultipleObjectsEx(None, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
            };
            if woke == WAIT_FAILED {
                // Never seen; better a tray that does not answer than a
                // spinning thread. Later starts still reach the channel.
                eprintln!("deckly: cannot wait for window messages");
                return rx.recv();
            }
        }
    }

    /// Handles every message waiting for this thread; says whether any did.
    fn dispatch_pending() -> bool {
        let mut any = false;
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid, writable MSG for each call; a null
        // window handle asks for every message of this thread.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            any = true;
            // SAFETY: `msg` was just filled in by PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        any
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::mpsc::{Receiver, RecvError};

    #[derive(Clone, Copy)]
    pub struct Waker;

    impl Waker {
        pub fn for_this_thread() -> Self {
            Self
        }

        pub fn wake(&self) {}
    }

    pub fn recv<T>(rx: &Receiver<T>) -> Result<T, RecvError> {
        rx.recv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn a_value_from_another_thread_ends_the_wait() {
        let (tx, rx) = channel();
        let waker = Waker::for_this_thread();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            tx.send(7).unwrap();
            waker.wake();
        });
        assert_eq!(recv(&rx), Ok(7));
        sender.join().unwrap();
    }

    #[test]
    fn a_closed_channel_ends_the_wait() {
        let (tx, rx) = channel::<u8>();
        drop(tx);
        assert!(recv(&rx).is_err());
    }
}
