//! Installing plugins: `.streamDeckPlugin` archives (zip files holding one
//! `<uuid>.sdPlugin` folder), zip downloads holding either, and plain
//! plugin folders.
//!
//! Archives are untrusted: entry names must stay inside the target folder and
//! sizes and counts are bounded. Files are unpacked into a staging folder
//! next to the plugins and only moved into place once the manifest reads.

use crate::manifest::{Manifest, ManifestError};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 50_000;
const MAX_DEPTH: usize = 32;

#[derive(Debug)]
pub enum InstallError {
    Io(io::Error),
    NotAPlugin(String),
    TooLarge,
    Manifest(ManifestError),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::Io(e) => write!(f, "{e}"),
            InstallError::NotAPlugin(why) => write!(f, "not a plugin: {why}"),
            InstallError::TooLarge => write!(f, "the plugin is too large"),
            InstallError::Manifest(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// A plugin unpacked into the staging folder, ready to be moved into place.
#[derive(Debug)]
pub struct Staged {
    pub manifest: Manifest,
    /// The `<uuid>.sdPlugin` folder inside the staging folder.
    pub dir: PathBuf,
    _root: TempDir,
}

/// A staging folder, removed with whatever is left in it when dropped.
#[derive(Debug)]
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn staging_root(plugins_dir: &Path) -> io::Result<TempDir> {
    let root = plugins_dir.join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root)?;
    Ok(TempDir(root))
}

fn staged(root: TempDir, dir: PathBuf) -> Result<Staged, InstallError> {
    let manifest = Manifest::load(&dir).map_err(InstallError::Manifest)?;
    if manifest.offers_nothing() {
        return Err(InstallError::NotAPlugin(
            "it has no actions and no code Deckly can use".into(),
        ));
    }
    let expected = format!("{}.sdPlugin", manifest.uuid);
    if dir.file_name().and_then(|n| n.to_str()) != Some(expected.as_str()) {
        return Err(InstallError::NotAPlugin(format!(
            "the plugin folder should be named {expected}"
        )));
    }
    Ok(Staged {
        manifest,
        dir,
        _root: root,
    })
}

/// Unpacks a `.streamDeckPlugin` archive into a staging folder. Plain zip
/// downloads work too: the plugin folder may sit inside other folders, or
/// the zip may hold one `.streamDeckPlugin` file.
pub fn stage_archive(archive: &Path, plugins_dir: &Path) -> Result<Staged, InstallError> {
    unpack(archive, plugins_dir, true)
}

fn unpack(archive: &Path, plugins_dir: &Path, open_inner: bool) -> Result<Staged, InstallError> {
    let mut file = File::open(archive)?;
    let mut magic = [0u8; 6];
    if file.read_exact(&mut magic).is_ok() && &magic == b"ELGATO" {
        return Err(InstallError::Manifest(ManifestError::Encrypted));
    }
    let mut zip = zip::ZipArchive::new(File::open(archive)?)
        .map_err(|e| InstallError::NotAPlugin(e.to_string()))?;
    if zip.len() > MAX_FILES {
        return Err(InstallError::TooLarge);
    }
    // The staging folder is removed again on any error.
    let root = staging_root(plugins_dir)?;
    let mut total = 0u64;
    let mut top: Option<PathBuf> = None;
    let mut inner = None;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| InstallError::NotAPlugin(e.to_string()))?;
        let Some(name) = entry.enclosed_name() else {
            return Err(InstallError::NotAPlugin(format!(
                "unsafe entry name {:?}",
                entry.name()
            )));
        };
        let Some(parts) = name
            .components()
            .map(|c| c.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        // Archives made on a Mac carry a resource-fork folder.
        if parts.first() == Some(&"__MACOSX") {
            continue;
        }
        let Some(at) = parts.iter().position(|p| p.ends_with(".sdPlugin")) else {
            let packed = name
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("streamDeckPlugin"));
            if packed && !entry.is_dir() && inner.replace(i).is_some() {
                return Err(InstallError::NotAPlugin(
                    "more than one plugin in the archive".into(),
                ));
            }
            continue;
        };
        let folder: PathBuf = parts[..=at].iter().collect();
        match &top {
            None => top = Some(folder),
            Some(t) if *t != folder => {
                return Err(InstallError::NotAPlugin(
                    "more than one plugin folder in the archive".into(),
                ));
            }
            Some(_) => {}
        }
        if parts.len() - at > MAX_DEPTH {
            return Err(InstallError::NotAPlugin("folders nest too deep".into()));
        }
        let out: PathBuf = std::iter::once(root.0.as_os_str())
            .chain(parts[at..].iter().map(|p| p.as_ref()))
            .collect();
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let budget = MAX_TOTAL_BYTES - total;
        let mut dest = File::create(&out)?;
        let written = io::copy(&mut (&mut entry).take(budget + 1), &mut dest)?;
        if written > budget {
            return Err(InstallError::TooLarge);
        }
        total += written;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            // Keep the executable bits; never setuid or world-writable.
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode & 0o755))?;
        }
    }
    if top.is_none()
        && open_inner
        && let Some(i) = inner
    {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| InstallError::NotAPlugin(e.to_string()))?;
        let path = root.0.join("inner.streamDeckPlugin");
        let written = io::copy(
            &mut (&mut entry).take(MAX_TOTAL_BYTES + 1),
            &mut File::create(&path)?,
        )?;
        if written > MAX_TOTAL_BYTES {
            return Err(InstallError::TooLarge);
        }
        // One level only, so nested archives cannot recurse.
        return unpack(&path, plugins_dir, false);
    }
    let dir = top
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| root.0.join(n))
        .ok_or_else(|| InstallError::NotAPlugin("no .sdPlugin folder inside".into()))?;
    staged(root, dir)
}

