//! The protocol between Deckly and Deckly plugins: message types, framing
//! and limits, shared by the plugin host and the plugin SDK.
//!
//! The host starts a plugin with pipes on its standard input and output and
//! sends frames (see [`frame`]) both ways. It opens with
//! [`HostMessage::Hello`]; the plugin answers [`PluginMessage::Ready`].
//! Everything a plugin sends is untrusted: [`Reader::read`] checks each
//! message against the [`limits`].
//!
//! Pure: no processes, pipes or threads. Design notes: docs/plugin-sdk.md.

pub mod frame;
pub mod limits;
pub mod manifest;
mod message;

pub use frame::{Error, Frame, Reader, Received, Writer, decode, encode_blob, encode_message};
pub use manifest::{MANIFEST_FILE, Manifest, ManifestAction, ManifestState, current_target};
pub use message::*;

#[cfg(test)]
mod tests;
