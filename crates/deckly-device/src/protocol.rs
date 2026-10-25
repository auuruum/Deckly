//! Report encoding and decoding for the JPEG-image ("second generation")
//! family: Stream Deck (v2), MK.2 and XL.
//!
//! Every layout and offset here is recorded with its evidence in
//! docs/clean-room/2026-10-25-mk2-hid.md. All functions are pure so they can
//! be tested without hardware; parsing never trusts the input.

/// Largest input report we read (report id included).
pub const INPUT_REPORT_LEN: usize = 512;
/// Output report carrying one chunk of a key image (report id included).
pub const IMAGE_REPORT_LEN: usize = 1024;
pub const IMAGE_HEADER_LEN: usize = 8;
pub const IMAGE_CHUNK_LEN: usize = IMAGE_REPORT_LEN - IMAGE_HEADER_LEN;
pub const FEATURE_REPORT_LEN: usize = 32;
/// Key images above this size are refused; a 96 px JPEG is a few KB.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024;
/// The device shows key images turned by 180 degrees, so they are sent
/// pre-rotated.
pub const IMAGE_ROTATED_180: bool = true;

const REPORT_INPUT: u8 = 0x01;
const REPORT_IMAGE: u8 = 0x02;
const IMAGE_COMMAND_KEY: u8 = 0x07;
const REPORT_COMMAND: u8 = 0x03;
const COMMAND_RESET: u8 = 0x02;
const COMMAND_BRIGHTNESS: u8 = 0x08;
pub const REPORT_FIRMWARE: u8 = 0x05;
pub const REPORT_SERIAL: u8 = 0x06;
pub const REPORT_LAYOUT: u8 = 0x08;
/// Key states start after the report id, the event type and a 16-bit count.
const KEY_STATES_OFFSET: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    ImageTooLarge(usize),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::ImageTooLarge(n) => write!(f, "key image is too large ({n} bytes)"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Splits a JPEG key image into output reports, each padded to the full
/// report length.
pub fn image_reports(key: u8, jpeg: &[u8]) -> Result<Vec<[u8; IMAGE_REPORT_LEN]>, ProtocolError> {
    if jpeg.len() > MAX_IMAGE_BYTES {
        return Err(ProtocolError::ImageTooLarge(jpeg.len()));
    }
    let chunks: Vec<&[u8]> = if jpeg.is_empty() {
        vec![&[]]
    } else {
        jpeg.chunks(IMAGE_CHUNK_LEN).collect()
    };
    let last = chunks.len() - 1;
    let mut reports = Vec::with_capacity(chunks.len());
    for (page, chunk) in chunks.into_iter().enumerate() {
        let mut report = [0u8; IMAGE_REPORT_LEN];
        // Both values fit in u16: chunk <= 1016 bytes and page < 65 because
        // of MAX_IMAGE_BYTES.
        let len = (chunk.len() as u16).to_le_bytes();
        let page_le = (page as u16).to_le_bytes();
        report[..IMAGE_HEADER_LEN].copy_from_slice(&[
            REPORT_IMAGE,
            IMAGE_COMMAND_KEY,
            key,
            u8::from(page == last),
            len[0],
            len[1],
            page_le[0],
            page_le[1],
        ]);
        report[IMAGE_HEADER_LEN..IMAGE_HEADER_LEN + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }
    Ok(reports)
}

/// Returns the pressed state of each key, or `None` when the report is not
/// a key report or is too short for `key_count` keys.
pub fn parse_keys(report: &[u8], key_count: u8) -> Option<Vec<bool>> {
    let (&id, rest) = report.split_first()?;
    if id != REPORT_INPUT || rest.first() != Some(&0x00) {
        return None;
    }
    let states = report.get(KEY_STATES_OFFSET..KEY_STATES_OFFSET + usize::from(key_count))?;
    Some(states.iter().map(|&b| b != 0).collect())
}

pub fn brightness_report(percent: u8) -> [u8; FEATURE_REPORT_LEN] {
    let mut report = [0u8; FEATURE_REPORT_LEN];
    report[0] = REPORT_COMMAND;
    report[1] = COMMAND_BRIGHTNESS;
    report[2] = percent.min(100);
    report
}

pub fn reset_report() -> [u8; FEATURE_REPORT_LEN] {
    let mut report = [0u8; FEATURE_REPORT_LEN];
    report[0] = REPORT_COMMAND;
    report[1] = COMMAND_RESET;
    report
}

/// Serial number from feature report 0x06: `[id, len, ascii...]`.
pub fn parse_serial(report: &[u8]) -> Option<String> {
    if report.first() != Some(&REPORT_SERIAL) {
        return None;
    }
    let len = usize::from(*report.get(1)?);
    ascii(report.get(2..2 + len)?)
}

/// Firmware version from feature report 0x05: `[id, len, 4 unknown bytes,
/// ascii...]`, where `len` counts the 4 unknown bytes.
pub fn parse_firmware(report: &[u8]) -> Option<String> {
    if report.first() != Some(&REPORT_FIRMWARE) {
        return None;
    }
    let len = usize::from(*report.get(1)?);
    ascii(report.get(6..2 + len)?)
}

/// Key grid reported by feature report 0x08.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub rows: u8,
    pub columns: u8,
    pub key_width: u16,
    pub key_height: u16,
}

pub fn parse_layout(report: &[u8]) -> Option<Layout> {
    if report.first() != Some(&REPORT_LAYOUT) {
        return None;
    }
    let b = report.get(1..7)?;
    Some(Layout {
        rows: b[0],
        columns: b[1],
        key_width: u16::from_le_bytes([b[2], b[3]]),
        key_height: u16::from_le_bytes([b[4], b[5]]),
    })
}

fn ascii(bytes: &[u8]) -> Option<String> {
    let text: String = bytes
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| char::from(b))
        .collect();
    (!text.is_empty() && text.chars().all(|c| c.is_ascii_graphic())).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Feature reports read from a Stream Deck MK.2, firmware 1.02.000,
    // on 2026-10-25 (docs/clean-room/2026-10-25-mk2-hid.md).
    const MK2_SERIAL: [u8; 16] = [
        0x06, 0x0e, 0x41, 0x38, 0x52, 0x4b, 0x41, 0x34, 0x32, 0x31, 0x32, 0x49, 0x45, 0x4b, 0x36,
        0x4f,
    ];
    const MK2_FIRMWARE: [u8; 14] = [
        0x05, 0x0c, 0xb2, 0xcd, 0xb1, 0x35, 0x31, 0x2e, 0x30, 0x32, 0x2e, 0x30, 0x30, 0x30,
    ];
    const MK2_LAYOUT: [u8; 16] = [
        0x08, 0x03, 0x05, 0x48, 0x00, 0x48, 0x00, 0xe0, 0x01, 0x10, 0x01, 0x04, 0x05, 0x01, 0x04,
        0x04,
    ];

    #[test]
    fn parses_mk2_feature_reports() {
        assert_eq!(parse_serial(&MK2_SERIAL).as_deref(), Some("A8RKA4212IEK6O"));
        assert_eq!(parse_firmware(&MK2_FIRMWARE).as_deref(), Some("1.02.000"));
        assert_eq!(
            parse_layout(&MK2_LAYOUT),
            Some(Layout {
                rows: 3,
                columns: 5,
                key_width: 72,
                key_height: 72
            })
        );
    }

    #[test]
    fn feature_parsers_reject_bad_input() {
        assert_eq!(parse_serial(&[]), None);
        assert_eq!(parse_serial(&[0x06, 0xff, 0x41]), None);
        assert_eq!(parse_serial(&[0x05, 0x01, 0x41]), None);
        assert_eq!(parse_firmware(&[0x05, 0x02]), None);
        assert_eq!(parse_layout(&[0x08, 0x03]), None);
    }

    #[test]
    fn splits_image_into_reports() {
        let jpeg: Vec<u8> = (0..2500u32).map(|i| i as u8).collect();
        let reports = image_reports(4, &jpeg).unwrap();
        assert_eq!(reports.len(), 3);
        assert_eq!(&reports[0][..8], &[0x02, 0x07, 4, 0, 0xf8, 0x03, 0, 0]);
        assert_eq!(&reports[2][..8], &[0x02, 0x07, 4, 1, 0xd4, 0x01, 2, 0]);
        let joined: Vec<u8> = reports
            .iter()
            .enumerate()
            .flat_map(|(i, r)| {
                let len = if i == 2 {
                    2500 - 2 * IMAGE_CHUNK_LEN
                } else {
                    IMAGE_CHUNK_LEN
                };
                r[8..8 + len].to_vec()
            })
            .collect();
        assert_eq!(joined, jpeg);
    }

    #[test]
    fn exact_chunk_multiple_has_no_empty_tail() {
        let reports = image_reports(0, &vec![1u8; IMAGE_CHUNK_LEN * 2]).unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[1][3], 1);
    }

    #[test]
    fn refuses_huge_image() {
        assert!(image_reports(0, &vec![0u8; MAX_IMAGE_BYTES + 1]).is_err());
    }

    #[test]
    fn parses_key_states() {
        let mut report = [0u8; 512];
        report[0] = 0x01;
        report[2] = 15;
        report[4 + 2] = 1;
        let keys = parse_keys(&report, 15).unwrap();
        assert_eq!(keys.iter().filter(|&&k| k).count(), 1);
        assert!(keys[2]);
        assert_eq!(parse_keys(&report[..10], 15), None);
        assert_eq!(parse_keys(&[0x02, 0, 0, 0], 1), None);
        assert_eq!(parse_keys(&[], 1), None);
    }

    #[test]
    fn clamps_brightness() {
        assert_eq!(&brightness_report(250)[..3], &[0x03, 0x08, 100]);
    }
}