/// Copies a `<uuid>.sdPlugin` folder, or the one such folder inside `src`
/// (an unpacked download), into a staging folder.
pub fn stage_folder(src: &Path, plugins_dir: &Path) -> Result<Staged, InstallError> {
    let Some(name) = src
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.ends_with(".sdPlugin"))
        .map(str::to_owned)
    else {
        let mut inner = std::fs::read_dir(src)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.extension().is_some_and(|e| e == "sdPlugin"));
        return match (inner.next(), inner.next()) {
            (Some(dir), None) => stage_folder(&dir, plugins_dir),
            _ => Err(InstallError::NotAPlugin(
                "the folder must be a .sdPlugin folder or hold one".into(),
            )),
        };
    };
    // Check the manifest before copying anything.
    Manifest::load(src).map_err(InstallError::Manifest)?;
    let root = staging_root(plugins_dir)?;
    let dir = root.0.join(&name);
    let mut budget = (MAX_TOTAL_BYTES, MAX_FILES);
    copy_dir(src, &dir, &mut budget, 0)?;
    staged(root, dir)
}

fn copy_dir(
    src: &Path,
    dst: &Path,
    budget: &mut (u64, usize),
    depth: usize,
) -> Result<(), InstallError> {
    if depth > MAX_DEPTH {
        return Err(InstallError::NotAPlugin("folders nest too deep".into()));
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        // Symbolic links could point outside the plugin; skip them.
        let kind = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &to, budget, depth + 1)?;
        } else if kind.is_file() {
            let len = entry.metadata()?.len();
            if len > budget.0 || budget.1 == 0 {
                return Err(InstallError::TooLarge);
            }
            budget.0 -= len;
            budget.1 -= 1;
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Moves a staged plugin into `plugins_dir`, replacing an installed copy.
/// The running plugin must be stopped first. Returns the plugin id.
pub fn commit(staged: Staged, plugins_dir: &Path) -> Result<String, InstallError> {
    let uuid = staged.manifest.uuid.clone();
    let target = plugins_dir.join(format!("{uuid}.sdPlugin"));
    if target.exists() {
        remove(plugins_dir, &uuid)?;
    }
    // Archives and backups made on Windows carry no executable bit.
    #[cfg(unix)]
    if let Ok(crate::manifest::Entry::Native(program)) = staged.manifest.entry(&staged.dir) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o755))?;
    }
    retry(|| std::fs::rename(&staged.dir, &target))?;
    Ok(uuid)
}

/// Deletes an installed plugin folder. It is renamed first so a half-deleted
/// folder never looks like an installed plugin.
pub fn remove(plugins_dir: &Path, uuid: &str) -> Result<(), InstallError> {
    if !crate::manifest::valid_uuid(uuid) {
        return Err(InstallError::NotAPlugin(format!("bad plugin id {uuid}")));
    }
    let dir = plugins_dir.join(format!("{uuid}.sdPlugin"));
    let trash = plugins_dir.join(format!(".trash-{}", uuid::Uuid::new_v4().simple()));
    retry(|| std::fs::rename(&dir, &trash))?;
    std::fs::remove_dir_all(&trash)?;
    Ok(())
}

