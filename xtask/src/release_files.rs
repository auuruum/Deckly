//! `cargo dev release-files`: fills the Homebrew formula and the AUR
//! package with a release's tag and archive checksums.
//!
//! The checksum file is the `sha256sum` output for the release archives
//! (`<hex>  <file name>` per line). The files are written to `--out`
//! (default `dist`): `deckly.rb`, and `aur/` with `PKGBUILD`, `.SRCINFO` and
//! `deckly-bin.install`, ready to copy into the tap and AUR repositories.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use crate::dist::archive_name;
use crate::{path_value, root, value};

pub fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let mut tag = None;
    let mut checksums = None;
    let mut out = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--tag") => tag = Some(value("--tag", &mut args)?),
            Some("--checksums") => checksums = Some(path_value("--checksums", &mut args)?),
            Some("--out") => out = Some(path_value("--out", &mut args)?),
            _ => {
                return Err(format!(
                    "unknown release-files option {}; use cargo dev --help",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    let tag = tag.ok_or("release-files needs --tag")?;
    let checksums: PathBuf = checksums.ok_or("release-files needs --checksums")?;
    let version = version_of(&tag, env!("CARGO_PKG_VERSION"))?;
    let sums = fs::read_to_string(&checksums)
        .map_err(|e| format!("cannot read {}: {e}", checksums.display()))?;
    let files = Release::new(&tag, version, &parse_checksums(&sums))?;

    let out = match out {
        Some(out) => out,
        None => root()?.join("dist"),
    };
    let aur = out.join("aur");
    fs::create_dir_all(&aur).map_err(|e| format!("cannot create {}: {e}", aur.display()))?;
    for (path, text) in [
        (out.join("deckly.rb"), files.formula()?),
        (aur.join("PKGBUILD"), files.pkgbuild()?),
        (aur.join(".SRCINFO"), files.srcinfo()?),
        (
            aur.join("deckly-bin.install"),
            include_str!("../../packaging/aur/deckly-bin.install").to_owned(),
        ),
    ] {
        fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("{}", path.display());
    }
    Ok(())
}

/// The version a tag names, which must match the workspace version so the
/// app reports the version it was released as.
fn version_of<'a>(tag: &'a str, workspace: &str) -> Result<&'a str, String> {
    let version = tag
        .strip_prefix('v')
        .ok_or_else(|| format!("tag {tag} must look like v{workspace}"))?;
    if version == workspace {
        Ok(version)
    } else {
        Err(format!(
            "tag {tag} does not match the workspace version {workspace} in Cargo.toml"
        ))
    }
}

/// File name to SHA-256 from `sha256sum` output (text or binary mode).
fn parse_checksums(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*');
            let valid = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
            valid.then(|| (name.to_owned(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// The values the templates need.
struct Release<'a> {
    tag: &'a str,
    version: &'a str,
    linux_x86_64: String,
    macos_arm64: String,
    macos_x86_64: String,
}

impl<'a> Release<'a> {
    fn new(
        tag: &'a str,
        version: &'a str,
        sums: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let sum = |target: &str| {
            let name = archive_name(target)?;
            sums.get(&name)
                .cloned()
                .ok_or_else(|| format!("the checksums have no line for {name}"))
        };
        Ok(Self {
            tag,
            version,
            linux_x86_64: sum("x86_64-unknown-linux-gnu")?,
            macos_arm64: sum("aarch64-apple-darwin")?,
            macos_x86_64: sum("x86_64-apple-darwin")?,
        })
    }

    fn fill(&self, template: &str) -> Result<String, String> {
        // pacman versions cannot hold `-`; `1.0.0-beta.1` becomes `1.0.0_beta.1`.
        let pkgver = self.version.replace('-', "_");
        let text = template
            .replace("@TAG@", self.tag)
            .replace("@VERSION@", self.version)
            .replace("@PKGVER@", &pkgver)
            .replace("@SHA256_LINUX_X86_64@", &self.linux_x86_64)
            .replace("@SHA256_MACOS_ARM64@", &self.macos_arm64)
            .replace("@SHA256_MACOS_X86_64@", &self.macos_x86_64);
        match leftover_placeholder(&text) {
            Some(name) => Err(format!("template placeholder {name} has no value")),
            None => Ok(text),
        }
    }

    fn formula(&self) -> Result<String, String> {
        self.fill(include_str!("../../packaging/homebrew/deckly.rb.in"))
    }

    fn pkgbuild(&self) -> Result<String, String> {
        self.fill(include_str!("../../packaging/aur/PKGBUILD.in"))
    }

    fn srcinfo(&self) -> Result<String, String> {
        self.fill(include_str!("../../packaging/aur/SRCINFO.in"))
    }
}

/// The first `@NAME@` left in a filled template.
fn leftover_placeholder(text: &str) -> Option<&str> {
    text.split('@').skip(1).step_by(2).find(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sums() -> BTreeMap<String, String> {
        let text = format!(
            "{}  deckly-x86_64-unknown-linux-gnu.tar.gz\n\
             {} *deckly-aarch64-apple-darwin.tar.gz\n\
             {}  deckly-x86_64-apple-darwin.tar.gz\n\
             {}  deckly-x86_64-pc-windows-msvc.zip\n\
             not a checksum line\n",
            "a".repeat(64),
            "B".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
        );
        parse_checksums(&text)
    }

    #[test]
    fn checksums_are_read_from_sha256sum_output() {
        let sums = sums();
        assert_eq!(sums.len(), 4);
        assert_eq!(sums["deckly-aarch64-apple-darwin.tar.gz"], "b".repeat(64));
    }

    #[test]
    fn tag_must_match_the_workspace_version() {
        assert_eq!(version_of("v0.1.0", "0.1.0").unwrap(), "0.1.0");
        assert!(version_of("v0.2.0", "0.1.0").is_err());
        assert!(version_of("0.1.0", "0.1.0").is_err());
    }

    #[test]
    fn templates_are_filled_completely() {
        let sums = sums();
        let release = Release::new("v1.0.0-beta.1", "1.0.0-beta.1", &sums).unwrap();
        let pkgbuild = release.pkgbuild().unwrap();
        assert!(pkgbuild.contains("pkgver=1.0.0_beta.1\n"));
        assert!(pkgbuild.contains(&format!("sha256sums=('{}')", "a".repeat(64))));
        assert!(
            pkgbuild.contains("/download/v1.0.0-beta.1/deckly-x86_64-unknown-linux-gnu.tar.gz")
        );
        let srcinfo = release.srcinfo().unwrap();
        assert!(srcinfo.contains("\tpkgver = 1.0.0_beta.1\n"));
        let formula = release.formula().unwrap();
        assert!(formula.contains("version \"1.0.0-beta.1\""));
        assert!(formula.contains(&format!("sha256 \"{}\"", "b".repeat(64))));
        assert!(formula.contains(&format!("sha256 \"{}\"", "c".repeat(64))));
    }

    #[test]
    fn missing_archives_and_placeholders_are_errors() {
        let mut sums = sums();
        sums.remove("deckly-x86_64-apple-darwin.tar.gz");
        assert!(Release::new("v1.0.0", "1.0.0", &sums).is_err());
        assert_eq!(leftover_placeholder("a @NEW_VALUE@ b"), Some("NEW_VALUE"));
        assert_eq!(leftover_placeholder("mail me @ home, @ work"), None);
    }
}
