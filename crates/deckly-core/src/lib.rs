//! Device table and profile data model shared by every Deckly crate.
//!
//! Deckly is an independent app and is not made, approved or supported by
//! Elgato or Corsair.

pub mod device;
pub mod profile;
pub mod settings;

pub use device::{
    DeviceKind, DeviceModel, ELGATO_VENDOR_ID, ImageFormat, KeyRect, Protocol, Rotation, Screen,
};
pub use profile::{KeyConfig, Page, Profile, ProfileError, TitleAlign, TitleStyle};
pub use settings::{DeviceSettings, KeyPress, Orientation, Settings, Turn};
