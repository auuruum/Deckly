//! Device table and profile data model shared by every Deckly crate.
//!
//! Deckly is an independent app and is not made, approved or supported by
//! Elgato or Corsair.

pub mod device;
pub mod profile;

pub use device::{DeviceKind, DeviceModel, ELGATO_VENDOR_ID};
pub use profile::{KeyConfig, Page, Profile, ProfileError, TitleAlign, TitleStyle};