/// Removes staging and trash folders left behind by an earlier crash.
pub fn clean(plugins_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".staging-") || name.starts_with(".trash-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A just-stopped plugin can keep its files locked for a moment on Windows.
fn retry(mut op: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut last = Ok(());
    for _ in 0..20 {
        last = op();
        if last.is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deckly-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn archive(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    const MANIFEST: &[u8] = br#"{"UUID":"com.example.p","Name":"P","CodePath":"bin/p.js"}"#;

    #[test]
    fn installs_and_replaces_an_archive() {
        let dir = temp("install");
        let plugins = dir.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let file = dir.join("p.streamDeckPlugin");
        archive(
            &file,
            &[
                ("com.example.p.sdPlugin/manifest.json", MANIFEST),
                ("com.example.p.sdPlugin/bin/p.js", b"// v1"),
                ("__MACOSX/junk", b"x"),
            ],
        );
        let staged = stage_archive(&file, &plugins).unwrap();
        assert_eq!(staged.manifest.name, "P");
        assert_eq!(commit(staged, &plugins).unwrap(), "com.example.p");
        let installed = plugins.join("com.example.p.sdPlugin");
        assert!(installed.join("bin/p.js").is_file());
        // A second install replaces the first.
        archive(
            &file,
            &[
                ("com.example.p.sdPlugin/manifest.json", MANIFEST),
                ("com.example.p.sdPlugin/bin/p.js", b"// v2"),
            ],
        );
        commit(stage_archive(&file, &plugins).unwrap(), &plugins).unwrap();
        assert_eq!(std::fs::read(installed.join("bin/p.js")).unwrap(), b"// v2");
        // Nothing is left in staging.
        let leftovers: Vec<_> = std::fs::read_dir(&plugins)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, ["com.example.p.sdPlugin"]);
        remove(&plugins, "com.example.p").unwrap();
        assert!(!installed.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_bad_archives() {
        let dir = temp("bad-archive");
        let plugins = dir.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let file = dir.join("p.streamDeckPlugin");
        // Escaping names never land outside the staging folder.
        archive(
            &file,
            &[
                ("com.example.p.sdPlugin/manifest.json", MANIFEST),
                ("../../evil.txt", b"x"),
            ],
        );
        assert!(stage_archive(&file, &plugins).is_err());
        assert!(!dir.join("evil.txt").exists());
        // Folder name must match the manifest id.
        archive(&file, &[("com.other.sdPlugin/manifest.json", MANIFEST)]);
        assert!(stage_archive(&file, &plugins).is_err());
        // Nothing in it that Deckly could use.
        archive(
            &file,
            &[(
                "com.example.p.sdPlugin/manifest.json",
                br#"{"UUID":"com.example.p","Name":"P"}"#,
            )],
        );
        assert!(stage_archive(&file, &plugins).is_err());
        // No plugin folder at all.
        archive(&file, &[("readme.txt", b"x")]);
        assert!(stage_archive(&file, &plugins).is_err());
        // Encrypted marketplace downloads.
        std::fs::write(&file, b"ELGATO\x01\x00rest").unwrap();
        assert!(matches!(
            stage_archive(&file, &plugins),
            Err(InstallError::Manifest(ManifestError::Encrypted))
        ));
        std::fs::write(&file, b"not a zip").unwrap();
        assert!(stage_archive(&file, &plugins).is_err());
        // Every failure cleaned up after itself.
        assert_eq!(std::fs::read_dir(&plugins).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installs_a_folder() {
        let dir = temp("folder");
        let plugins = dir.join("plugins");
        let src = dir.join("src/com.example.p.sdPlugin");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(src.join("manifest.json"), MANIFEST).unwrap();
        std::fs::write(src.join("bin/p.js"), b"x").unwrap();
        let staged = stage_folder(&src, &plugins).unwrap();
        commit(staged, &plugins).unwrap();
        assert!(plugins.join("com.example.p.sdPlugin/bin/p.js").is_file());
        // A folder holding one plugin folder (an unpacked download).
        commit(stage_folder(&dir.join("src"), &plugins).unwrap(), &plugins).unwrap();
        std::fs::create_dir_all(dir.join("src/com.other.sdPlugin")).unwrap();
        assert!(stage_folder(&dir.join("src"), &plugins).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installs_plain_zip_downloads() {
        let dir = temp("zip");
        let plugins = dir.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        // The plugin folder inside a release folder, with Mac leftovers.
        let file = dir.join("p.zip");
        archive(
            &file,
            &[
                ("p-1.0/com.example.p.sdPlugin/manifest.json", MANIFEST),
                ("p-1.0/com.example.p.sdPlugin/bin/p.js", b"x"),
                ("p-1.0/readme.txt", b"x"),
                (
                    "__MACOSX/p-1.0/com.example.p.sdPlugin/._manifest.json",
                    b"x",
                ),
            ],
        );
        commit(stage_archive(&file, &plugins).unwrap(), &plugins).unwrap();
        assert!(plugins.join("com.example.p.sdPlugin/bin/p.js").is_file());
        // A zip holding a .streamDeckPlugin file.
        let packed = dir.join("p.streamDeckPlugin");
        archive(
            &packed,
            &[("com.example.p.sdPlugin/manifest.json", MANIFEST)],
        );
        let bytes = std::fs::read(&packed).unwrap();
        archive(&file, &[("dl/p.streamDeckPlugin", &bytes)]);
        assert_eq!(stage_archive(&file, &plugins).unwrap().manifest.name, "P");
        // Archives nested twice are not opened.
        let twice = dir.join("twice.streamDeckPlugin");
        std::fs::copy(&file, &twice).unwrap();
        archive(
            &file,
            &[("twice.streamDeckPlugin", &std::fs::read(&twice).unwrap())],
        );
        assert!(stage_archive(&file, &plugins).is_err());
        // Two plugins in one zip are refused.
        archive(
            &file,
            &[
                ("a/com.example.p.sdPlugin/manifest.json", MANIFEST),
                ("b/com.example.p.sdPlugin/manifest.json", MANIFEST),
            ],
        );
        assert!(stage_archive(&file, &plugins).is_err());
        let staging: Vec<_> = std::fs::read_dir(&plugins)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(staging, ["com.example.p.sdPlugin"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
