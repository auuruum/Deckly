//! Supported hardware and its physical layout.
//!
//! Layouts come from the public product specifications and from our own
//! hardware observations (docs/clean-room/). A model is only marked `tested`
//! after it was run against real hardware and recorded in docs/devices.md.

use serde::{Deserialize, Serialize};

pub const ELGATO_VENDOR_ID: u16 = 0x0FD9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    OriginalV2,
    Mk2,
    Xl,
    XlV2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceModel {
    pub kind: DeviceKind,
    pub name: &'static str,
    pub product_id: u16,
    pub columns: u8,
    pub rows: u8,
    /// Width and height of one key image in pixels.
    pub key_image_size: u32,
    /// Verified on real hardware (see docs/devices.md).
    pub tested: bool,
}

impl DeviceModel {
    pub const ALL: &'static [DeviceModel] = &[
        DeviceModel {
            kind: DeviceKind::Mk2,
            name: "Stream Deck MK.2",
            product_id: 0x0080,
            columns: 5,
            rows: 3,
            key_image_size: 72,
            tested: true,
        },
        DeviceModel {
            kind: DeviceKind::OriginalV2,
            name: "Stream Deck (v2)",
            product_id: 0x006D,
            columns: 5,
            rows: 3,
            key_image_size: 72,
            tested: false,
        },
        DeviceModel {
            kind: DeviceKind::Xl,
            name: "Stream Deck XL",
            product_id: 0x006C,
            columns: 8,
            rows: 4,
            key_image_size: 96,
            tested: false,
        },
        DeviceModel {
            kind: DeviceKind::XlV2,
            name: "Stream Deck XL (v2)",
            product_id: 0x008F,
            columns: 8,
            rows: 4,
            key_image_size: 96,
            tested: false,
        },
    ];

    pub fn of(kind: DeviceKind) -> &'static DeviceModel {
        // `ALL` has one entry per kind; the fallback only guards against a
        // future enum variant missing from the table.
        Self::ALL
            .iter()
            .find(|m| m.kind == kind)
            .unwrap_or(&Self::ALL[0])
    }

    pub fn from_product_id(product_id: u16) -> Option<&'static DeviceModel> {
        Self::ALL.iter().find(|m| m.product_id == product_id)
    }

    pub fn key_count(&self) -> u8 {
        self.columns * self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_one_model() {
        for kind in [
            DeviceKind::OriginalV2,
            DeviceKind::Mk2,
            DeviceKind::Xl,
            DeviceKind::XlV2,
        ] {
            assert_eq!(
                DeviceModel::ALL.iter().filter(|m| m.kind == kind).count(),
                1
            );
            assert_eq!(DeviceModel::of(kind).kind, kind);
        }
    }

    #[test]
    fn product_ids_are_unique() {
        for m in DeviceModel::ALL {
            assert_eq!(DeviceModel::from_product_id(m.product_id), Some(m));
        }
    }

    #[test]
    fn mk2_layout() {
        let m = DeviceModel::of(DeviceKind::Mk2);
        assert_eq!((m.columns, m.rows, m.key_count()), (5, 3, 15));
    }
}
