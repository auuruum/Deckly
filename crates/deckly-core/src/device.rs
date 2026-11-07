//! Supported hardware and its physical layout.
//!
//! Layouts come from Elgato's published HID documentation and from our own
//! hardware observations (docs/clean-room/). A model is only marked `tested`
//! after it was run against real hardware and recorded in docs/devices.md.

use serde::{Deserialize, Serialize};

pub const ELGATO_VENDOR_ID: u16 = 0x0FD9;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    OriginalV2,
    Mk2,
    Mk2Scissor,
    Module15,
    Xl,
    XlV2,
    Module32,
    Neo,
    Plus,
    PlusXl,
    Mini,
    MiniV2,
    MiniDiscord,
    Module6,
}

/// The HID command set a model speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// Report-id/command reports and JPEG images: every family except Mini.
    Main,
    /// The legacy 6-key protocol with BMP images.
    Mini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Bmp,
}

/// How a key image is turned before it is sent, so it appears upright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Half,
    Clockwise,
    CounterClockwise,
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
    pub protocol: Protocol,
    pub rotation: Rotation,
    /// The screen behind the keys, where known. Only models with a known
    /// screen accept a standby image.
    pub screen: Option<Screen>,
    /// Verified on real hardware (see docs/devices.md).
    pub tested: bool,
}

/// The single screen the keys look onto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    /// Largest standby image (JPEG) the device takes, in bytes.
    pub standby_max_bytes: u32,
}

/// A key's window on the screen, in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyRect {
    pub x: u32,
    pub y: u32,
    pub size: u32,
}

// Screens of the 15-key and 32-key families, from Elgato's documentation and
// the official app (docs/clean-room/2026-10-26-orientation-screensaver-standby.md).
const SCREEN_15: Option<Screen> = Some(Screen {
    width: 480,
    height: 272,
    standby_max_bytes: 194_560,
});
const SCREEN_32: Option<Screen> = Some(Screen {
    width: 1024,
    height: 600,
    standby_max_bytes: 614_400,
});

const fn model(
    kind: DeviceKind,
    name: &'static str,
    product_id: u16,
    (columns, rows): (u8, u8),
    key_image_size: u32,
    protocol: Protocol,
    rotation: Rotation,
) -> DeviceModel {
    DeviceModel {
        kind,
        name,
        product_id,
        columns,
        rows,
        key_image_size,
        protocol,
        rotation,
        screen: None,
        tested: false,
    }
}

const fn with_screen(m: DeviceModel, screen: Option<Screen>) -> DeviceModel {
    DeviceModel { screen, ..m }
}

