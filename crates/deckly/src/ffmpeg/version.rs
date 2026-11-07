//! FFmpeg versions, read from what `ffmpeg -version` prints and from the
//! version strings package managers list.

use std::fmt;

/// Video import needs FFmpeg 7 or newer.
pub const MINIMUM_MAJOR: u32 = 7;
/// FFmpeg majors are small; a larger leading number is a date, as in the
/// `2025-10-30-git-…` versions of nightly builds.
const MAX_MAJOR: u32 = 99;
/// libavcodec's major version minus this is FFmpeg's (libavcodec 61 is
/// FFmpeg 7, 62 is 8, 63 is 9). Used for builds without a release number.
const LIBAVCODEC_OFFSET: u32 = 54;
/// Older libavcodec majors do not follow that offset.
const MIN_LIBAVCODEC: u32 = 58;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    /// From a release number. `false` for a development build, whose major
    /// version comes from its libavcodec.
    pub release: bool,
}

impl Version {
    pub fn supported(self) -> bool {
        self.major >= MINIMUM_MAJOR
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.release {
            write!(f, "{}.{}", self.major, self.minor)
        } else {
            write!(f, "{} (development build)", self.major)
        }
    }
}

/// `7.1.2-x` -> (7, 1). The text must start with a digit; at most six
/// digits a part are read.
fn leading_numbers(text: &str) -> Option<(u32, u32)> {
    let number = |s: &str| -> Option<(u32, usize)> {
        let len = s.bytes().take_while(u8::is_ascii_digit).count();
        if len == 0 || len > 6 {
            return None;
        }
        Some((s.get(..len)?.parse().ok()?, len))
    };
    let (major, len) = number(text)?;
    let minor = text
        .get(len..)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(number)
        .map_or(0, |(minor, _)| minor);
    Some((major, minor))
}

/// The version in `ffmpeg -version` output. `None` when the output is not
/// FFmpeg's.
pub fn from_ffmpeg_output(text: &str) -> Option<Version> {
    let rest = text
        .lines()
        .take(64)
        .find_map(|line| line.trim().strip_prefix("ffmpeg version "))?;
    let token = rest.split_whitespace().next().unwrap_or_default();
    // Release branch builds name their tag: `n7.1.2`.
    let token = token.strip_prefix('n').unwrap_or(token);
    if let Some((major, minor)) = leading_numbers(token).filter(|(m, _)| *m <= MAX_MAJOR) {
        return Some(Version {
            major,
            minor,
            release: true,
        });
    }
    // Development builds (`N-128227-g…`, `2025-10-30-git-…`): libavcodec
    // tells the FFmpeg it belongs to.
    let lavc = text
        .lines()
        .take(64)
        .find_map(|line| line.trim().strip_prefix("libavcodec"))?;
    let (lavc_major, _) = leading_numbers(lavc.trim_start())?;
    (MIN_LIBAVCODEC..=MAX_MAJOR + LIBAVCODEC_OFFSET)
        .contains(&lavc_major)
        .then(|| Version {
            major: lavc_major - LIBAVCODEC_OFFSET,
            minor: 0,
            release: false,
        })
}

/// A package version such as `7:6.1.1-3ubuntu5` (APT, with an epoch),
/// `2:8.0.1-1` (pacman), `7.1.1` (DNF) or `8.0-1.2` (zypper).
pub fn from_package(text: &str) -> Option<Version> {
    let text = text.trim();
    let text = text.split_once(':').map_or(text, |(_, version)| version);
    let (major, minor) = leading_numbers(text).filter(|(m, _)| *m <= MAX_MAJOR)?;
    Some(Version {
        major,
        minor,
        release: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(major: u32, minor: u32) -> Option<Version> {
        Some(Version {
            major,
            minor,
            release: true,
        })
    }

    #[test]
    fn release_builds_are_read_from_the_first_line() {
        let cases = [
            (
                "ffmpeg version n9.0.2 Copyright (c) 2000-2026",
                release(9, 0),
            ),
            ("ffmpeg version 7.1.1-1ubuntu1 Copyright", release(7, 1)),
            ("ffmpeg version 6.1.1-3ubuntu5 Copyright", release(6, 1)),
            (
                "ffmpeg version n9.0.2-39-g7adfbe40dc-20261031 Copyright",
                release(9, 0),
            ),
            ("ffmpeg version 4.4.2-0ubuntu0.22.04.1", release(4, 4)),
            ("ffmpeg version 7 Copyright", release(7, 0)),
        ];
        for (text, expected) in cases {
            assert_eq!(from_ffmpeg_output(text), expected, "{text}");
        }
        assert!(release(7, 0).unwrap().supported());
        assert!(!release(6, 1).unwrap().supported());
    }

    #[test]
    fn development_builds_use_libavcodec() {
        let text = "ffmpeg version N-128227-g4811625b5f-20261031 Copyright\n\
                    built with gcc 15\n\
                    libavutil      61.  1.102 / 61.  1.102\n\
                    libavcodec     63.  1.102 / 63.  1.102\n";
        let version = from_ffmpeg_output(text).unwrap();
        assert_eq!((version.major, version.release), (9, false));
        assert_eq!(version.to_string(), "9 (development build)");
        // A nightly named by its date is not FFmpeg 2025.
        let dated = "ffmpeg version 2025-10-30-git-abc-essentials_build\n\
                     libavcodec     61. 19.101 / 61. 19.101\n";
        assert_eq!(from_ffmpeg_output(dated).unwrap().major, 7);
        let old = "ffmpeg version N-1-gabc\nlibavcodec     60.  3.100\n";
        assert!(!from_ffmpeg_output(old).unwrap().supported());
    }

    #[test]
    fn other_output_is_not_a_version() {
        for text in [
            "",
            "hello",
            "ffmpeg version",
            "ffmpeg version abc",
            "ffmpeg version N-1\nlibavcodec 9999999999",
            "ffmpeg version N-1\nlibavcodec 12.1",
            "avconv version 12",
        ] {
            assert_eq!(from_ffmpeg_output(text), None, "{text:?}");
        }
    }

    #[test]
    fn package_versions_drop_epochs_and_revisions() {
        assert_eq!(from_package("7:6.1.1-3ubuntu5"), release(6, 1));
        assert_eq!(from_package(" 2:8.0.1-1\n"), release(8, 0));
        assert_eq!(from_package("7.1.1"), release(7, 1));
        assert_eq!(from_package("8.0-1.2"), release(8, 0));
        assert_eq!(from_package("(none)"), None);
        assert_eq!(from_package(""), None);
        assert_eq!(from_package("20251030"), None);
    }
}
