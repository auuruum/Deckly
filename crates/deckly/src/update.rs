//! Update checks and one-click updates from Deckly's GitHub releases.
//!
//! Deckly asks GitHub for its latest release once a day while the setting
//! is on, or when the user asks in Preferences. A newer release is offered,
//! never forced: the user picks Update Now, Skip This Version or Later.
//!
//! How Deckly updates depends on how it was installed:
//! - Windows setup: the new setup program runs silently and starts Deckly.
//! - Linux install script: the programs in Deckly's folder are replaced.
//! - macOS install script: the app bundle is replaced.
//! - A package manager (AUR, Homebrew, winget) or a source build: Deckly
//!   only says how to update, so it never fights the package manager.
//!
//! Downloads use the system's `curl` and `tar`, as Node.js downloads do, and
//! are checked against the release's `SHA256SUMS` before anything changes.
//! Everything GitHub sends is untrusted and bounded.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const REPO: &str = "deckly-app/Deckly";
pub const RELEASES_PAGE: &str = "https://github.com/deckly-app/Deckly/releases";
/// How often the automatic check runs while Deckly is open.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The first automatic check waits until startup is done.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(15);
const MAX_RELEASE_JSON: u64 = 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 64 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 300 * 1024 * 1024;
const MAX_TAG_CHARS: usize = 64;
/// Environment switch: the next start waits for this Deckly to quit.
pub const AFTER_UPDATE_ENV: &str = "DECKLY_AFTER_UPDATE";
/// Development switch: pretend this version is the latest release.
const FAKE_ENV: &str = "DECKLY_FAKE_UPDATE";

/// A published release newer than this Deckly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Without the leading `v`, e.g. `0.2.0`.
    pub version: String,
    pub tag: String,
}

impl Release {
    pub fn page(&self) -> String {
        format!("{RELEASES_PAGE}/tag/{}", self.tag)
    }

    fn asset_url(&self, name: &str) -> String {
        format!(
            "https://github.com/{REPO}/releases/download/{}/{name}",
            self.tag
        )
    }
}

/// How this copy of Deckly gets updated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    /// Windows: run the new setup program.
    Setup,
    /// Linux: replace `deckly` and `deckly-web` in this folder.
    Folder(PathBuf),
    /// macOS: replace this `Deckly.app`.
    Bundle(PathBuf),
    /// Deckly does not update itself here; the text says how to.
    Manual(&'static str),
}

/// Where the check or update stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// Ready to finish: Deckly quits and the next one starts.
    Ready(Release, Finish),
    /// The check failed; shown in Preferences only, since being offline is
    /// normal.
    Failed(String),
    /// Updating to this release failed.
    InstallFailed(Release, String),
}

/// What happens once the new version is in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish {
    /// Start this setup program silently; it starts the new Deckly.
    RunSetup(PathBuf),
    /// Start this program, which waits for this Deckly to quit.
    Restart(PathBuf),
}

impl Finish {
    /// Starts the next step. Deckly quits right after.
    pub fn start(&self) -> Result<(), String> {
        let mut command = match self {
            Finish::RunSetup(setup) => {
                let mut c = Command::new(setup);
                c.args([
                    "/VERYSILENT",
                    "/SUPPRESSMSGBOXES",
                    "/NORESTART",
                    "/LAUNCH=1",
                ]);
                c
            }
            Finish::Restart(program) => {
                let mut c = Command::new(program);
                c.env(AFTER_UPDATE_ENV, "1");
                c
            }
        };
        command
            .stdin(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("cannot start the new version: {e}"))
    }
}

struct Inner {
    status: Status,
    method: Method,
    /// The banner was closed with Later; it comes back next start.
    dismissed: bool,
}

/// Shared between the window, Preferences and the checking thread.
#[derive(Clone)]
pub struct Updater {
    inner: Arc<Mutex<Inner>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    work_dir: Option<PathBuf>,
}

