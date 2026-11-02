//! macOS: the system posts distributed notifications when the screen locks
//! and unlocks. They reach the main thread, whose run loop the app keeps
//! running, also with the window closed to the tray.

use crate::lock_gate::Gate;
use objc2_core_foundation::{
    CFDictionary, CFNotificationCallback, CFNotificationCenter, CFNotificationName,
    CFNotificationSuspensionBehavior, CFString, kCFBooleanFalse,
};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;
use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

const LOCKED: &str = "com.apple.screenIsLocked";
const UNLOCKED: &str = "com.apple.screenIsUnlocked";
const LOCKED_KEY: &str = "CGSSessionScreenIsLocked";

/// The callbacks take no context of their own.
static GATE: OnceLock<Gate> = OnceLock::new();

pub fn watch(gate: Gate) {
    let gate = GATE.get_or_init(|| gate);
    let Some(center) = CFNotificationCenter::distributed_center() else {
        eprintln!("deckly: cannot watch the lock screen: no notification center");
        gate.set_undetected();
        return;
    };
    let observers: [(&'static str, CFNotificationCallback); 2] =
        [(LOCKED, Some(locked)), (UNLOCKED, Some(unlocked))];
    for (name, callback) in observers {
        let name = CFString::from_static_str(name);
        // SAFETY: the callbacks live as long as the program and use neither
        // the null observer nor the null object. AppKit suspends distributed
        // notifications while the app is in the background, which is where
        // Deckly mostly is, so they must be delivered regardless.
        unsafe {
            center.add_observer(
                ptr::null(),
                callback,
                Some(&name),
                ptr::null(),
                CFNotificationSuspensionBehavior::DeliverImmediately,
            );
        }
    }
    // Read after registering, so a change between the two is not lost.
    match screen_is_locked() {
        Some(locked) => gate.set_locked(locked),
        None => gate.set_undetected(),
    }
}

unsafe extern "C-unwind" fn locked(
    _center: *mut CFNotificationCenter,
    _observer: *mut c_void,
    _name: *const CFNotificationName,
    _object: *const c_void,
    _info: *const CFDictionary,
) {
    set(true);
}

unsafe extern "C-unwind" fn unlocked(
    _center: *mut CFNotificationCenter,
    _observer: *mut c_void,
    _name: *const CFNotificationName,
    _object: *const c_void,
    _info: *const CFDictionary,
) {
    set(false);
}

fn set(locked: bool) {
    if let Some(gate) = GATE.get() {
        gate.set_locked(locked);
    }
}

/// Whether the screen is locked now; `None` outside a graphical session. The
/// key is only there while locked.
fn screen_is_locked() -> Option<bool> {
    let session = CGSessionCopyCurrentDictionary()?;
    let key = CFString::from_static_str(LOCKED_KEY);
    let key: *const CFString = &*key;
    // SAFETY: the key is a live CFString, like the keys of this dictionary.
    // The value is only compared, never read.
    let value = unsafe { session.value(key.cast()) };
    // SAFETY: a constant of CoreFoundation, set when the library loads.
    let off = unsafe { kCFBooleanFalse }.map_or(ptr::null(), |off| ptr::from_ref(off).cast());
    Some(!value.is_null() && value != off)
}
