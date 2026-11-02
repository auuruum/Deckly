//! Tells the [`Gate`] when the session locks and unlocks. Each system signals
//! that its own way; all of them wait for events and never poll. A system
//! that cannot say is marked undetected, which leaves keys working.
//!
//! [`watch`] must be called on the main thread and only once: macOS delivers
//! its notifications there.

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
use crate::lock_gate::Gate;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use self::linux::watch;
#[cfg(target_os = "macos")]
pub use self::macos::watch;
#[cfg(windows)]
pub use self::windows::watch;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn watch(gate: Gate) {
    gate.set_undetected();
}