impl Updater {
    pub fn new(work_dir: Option<PathBuf>, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let method = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .map(|exe| method_for(&exe, Os::CURRENT, &|p| p.exists()))
            .unwrap_or(Method::Manual(DOWNLOAD_HINT));
        Self {
            inner: Arc::new(Mutex::new(Inner {
                status: Status::Idle,
                method,
                dismissed: false,
            })),
            repaint,
            work_dir,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn status(&self) -> Status {
        self.lock().status.clone()
    }

    pub fn method(&self) -> Method {
        self.lock().method.clone()
    }

    pub fn dismissed(&self) -> bool {
        self.lock().dismissed
    }

    pub fn dismiss(&self) {
        let mut inner = self.lock();
        inner.dismissed = true;
        if matches!(inner.status, Status::InstallFailed(..)) {
            inner.status = Status::Idle;
        }
    }

    fn set(&self, status: Status) {
        self.lock().status = status;
        (self.repaint)();
    }

    /// The new version could not be started.
    pub fn fail(&self, release: Release, error: String) {
        self.set(Status::InstallFailed(release, error));
    }

    /// Takes the finished update, so it is started only once.
    pub fn take_ready(&self) -> Option<(Release, Finish)> {
        let mut inner = self.lock();
        match std::mem::take(&mut inner.status) {
            Status::Ready(release, finish) => Some((release, finish)),
            other => {
                inner.status = other;
                None
            }
        }
    }

    /// Checks for a newer release on a background thread. Does nothing while
    /// a check or download runs.
    pub fn check(&self) {
        {
            let mut inner = self.lock();
            if matches!(
                inner.status,
                Status::Checking | Status::Downloading(_) | Status::Ready(..)
            ) {
                return;
            }
            inner.status = Status::Checking;
            inner.dismissed = false;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            let status = match latest() {
                Ok(Some(release)) if is_newer(&release.version, env!("CARGO_PKG_VERSION")) => {
                    Status::Available(release)
                }
                Ok(_) => Status::UpToDate,
                Err(e) => Status::Failed(e),
            };
            this.set(status);
        });
    }

    /// Checks once shortly after start, then once a day, while `enabled`
    /// says so.
    pub fn start_daily(&self, enabled: impl Fn() -> bool + Send + 'static) {
        if cfg!(debug_assertions) && std::env::var_os(FAKE_ENV).is_none() {
            return;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(FIRST_CHECK_AFTER);
            loop {
                if enabled() && matches!(this.status(), Status::Idle | Status::UpToDate) {
                    this.check();
                }
                std::thread::sleep(CHECK_EVERY);
            }
        });
    }

    /// Downloads and puts in place `release`, on a background thread.
    pub fn install(&self, release: Release) {
        let method = {
            let mut inner = self.lock();
            if matches!(inner.status, Status::Downloading(_) | Status::Ready(..)) {
                return;
            }
            inner.status = Status::Downloading(release.clone());
            inner.method.clone()
        };
        (self.repaint)();
        let this = self.clone();
        std::thread::spawn(move || {
            let result = match &this.work_dir {
                Some(dir) => prepare(&release, &method, dir),
                None => Err("Deckly has no settings folder to download into".to_owned()),
            };
            this.set(match result {
                Ok(finish) => Status::Ready(release, finish),
                Err(e) => Status::InstallFailed(release, e),
            });
        });
    }
}

const DOWNLOAD_HINT: &str = "Download the new version from the release page.";
const SOURCE_HINT: &str = "This Deckly was built from source: pull the new version and rebuild.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Os {
    Windows,
    Linux,
    Mac,
}

impl Os {
    const CURRENT: Os = if cfg!(windows) {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Linux
    };
}

/// Decides how a Deckly at `exe` (symlinks resolved) is updated.
fn method_for(exe: &Path, os: Os, exists: &dyn Fn(&Path) -> bool) -> Method {
    // Cargo marks its target folder with this file.
    if exe.ancestors().any(|dir| exists(&dir.join("CACHEDIR.TAG"))) {
        return Method::Manual(SOURCE_HINT);
    }
    let Some(dir) = exe.parent() else {
        return Method::Manual(DOWNLOAD_HINT);
    };
    let text = exe.to_string_lossy().to_ascii_lowercase();
    match os {
        Os::Windows if text.contains(r"\winget\") => {
            Method::Manual("Deckly came from winget: run winget upgrade Deckly.")
        }
        // Inno Setup's uninstaller sits next to programs it installed.
        Os::Windows if exists(&dir.join("unins000.exe")) => Method::Setup,
        Os::Windows => Method::Manual(DOWNLOAD_HINT),
        Os::Linux if !cfg!(target_arch = "x86_64") => Method::Manual(DOWNLOAD_HINT),
        Os::Linux if exe.starts_with("/usr") || exe.starts_with("/opt") => Method::Manual(
            "Deckly came from a system package: update it with your package manager \
             (for example paru -Syu).",
        ),
        Os::Linux if exists(&dir.join("deckly-web")) => Method::Folder(dir.to_path_buf()),
        Os::Linux => Method::Manual(DOWNLOAD_HINT),
        Os::Mac if text.contains("/cellar/") || text.contains("/homebrew/") => {
            Method::Manual("Deckly came from Homebrew: run brew upgrade deckly.")
        }
        Os::Mac => {
            let bundle = dir
                .parent()
                .and_then(Path::parent)
                .filter(|b| b.extension().is_some_and(|e| e == "app"));
            match bundle {
                Some(bundle) => Method::Bundle(bundle.to_path_buf()),
                None => Method::Manual(DOWNLOAD_HINT),
            }
        }
    }
}

