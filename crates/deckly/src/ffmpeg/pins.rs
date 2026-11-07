//! The FFmpeg builds "Download for Deckly" fetches, pinned by size and
//! SHA-256. Deckly never downloads a build that is not listed here.
//!
//! Source: the FFmpeg-Builds project's static LGPL builds of an FFmpeg
//! release branch. It keeps the last build of each month for two years, so
//! the pins name a month-end build. How to move to a newer one:
//! docs/ffmpeg-downloads.md.

/// How the binary is packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Archive {
    Zip,
    TarXz,
}

#[derive(Debug)]
pub struct Pin {
    /// `std::env::consts::OS` and `ARCH` this build runs on.
    pub os: &'static str,
    pub arch: &'static str,
    pub url: &'static str,
    /// Exact size of the download in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the download.
    pub sha256: &'static str,
    pub archive: Archive,
    /// Path of the FFmpeg program inside the archive.
    pub member: &'static str,
}

/// The FFmpeg version the pinned builds hold.
pub const VERSION: &str = "9.0.2";
pub const LICENSE: &str = "LGPL 2.1 or later";
pub const PROJECT: &str = "FFmpeg-Builds";
pub const PROJECT_PAGE: &str = "https://github.com/FFmpeg-Builds/FFmpeg-Builds";
/// The month-end build the pins come from.
pub const RELEASE: &str = "autobuild-2026-10-31-12-59";

macro_rules! url {
    ($file:literal) => {
        concat!(
            "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/",
            "autobuild-2026-10-31-12-59/",
            $file
        )
    };
}

pub const PINS: &[Pin] = &[
    Pin {
        os: "linux",
        arch: "x86_64",
        url: url!("ffmpeg-n9.0.2-39-g7adfbe40dc-linux64-lgpl-9.0.tar.xz"),
        size: 136_027_168,
        sha256: "6046d6731b26f58887e953561551d254a6eec9fbca4100dfeab56e19af8328d5",
        archive: Archive::TarXz,
        member: "ffmpeg-n9.0.2-39-g7adfbe40dc-linux64-lgpl-9.0/bin/ffmpeg",
    },
    Pin {
        os: "linux",
        arch: "aarch64",
        url: url!("ffmpeg-n9.0.2-39-g7adfbe40dc-linuxarm64-lgpl-9.0.tar.xz"),
        size: 116_688_828,
        sha256: "3d1f66827a73ce5a446312bc4924234dc1515726d3743e2df35c0177e1f82128",
        archive: Archive::TarXz,
        member: "ffmpeg-n9.0.2-39-g7adfbe40dc-linuxarm64-lgpl-9.0/bin/ffmpeg",
    },
    Pin {
        os: "windows",
        arch: "x86_64",
        url: url!("ffmpeg-n9.0.2-39-g7adfbe40dc-win64-lgpl-9.0.zip"),
        size: 168_715_328,
        sha256: "9d37c564e0d01c18cde184a079fa6ef04d7d2783ada41da81d04165cc8f1a650",
        archive: Archive::Zip,
        member: "ffmpeg-n9.0.2-39-g7adfbe40dc-win64-lgpl-9.0/bin/ffmpeg.exe",
    },
    Pin {
        os: "windows",
        arch: "aarch64",
        url: url!("ffmpeg-n9.0.2-39-g7adfbe40dc-winarm64-lgpl-9.0.zip"),
        size: 117_554_579,
        sha256: "d1981518b99a1c05d6b864200b1ac9459b09e2ee174feffe84b6e6bb0bfa79e2",
        archive: Archive::Zip,
        member: "ffmpeg-n9.0.2-39-g7adfbe40dc-winarm64-lgpl-9.0/bin/ffmpeg.exe",
    },
];

/// The pinned build for `os` and `arch`, if there is one.
pub fn for_target(os: &str, arch: &str) -> Option<&'static Pin> {
    PINS.iter().find(|pin| pin.os == os && pin.arch == arch)
}

pub fn for_this_system() -> Option<&'static Pin> {
    for_target(std::env::consts::OS, std::env::consts::ARCH)
}

/// Why there is no download here, for the window.
pub fn unavailable_reason() -> &'static str {
    if cfg!(target_os = "macos") {
        "No LGPL FFmpeg build for macOS has a source Deckly can pin yet. Install \
         FFmpeg with Homebrew or choose an existing FFmpeg."
    } else {
        "Deckly has no FFmpeg download for this system. Install FFmpeg with your \
         package manager or choose an existing FFmpeg."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_are_well_formed() {
        for pin in PINS {
            assert!(
                pin.url
                    .starts_with(&format!("{PROJECT_PAGE}/releases/download/{RELEASE}/"))
            );
            assert_eq!(pin.sha256.len(), 64, "{}", pin.url);
            assert!(
                pin.sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
            assert!(pin.size > 0 && pin.size <= super::super::download::MAX_DOWNLOAD);
            assert!(pin.url.contains("-lgpl-"), "only LGPL builds: {}", pin.url);
            assert!(pin.url.contains(&format!("n{VERSION}-")));
            let file = pin.url.rsplit('/').next().unwrap();
            let top = pin.member.split('/').next().unwrap();
            assert!(file.starts_with(top), "{} is not in {file}", pin.member);
            let name = if pin.os == "windows" {
                "/bin/ffmpeg.exe"
            } else {
                "/bin/ffmpeg"
            };
            assert!(pin.member.ends_with(name));
            match pin.archive {
                Archive::Zip => assert!(file.ends_with(".zip")),
                Archive::TarXz => assert!(file.ends_with(".tar.xz")),
            }
        }
    }

    #[test]
    fn each_system_has_at_most_one_build() {
        assert!(for_target("linux", "x86_64").is_some());
        assert!(for_target("windows", "aarch64").is_some());
        assert!(for_target("macos", "aarch64").is_none());
        for pin in PINS {
            let same = PINS
                .iter()
                .filter(|p| p.os == pin.os && p.arch == pin.arch)
                .count();
            assert_eq!(same, 1);
        }
    }

    /// Compares the pins with the digests GitHub reports for the release.
    /// Run when updating them: `cargo test -p deckly pins_match -- --ignored`.
    #[test]
    #[ignore = "asks GitHub; run when updating the pins"]
    fn pins_match_the_published_digests() {
        let api = format!(
            "https://api.github.com/repos/FFmpeg-Builds/FFmpeg-Builds/releases/tags/{RELEASE}"
        );
        let out = std::process::Command::new("curl")
            .args(["-fsSL", "--proto", "=https", &api])
            .output()
            .unwrap();
        assert!(out.status.success());
        let release: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let assets = release["assets"].as_array().unwrap();
        for pin in PINS {
            let asset = assets
                .iter()
                .find(|a| a["browser_download_url"] == pin.url)
                .unwrap_or_else(|| panic!("{} is not published", pin.url));
            assert_eq!(asset["size"], pin.size, "{}", pin.url);
            assert_eq!(
                asset["digest"],
                format!("sha256:{}", pin.sha256),
                "{}",
                pin.url
            );
        }
    }
}