impl DeviceModel {
    pub const ALL: &'static [DeviceModel] = {
        use DeviceKind as K;
        use Protocol::{Main, Mini};
        use Rotation::{Clockwise, CounterClockwise, Half};
        &[
            DeviceModel {
                tested: true,
                ..with_screen(
                    model(K::Mk2, "Stream Deck MK.2", 0x0080, (5, 3), 72, Main, Half),
                    SCREEN_15,
                )
            },
            with_screen(
                model(
                    K::OriginalV2,
                    "Stream Deck (v2)",
                    0x006D,
                    (5, 3),
                    72,
                    Main,
                    Half,
                ),
                SCREEN_15,
            ),
            with_screen(
                model(
                    K::Mk2Scissor,
                    "Stream Deck MK.2 (scissor keys)",
                    0x00A5,
                    (5, 3),
                    72,
                    Main,
                    Half,
                ),
                SCREEN_15,
            ),
            with_screen(
                model(
                    K::Module15,
                    "Stream Deck 15-key module",
                    0x00B9,
                    (5, 3),
                    72,
                    Main,
                    Half,
                ),
                SCREEN_15,
            ),
            with_screen(
                model(K::Xl, "Stream Deck XL", 0x006C, (8, 4), 96, Main, Half),
                SCREEN_32,
            ),
            with_screen(
                model(
                    K::XlV2,
                    "Stream Deck XL (v2)",
                    0x008F,
                    (8, 4),
                    96,
                    Main,
                    Half,
                ),
                SCREEN_32,
            ),
            with_screen(
                model(
                    K::Module32,
                    "Stream Deck 32-key module",
                    0x00BA,
                    (8, 4),
                    96,
                    Main,
                    Half,
                ),
                SCREEN_32,
            ),
            model(K::Neo, "Stream Deck Neo", 0x009A, (4, 2), 96, Main, Half),
            model(
                K::Plus,
                "Stream Deck +",
                0x0084,
                (4, 2),
                120,
                Main,
                Rotation::None,
            ),
            model(
                K::PlusXl,
                "Stream Deck + XL",
                0x00C6,
                (9, 4),
                112,
                Main,
                CounterClockwise,
            ),
            model(
                K::Mini,
                "Stream Deck Mini",
                0x0063,
                (3, 2),
                80,
                Mini,
                Clockwise,
            ),
            model(
                K::MiniV2,
                "Stream Deck Mini (v2)",
                0x0090,
                (3, 2),
                80,
                Mini,
                Clockwise,
            ),
            model(
                K::MiniDiscord,
                "Stream Deck Mini Discord",
                0x00B3,
                (3, 2),
                80,
                Mini,
                Clockwise,
            ),
            model(
                K::Module6,
                "Stream Deck 6-key module",
                0x00B8,
                (3, 2),
                80,
                Mini,
                Clockwise,
            ),
        ]
    };

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

    pub fn image_format(&self) -> ImageFormat {
        match self.protocol {
            Protocol::Main => ImageFormat::Jpeg,
            Protocol::Mini => ImageFormat::Bmp,
        }
    }

    /// Size of the picture that spans all keys: the screen where it is
    /// known, otherwise the key grid with gaps of 5/12 of a key.
    pub fn panel_size(&self) -> (u32, u32) {
        match self.screen {
            Some(s) => (s.width, s.height),
            None => {
                let span = |n: u8| {
                    let n = u32::from(n.max(1));
                    n * self.key_image_size + (n - 1) * self.gap()
                };
                (span(self.columns), span(self.rows))
            }
        }
    }

    fn gap(&self) -> u32 {
        self.key_image_size * 5 / 12
    }

    /// Whether a wallpaper is cut the same way on both models: the same
    /// rows and columns, and the same panel shape and key places once
    /// scaled to the same width. Wallpaper places are relative to the panel,
    /// so such models show the same picture on their keys.
    pub fn same_layout(&self, other: &DeviceModel) -> bool {
        let ((aw, ah), (bw, bh)) = (self.panel_size(), other.panel_size());
        let (aw, ah, bw, bh) = (u64::from(aw), u64::from(ah), u64::from(bw), u64::from(bh));
        // a / aw == b / bw, without dividing.
        let same = |a: u32, b: u32, a_span: u64, b_span: u64| {
            u64::from(a) * b_span == u64::from(b) * a_span
        };
        self.columns == other.columns
            && self.rows == other.rows
            && aw * bh == bw * ah
            && self
                .key_rects()
                .iter()
                .zip(other.key_rects())
                .all(|(a, b)| {
                    same(a.x, b.x, aw, bw) && same(a.y, b.y, ah, bh) && same(a.size, b.size, aw, bw)
                })
    }

    /// The models in groups that share a layout (see [`Self::same_layout`]),
    /// in table order.
    pub fn layouts() -> Vec<Vec<&'static DeviceModel>> {
        let mut groups: Vec<Vec<&'static DeviceModel>> = Vec::new();
        for model in Self::ALL {
            match groups
                .iter_mut()
                .find(|g| g.first().is_some_and(|first| first.same_layout(model)))
            {
                Some(group) => group.push(model),
                None => groups.push(vec![model]),
            }
        }
        groups
    }

    /// Where each key sits on the panel, row by row from the top left.
    /// Keys are spread evenly with no outer margin (inferred for the
    /// screens we know; see the clean-room note).
    pub fn key_rects(&self) -> Vec<KeyRect> {
        let (w, h) = self.panel_size();
        let key = self.key_image_size;
        let step = |extent: u32, n: u8| {
            if n > 1 {
                extent.saturating_sub(key) / u32::from(n - 1)
            } else {
                0
            }
        };
        let (dx, dy) = (step(w, self.columns), step(h, self.rows));
        (0..self.rows)
            .flat_map(|r| {
                (0..self.columns).map(move |c| KeyRect {
                    x: u32::from(c) * dx,
                    y: u32::from(r) * dy,
                    size: key,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_that_cut_a_wallpaper_alike_share_a_layout() {
        use DeviceKind as K;
        let kinds: Vec<Vec<DeviceKind>> = DeviceModel::layouts()
            .iter()
            .map(|g| g.iter().map(|m| m.kind).collect())
            .collect();
        assert_eq!(
            kinds,
            vec![
                vec![K::Mk2, K::OriginalV2, K::Mk2Scissor, K::Module15],
                vec![K::Xl, K::XlV2, K::Module32],
                // Same grid and gaps in proportion to their key size.
                vec![K::Neo, K::Plus],
                vec![K::PlusXl],
                vec![K::Mini, K::MiniV2, K::MiniDiscord, K::Module6],
            ]
        );
        let mk2 = DeviceModel::of(K::Mk2);
        assert!(mk2.same_layout(mk2));
        assert!(!mk2.same_layout(DeviceModel::of(K::Xl)));
        // Every model is in exactly one group.
        let total: usize = DeviceModel::layouts().iter().map(Vec::len).sum();
        assert_eq!(total, DeviceModel::ALL.len());
    }

    #[test]
    fn every_kind_has_one_model() {
        for m in DeviceModel::ALL {
            assert_eq!(
                DeviceModel::ALL.iter().filter(|o| o.kind == m.kind).count(),
                1
            );
            assert_eq!(DeviceModel::of(m.kind), m);
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
        assert_eq!(m.rotation, Rotation::Half);
        assert!(m.tested);
    }

    // Layouts and rotations from Elgato's HID documentation
    // (docs/clean-room/2026-10-26-elgato-hid-docs.md).
    #[test]
    fn documented_layouts() {
        let plus = DeviceModel::of(DeviceKind::Plus);
        assert_eq!((plus.key_count(), plus.key_image_size), (8, 120));
        assert_eq!(plus.rotation, Rotation::None);
        let plus_xl = DeviceModel::of(DeviceKind::PlusXl);
        assert_eq!((plus_xl.columns, plus_xl.rows), (9, 4));
        assert_eq!(plus_xl.rotation, Rotation::CounterClockwise);
        let mini = DeviceModel::of(DeviceKind::Mini);
        assert_eq!((mini.key_count(), mini.key_image_size), (6, 80));
        assert_eq!(mini.image_format(), ImageFormat::Bmp);
        assert_eq!(DeviceModel::of(DeviceKind::Xl).key_count(), 32);
    }

    #[test]
    fn mk2_keys_on_its_screen() {
        let m = DeviceModel::of(DeviceKind::Mk2);
        assert_eq!(m.panel_size(), (480, 272));
        let rects = m.key_rects();
        assert_eq!(rects.len(), 15);
        assert_eq!(
            rects[0],
            KeyRect {
                x: 0,
                y: 0,
                size: 72
            }
        );
        assert_eq!(rects[1].x, 102);
        assert_eq!(rects[5].y, 100);
        assert_eq!(
            rects[14],
            KeyRect {
                x: 408,
                y: 200,
                size: 72
            }
        );
    }

    #[test]
    fn key_rects_stay_inside_the_panel() {
        for m in DeviceModel::ALL {
            let (w, h) = m.panel_size();
            let rects = m.key_rects();
            assert_eq!(rects.len(), usize::from(m.key_count()), "{}", m.name);
            for r in rects {
                assert!(r.x + r.size <= w && r.y + r.size <= h, "{}", m.name);
            }
        }
        // A model without a known screen gets a grid with gaps.
        let mini = DeviceModel::of(DeviceKind::Mini);
        assert_eq!(mini.panel_size(), (3 * 80 + 2 * 33, 2 * 80 + 33));
    }

    #[test]
    fn standby_needs_a_known_screen() {
        assert!(DeviceModel::of(DeviceKind::Xl).screen.is_some());
        assert!(DeviceModel::of(DeviceKind::Plus).screen.is_none());
        assert!(DeviceModel::of(DeviceKind::Mini).screen.is_none());
    }

    #[test]
    fn only_the_mk2_is_tested() {
        let tested: Vec<_> = DeviceModel::ALL.iter().filter(|m| m.tested).collect();
        assert_eq!(tested.len(), 1);
        assert_eq!(tested[0].kind, DeviceKind::Mk2);
    }

    #[test]
    fn device_kinds_keep_their_names() {
        // Profiles store these names; renaming one breaks saved profiles.
        let json = serde_json::to_string(&[DeviceKind::Mk2, DeviceKind::XlV2, DeviceKind::PlusXl])
            .unwrap_or_default();
        assert_eq!(json, r#"["mk2","xl_v2","plus_xl"]"#);
    }
}