/// Whether `latest` is a newer version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (
        semver::Version::parse(latest),
        semver::Version::parse(current),
    ) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => false,
    }
}

/// The latest published release, `None` when there is none yet.
fn latest() -> Result<Option<Release>, String> {
    if let Some(fake) = std::env::var_os(FAKE_ENV) {
        let version = fake.to_string_lossy().into_owned();
        return Ok(Some(Release {
            tag: format!("v{version}"),
            version,
        }));
    }
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut curl = curl();
    curl.args(["-sSL", "--max-time", "30", "--max-filesize"])
        .arg(MAX_RELEASE_JSON.to_string())
        .args(["-H", "Accept: application/vnd.github+json"])
        .args([
            "-H",
            concat!("User-Agent: Deckly/", env!("CARGO_PKG_VERSION")),
        ])
        .args(["-w", "\n%{http_code}"])
        .arg(url);
    let output = run(curl)?;
    let text = String::from_utf8_lossy(&output);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_ref()));
    match code.trim() {
        "200" => parse_release(body).map(Some),
        // No release published yet.
        "404" => Ok(None),
        "403" | "429" => Err("GitHub is limiting requests; try again later".to_owned()),
        code => Err(format!("GitHub answered with status {code}")),
    }
}

/// Reads the tag of GitHub's latest-release answer.
fn parse_release(json: &str) -> Result<Release, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "GitHub sent an unreadable answer".to_owned())?;
    let tag = value["tag_name"]
        .as_str()
        .filter(|t| t.chars().count() <= MAX_TAG_CHARS)
        .ok_or("the latest release has no tag")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(version)
        .map_err(|_| format!("the latest release tag {tag} is not a version"))?;
    // Tags go into download URLs; keep them to plain version characters.
    if !tag
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
    {
        return Err(format!("the latest release tag {tag} is not a version"));
    }
    Ok(Release {
        version: version.to_owned(),
        tag: tag.to_owned(),
    })
}

/// The release file that updates this system.
fn asset(method: &Method) -> Option<&'static str> {
    match method {
        Method::Setup => Some("deckly-setup-windows-x64.exe"),
        Method::Folder(_) => Some("deckly-x86_64-unknown-linux-gnu.tar.gz"),
        Method::Bundle(_) if cfg!(target_arch = "aarch64") => {
            Some("deckly-aarch64-apple-darwin.tar.gz")
        }
        Method::Bundle(_) => Some("deckly-x86_64-apple-darwin.tar.gz"),
        Method::Manual(_) => None,
    }
}

/// Downloads, checks and puts the new version in place.
fn prepare(release: &Release, method: &Method, config_dir: &Path) -> Result<Finish, String> {
    if let Method::Manual(hint) = method {
        return Err((*hint).to_owned());
    }
    let name = asset(method).ok_or(DOWNLOAD_HINT)?;
    let updates = config_dir.join("updates");
    // Earlier updates leave their setup here; it is only needed while it runs.
    let _ = std::fs::remove_dir_all(&updates);
    let work = updates.join(&release.version);
    std::fs::create_dir_all(&work).map_err(|e| format!("cannot create {}: {e}", work.display()))?;

    let sums_file = work.join("SHA256SUMS");
    fetch(&release.asset_url("SHA256SUMS"), &sums_file, MAX_SUMS_BYTES)?;
    let sums = std::fs::read_to_string(&sums_file).map_err(|e| e.to_string())?;
    let expected = checksum_for(&sums, name)
        .ok_or_else(|| format!("the release has no checksum for {name}"))?;
    let download = work.join(name);
    fetch(&release.asset_url(name), &download, MAX_DOWNLOAD_BYTES)?;
    if sha256_file(&download)? != expected {
        let _ = std::fs::remove_dir_all(&work);
        return Err("the download does not match its checksum".to_owned());
    }

    let result = match method {
        Method::Setup => return Ok(Finish::RunSetup(download)),
        Method::Folder(dir) => replace_folder(&download, &work, dir),
        Method::Bundle(bundle) => replace_bundle(&download, &work, bundle),
        Method::Manual(hint) => Err((*hint).to_owned()),
    };
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Unpacks a release archive into `work` and returns its top folder.
fn unpack(archive: &Path, work: &Path) -> Result<PathBuf, String> {
    let mut tar = system_tool("tar");
    tar.arg("-xzf").arg(archive).arg("-C").arg(work);
    run(tar)?;
    let stem = archive
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".tar.gz"))
        .ok_or("the download is not a .tar.gz archive")?;
    let top = work.join(stem);
    if top.is_dir() {
        Ok(top)
    } else {
        Err(format!("the archive does not hold {stem}"))
    }
}

