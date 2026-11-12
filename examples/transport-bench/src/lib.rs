//! Two plugins with the same logic, one per transport, for the measurements
//! in docs/plugin-sdk.md. Not shipped; run with
//! `cargo test -p transport-bench --release -- --ignored --nocapture`.
//!
//! Both offer two actions: `probe` sets the key title to the wall-clock time
//! in microseconds when its key-down handler runs, and `tick` sets the title
//! to a counter once a second.

use std::time::{SystemTime, UNIX_EPOCH};

pub const PIPE_PLUGIN: &str = "com.deckly.bench.pipe";
pub const WS_PLUGIN: &str = "com.deckly.bench.ws";

/// Microseconds since 1970; the same clock in every process.
pub fn now_micros() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros())
}
