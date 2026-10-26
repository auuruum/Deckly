//! Report encoding and decoding for the legacy protocol of the 6-key Mini
//! family (Mini, Mini 2022, Mini Discord, 6-key module).
//!
//! Taken from Elgato's documentation, recorded in
//! docs/clean-room/2026-10-26-elgato-hid-docs.md. Not tested on hardware:
//! nobody on the project owns a Mini yet. All functions are pure.

use crate::protocol::{
    FEATURE_REPORT_LEN, IMAGE_REPORT_LEN, MAX_IMAGE_BYTES, ProtocolError, ascii,
};

const IMAGE_HEADER_LEN: usize = 16;
pub const IMAGE_CHUNK_LEN: usize = IMAGE_REPORT_LEN - IMAGE_HEADER_LEN;

const REPORT_INPUT: u8 = 0x01;
const REPORT_IMAGE: u8 = 0x02;
const IMAGE_COMMAND_UPLOAD: u8 = 0x01;
pub const REPORT_SERIAL: u8 = 0x03;
/// Primary ("AP2") firmware.
pub const REPORT_FIRMWARE: u8 = 0xA1;
/// Version and serial text start after the report id and 4 unused bytes.
const TEXT_OFFSET: usize = 5;
const FIRMWARE_TEXT_LEN: usize = 12;

/// Splits a BMP key image into output reports. The "show image" flag is set
/// on the last chunk only, so the key changes once the whole image is in.
pub fn image_reports(key: u8, bmp: &[u8]) -> Result<Vec<[u8; IMAGE_REPORT_LEN]>, ProtocolError> {
    if bmp.len() > MAX_IMAGE_BYTES {
        return Err(ProtocolError::ImageTooLarge(bmp.len()));
    }
    let chunks: Vec<&[u8]> = if bmp.is_empty() {
        vec![&[]]
    } else {
        bmp.chunks(IMAGE_CHUNK_LEN).collect()
    };
    let last = chunks.len() - 1;
    let mut reports = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.into_iter().enumerate() {
        let mut report = [0u8; IMAGE_REPORT_LEN];
        // index < 66 because of MAX_IMAGE_BYTES, so it fits in a byte.
        report[..6].copy_from_slice(&[
            REPORT_IMAGE,
            IMAGE_COMMAND_UPLOAD,
            index as u8,
            0,
            u8::from(index == last),
            key,
        ]);
        report[IMAGE_HEADER_LEN..IMAGE_HEADER_LEN + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }
    Ok(reports)
}

/// Returns the pressed state of each key. Key bytes follow the report id
/// directly, unlike the main protocol.
pub fn parse_keys(report: &[u8], key_count: u8) -> Option<Vec<bool>> {
    if report.first() != Some(&REPORT_INPUT) {
        return None;
    }
    let states = report.get(1..1 + usize::from(key_count))?;
    Some(states.iter().map(|&b| b != 0).collect())
}

pub fn brightness_report(percent: u8) -> [u8; FEATURE_REPORT_LEN] {
    let mut report = [0u8; FEATURE_REPORT_LEN];
    report[..6].copy_from_slice(&[0x05, 0x55, 0xAA, 0xD1, 0x01, percent.min(100)]);
    report
}

/// Shows the device's own logo again.
pub fn reset_report() -> [u8; FEATURE_REPORT_LEN] {
    let mut report = [0u8; FEATURE_REPORT_LEN];
    report[..3].copy_from_slice(&[0x0B, 0x63, 0x00]);
    report
}

pub fn parse_firmware(report: &[u8]) -> Option<String> {
    if report.first() != Some(&REPORT_FIRMWARE) {
        return None;
    }
    ascii(report.get(TEXT_OFFSET..TEXT_OFFSET + FIRMWARE_TEXT_LEN)?)
}

/// The documentation gives no length for the serial, so it runs to the
/// first zero byte or the end of the report.
pub fn parse_serial(report: &[u8]) -> Option<String> {
    if report.first() != Some(&REPORT_SERIAL) {
        return None;
    }
    ascii(report.get(TEXT_OFFSET..)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_image_into_reports() {
        // An 80 px, 24-bit BMP: 54-byte header plus 80 * 80 * 3 bytes.
        let bmp: Vec<u8> = (0..19_254u32).map(|i| i as u8).collect();
        let reports = image_reports(5, &bmp).unwrap();
        assert_eq!(reports.len(), 20);
        assert_eq!(
            &reports[0][..16],
            &[0x02, 0x01, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(&reports[19][..6], &[0x02, 0x01, 19, 0, 1, 5]);
        assert_eq!(&reports[0][16..20], &bmp[..4]);
        let joined: Vec<u8> = reports
            .iter()
            .flat_map(|r| r[16..].to_vec())
            .take(bmp.len())
            .collect();
        assert_eq!(joined, bmp);
        assert!(image_reports(0, &vec![0u8; MAX_IMAGE_BYTES + 1]).is_err());
    }

    #[test]
    fn parses_key_states() {
        let mut report = [0u8; 65];
        report[0] = 0x01;
        report[1 + 4] = 1;
        let keys = parse_keys(&report, 6).unwrap();
        assert_eq!(keys, vec![false, false, false, false, true, false]);
        assert_eq!(parse_keys(&report[..4], 6), None);
        assert_eq!(parse_keys(&[0x02, 1, 1, 1, 1, 1, 1], 6), None);
    }

    #[test]
    fn feature_reports() {
        assert_eq!(
            &brightness_report(250)[..6],
            &[0x05, 0x55, 0xAA, 0xD1, 0x01, 100]
        );
        assert_eq!(&reset_report()[..3], &[0x0B, 0x63, 0x00]);
        let mut firmware = [0u8; 32];
        firmware[0] = 0xA1;
        firmware[5..13].copy_from_slice(b"3.00.000");
        assert_eq!(parse_firmware(&firmware).as_deref(), Some("3.00.000"));
        let mut serial = [0u8; 32];
        serial[0] = 0x03;
        serial[5..17].copy_from_slice(b"BL01K1A00000");
        assert_eq!(parse_serial(&serial).as_deref(), Some("BL01K1A00000"));
        assert_eq!(parse_serial(&firmware), None);
        assert_eq!(parse_firmware(&[0xA1, 0, 0]), None);
    }
}