/// Linux: swaps in the new programs. A rename replaces a running program's
/// file without touching the copy in memory, so this Deckly keeps running
/// until it restarts.
fn replace_folder(archive: &Path, work: &Path, dir: &Path) -> Result<Finish, String> {
    let top = unpack(archive, work)?;
    for name in ["deckly-web", "deckly"] {
        let new = top.join(name);
        let staged = dir.join(format!("{name}.new"));
        std::fs::copy(&new, &staged)
            .map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
        set_executable(&staged)?;
        std::fs::rename(&staged, dir.join(name))
            .map_err(|e| format!("cannot replace {name}: {e}"))?;
    }
    Ok(Finish::Restart(dir.join("deckly")))
}

/// macOS: swaps in the new app bundle, keeping the old one until the new
/// one is in place.
fn replace_bundle(archive: &Path, work: &Path, bundle: &Path) -> Result<Finish, String> {
    let top = unpack(archive, work)?;
    let new = top.join("Deckly.app");
    if !new.join("Contents/MacOS/deckly").is_file() {
        return Err("the archive holds no Deckly.app".to_owned());
    }
    let old = bundle.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old).map_err(|e| format!("cannot move {}: {e}", bundle.display()))?;
    if let Err(e) = std::fs::rename(&new, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(format!("cannot put the new Deckly.app in place: {e}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(Finish::Restart(bundle.join("Contents/MacOS/deckly")))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// The checksum `sha256sum` output lists for `name`.
fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn curl() -> Command {
    let mut curl = system_tool("curl");
    curl.args(["--proto", "=https"]);
    curl
}

fn fetch(url: &str, out: &Path, max_bytes: u64) -> Result<(), String> {
    let mut curl = curl();
    curl.args(["-fsSL", "--max-time", "600", "--max-filesize"])
        .arg(max_bytes.to_string())
        .arg("-o")
        .arg(out)
        .arg(url);
    run(curl).map(|_| ())
}

/// A tool that ships with the system; on Windows it is taken from System32
/// so a program of the same name elsewhere on `PATH` is not used.
pub(crate) fn system_tool(name: &str) -> Command {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Command::new(
            Path::new(&root)
                .join("System32")
                .join(format!("{name}.exe")),
        )
    } else {
        Command::new(name)
    }
}

/// Runs a tool without a console window and returns what it printed.
fn run(mut command: Command) -> Result<Vec<u8>, String> {
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let name = command.get_program().to_string_lossy().into_owned();
    let out = command
        .output()
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{name} failed: {}", err.trim()))
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn method(exe: &str, os: Os, files: &[&str]) -> Method {
        let files: HashSet<PathBuf> = files.iter().map(PathBuf::from).collect();
        method_for(Path::new(exe), os, &|p| files.contains(p))
    }

    #[test]
    fn newer_versions_follow_semver() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0.0", "1.0.0-beta.2"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn release_answers_are_checked() {
        let release = parse_release(r#"{"tag_name": "v0.2.0", "name": "x"}"#).unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.tag, "v0.2.0");
        assert_eq!(
            release.page(),
            "https://github.com/deckly-app/Deckly/releases/tag/v0.2.0"
        );
        assert!(parse_release("{}").is_err());
        assert!(parse_release("not json").is_err());
        assert!(parse_release(r#"{"tag_name": "v1.0.0/../../x"}"#).is_err());
        assert!(parse_release(r#"{"tag_name": "nightly"}"#).is_err());
        let long = format!(r#"{{"tag_name": "v1.0.0-{}"}}"#, "a".repeat(100));
        assert!(parse_release(&long).is_err());
    }

    #[test]
    fn checksums_are_found_by_exact_name() {
        let h = "A".repeat(64);
        let sums = format!(
            "{h}  deckly-setup-windows-x64.exe\n{}  deckly-x86_64-unknown-linux-gnu.tar.gz.bak\n",
            "b".repeat(64)
        );
        assert_eq!(
            checksum_for(&sums, "deckly-setup-windows-x64.exe"),
            Some("a".repeat(64))
        );
        assert_eq!(
            checksum_for(&sums, "deckly-x86_64-unknown-linux-gnu.tar.gz"),
            None
        );
        assert_eq!(checksum_for("short  deckly.zip", "deckly.zip"), None);
    }

    #[test]
    fn source_builds_never_update_themselves() {
        let m = method(
            "/home/me/Deckly/target/release/deckly",
            Os::Linux,
            &[
                "/home/me/Deckly/target/CACHEDIR.TAG",
                "/home/me/Deckly/target/release/deckly-web",
            ],
        );
        assert_eq!(m, Method::Manual(SOURCE_HINT));
    }

    #[test]
    fn windows_updates_through_its_setup_unless_winget_owns_it() {
        let dir = r"C:\Users\me\AppData\Local\Programs\Deckly";
        let exe = format!(r"{dir}\deckly.exe");
        let uninstaller = format!(r"{dir}\unins000.exe");
        // Backslashes separate folders only in Windows paths.
        if cfg!(windows) {
            assert_eq!(method(&exe, Os::Windows, &[&uninstaller]), Method::Setup);
        }
        assert!(matches!(method(&exe, Os::Windows, &[]), Method::Manual(_)));
        let winget = r"C:\Users\me\AppData\Local\Microsoft\WinGet\Packages\Deckly\deckly.exe";
        assert!(
            matches!(method(winget, Os::Windows, &[]), Method::Manual(t) if t.contains("winget"))
        );
    }

    #[test]
    fn linux_replaces_its_own_folder_but_leaves_packages_alone() {
        if !cfg!(target_arch = "x86_64") {
            return;
        }
        let m = method(
            "/home/me/.local/lib/deckly/deckly",
            Os::Linux,
            &["/home/me/.local/lib/deckly/deckly-web"],
        );
        assert_eq!(
            m,
            Method::Folder(PathBuf::from("/home/me/.local/lib/deckly"))
        );
        let m = method(
            "/usr/lib/deckly/deckly",
            Os::Linux,
            &["/usr/lib/deckly/deckly-web"],
        );
        assert!(matches!(m, Method::Manual(t) if t.contains("package manager")));
    }

    #[test]
    fn macos_replaces_its_bundle_but_leaves_homebrew_alone() {
        let m = method(
            "/Users/me/Applications/Deckly.app/Contents/MacOS/deckly",
            Os::Mac,
            &[],
        );
        assert_eq!(
            m,
            Method::Bundle(PathBuf::from("/Users/me/Applications/Deckly.app"))
        );
        let brew = "/opt/homebrew/Cellar/deckly/0.1.0/Deckly.app/Contents/MacOS/deckly";
        assert!(matches!(method(brew, Os::Mac, &[]), Method::Manual(t) if t.contains("brew")));
        assert!(matches!(
            method("/Users/me/deckly", Os::Mac, &[]),
            Method::Manual(_)
        ));
    }

    #[test]
    fn a_finished_update_is_taken_once() {
        let updater = Updater::new(None, Arc::new(|| {}));
        let release = Release {
            version: "0.2.0".to_owned(),
            tag: "v0.2.0".to_owned(),
        };
        updater.set(Status::Ready(
            release,
            Finish::Restart(PathBuf::from("/x/deckly")),
        ));
        assert!(updater.take_ready().is_some());
        assert!(updater.take_ready().is_none());
        updater.set(Status::UpToDate);
        assert!(updater.take_ready().is_none());
        assert_eq!(updater.status(), Status::UpToDate);
    }

    #[cfg(unix)]
    #[test]
    fn linux_update_swaps_both_programs_and_restarts_the_new_one() {
        let root = std::env::temp_dir().join(format!("deckly-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let installed = root.join("lib/deckly");
        let work = root.join("work");
        let packed = root.join("deckly-x86_64-unknown-linux-gnu");
        for dir in [&installed, &work, &packed] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for name in ["deckly", "deckly-web"] {
            std::fs::write(installed.join(name), "old").unwrap();
            std::fs::write(packed.join(name), format!("new {name}")).unwrap();
        }
        let archive = root.join("deckly-x86_64-unknown-linux-gnu.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&root)
            .arg("deckly-x86_64-unknown-linux-gnu")
            .status()
            .unwrap();
        assert!(status.success());

        let finish = replace_folder(&archive, &work, &installed).unwrap();
        assert_eq!(finish, Finish::Restart(installed.join("deckly")));
        for name in ["deckly", "deckly-web"] {
            let path = installed.join(name);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("new {name}")
            );
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
            assert!(!installed.join(format!("{name}.new")).exists());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_manual_install_is_never_downloaded() {
        let release = Release {
            version: "9.9.9".to_owned(),
            tag: "v9.9.9".to_owned(),
        };
        let dir = std::env::temp_dir();
        assert!(prepare(&release, &Method::Manual("hint"), &dir).unwrap_err() == "hint");
    }
}
