//! "Install through the system": finds the package manager, checks what
//! version of FFmpeg its repositories hold, and builds the exact command.
//!
//! The repository check reads only the package manager's local cache, so
//! nothing goes to the network until the user clicks Install. winget and
//! Homebrew have no such cache; their FFmpeg packages are current releases,
//! and the version is checked after installing like on every route.
//! AUR helpers are not used.

use super::version::{self, Version};
use crate::os::elevate::{self, Os};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_QUERY_OUTPUT: usize = 256 * 1024;
const WINGET_PACKAGE: &str = "Gyan.FFmpeg";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Winget,
    Brew,
}

impl Manager {
    pub fn label(self) -> &'static str {
        match self {
            Manager::Apt => "APT",
            Manager::Dnf => "DNF",
            Manager::Pacman => "pacman",
            Manager::Zypper => "zypper",
            Manager::Winget => "winget",
            Manager::Brew => "Homebrew",
        }
    }

    /// The program that installs, and the one that queries the repository.
    fn programs(self) -> (&'static str, Option<&'static str>) {
        match self {
            Manager::Apt => ("apt-get", Some("apt-cache")),
            Manager::Dnf => ("dnf", Some("dnf")),
            Manager::Pacman => ("pacman", Some("pacman")),
            Manager::Zypper => ("zypper", Some("zypper")),
            Manager::Winget => ("winget", None),
            Manager::Brew => ("brew", None),
        }
    }

    /// Arguments of the repository query; offline, in the C locale.
    fn query_args(self) -> &'static [&'static str] {
        match self {
            Manager::Apt => &["policy", "ffmpeg"],
            Manager::Dnf => &[
                "--cacheonly",
                "--quiet",
                "repoquery",
                "--latest-limit=1",
                "--queryformat=%{name} %{version}\\n",
                "ffmpeg",
            ],
            Manager::Pacman => &["-Si", "ffmpeg"],
            // openSUSE names FFmpeg packages by their major version.
            Manager::Zypper => &[
                "--non-interactive",
                "--no-refresh",
                "info",
                "ffmpeg-8",
                "ffmpeg-7",
            ],
            Manager::Winget | Manager::Brew => &[],
        }
    }

    /// The package and version the repository offers, from the query's
    /// output. Only the package named `ffmpeg` counts on DNF: Fedora's own
    /// `ffmpeg-free` cannot decode H.264.
    pub fn parse_query(self, out: &str) -> Option<(String, Version)> {
        match self {
            Manager::Apt => {
                let candidate = field(out, "Candidate")?;
                Some(("ffmpeg".into(), version::from_package(candidate)?))
            }
            Manager::Pacman => Some((
                "ffmpeg".into(),
                version::from_package(field(out, "Version")?)?,
            )),
            Manager::Dnf => out.lines().find_map(|line| {
                let mut words = line.split_whitespace();
                (words.next()? == "ffmpeg")
                    .then(|| words.next())
                    .flatten()
                    .and_then(version::from_package)
                    .map(|v| ("ffmpeg".to_owned(), v))
            }),
            Manager::Zypper => {
                let mut best: Option<(String, Version)> = None;
                let mut name = None;
                for line in out.lines().take(10_000) {
                    let Some((key, value)) = line.split_once(':') else {
                        continue;
                    };
                    match key.trim() {
                        "Name" => name = Some(value.trim()),
                        "Version" => {
                            let found = name.take().filter(|n| n.starts_with("ffmpeg-"));
                            if let (Some(n), Some(v)) = (found, version::from_package(value))
                                && best
                                    .as_ref()
                                    .is_none_or(|(_, b)| (v.major, v.minor) > (b.major, b.minor))
                            {
                                best = Some((n.chars().take(32).collect(), v));
                            }
                        }
                        _ => {}
                    }
                }
                best
            }
            Manager::Winget | Manager::Brew => None,
        }
    }

    fn default_package(self) -> &'static str {
        match self {
            Manager::Zypper => "ffmpeg-8",
            Manager::Winget => WINGET_PACKAGE,
            _ => "ffmpeg",
        }
    }

    /// The unattended install command, program first, without elevation.
    fn install_argv(self, program: &Path, package: &str) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();
        let words: &[&str] = match self {
            Manager::Apt => {
                // pkexec clears the environment; `env` sets what apt needs.
                argv.extend([
                    "/usr/bin/env".into(),
                    "DEBIAN_FRONTEND=noninteractive".into(),
                ]);
                &["install", "-y"]
            }
            // RPM Fusion's ffmpeg replaces Fedora's ffmpeg-free libraries.
            Manager::Dnf => &["install", "-y", "--allowerasing"],
            Manager::Pacman => &["-S", "--needed", "--noconfirm"],
            Manager::Zypper => &["--non-interactive", "install"],
            Manager::Winget => &[
                "install",
                "--exact",
                "--source",
                "winget",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
                "--id",
            ],
            Manager::Brew => &["install"],
        };
        argv.push(program.as_os_str().to_owned());
        argv.extend(words.iter().map(OsString::from));
        argv.push(package.into());
        argv
    }

    /// The command to type in a terminal.
    fn terminal_command(self, package: &str) -> String {
        match self {
            Manager::Apt => format!("sudo apt-get install {package}"),
            Manager::Dnf => format!("sudo dnf install --allowerasing {package}"),
            Manager::Pacman => format!("sudo pacman -S --needed {package}"),
            Manager::Zypper => format!("sudo zypper install {package}"),
            Manager::Winget => format!("winget install --exact --source winget --id {package}"),
            Manager::Brew => format!("brew install {package}"),
        }
    }
}

/// `Key: value` lines, as APT, pacman and zypper print them.
fn field<'a>(out: &'a str, key: &str) -> Option<&'a str> {
    out.lines().take(10_000).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// What the repositories hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Repository {
    Has(Version),
    TooOld(Version),
    /// No full FFmpeg package.
    Missing,
    /// Not checked offline; the version is checked after installing.
    NotChecked,
    Unknown(String),
}

/// The route as the window shows it.
#[derive(Clone, Debug)]
pub struct Plan {
    pub manager: Manager,
    pub package: String,
    pub repository: Repository,
    /// The command Install runs, or why Install is not offered.
    pub install: Result<Vec<OsString>, String>,
    /// For Copy.
    pub terminal: String,
}

impl Plan {
    /// The Install command as text, for showing before it runs.
    pub fn shown_command(&self) -> Option<String> {
        let argv = self.install.as_ref().ok()?;
        Some(
            argv.iter()
                .map(|a| {
                    let a = a.to_string_lossy();
                    if cfg!(windows) {
                        if a.is_empty() || a.contains(' ') {
                            format!("\"{a}\"")
                        } else {
                            a.into_owned()
                        }
                    } else if a.is_empty() || a.contains([' ', '"', '\'', '\\', '$', '`']) {
                        format!("'{}'", a.replace('\'', r"'\''"))
                    } else {
                        a.into_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    pub fn command(&self) -> Option<Command> {
        let argv = self.install.as_ref().ok()?;
        let (program, args) = argv.split_first()?;
        let mut command = Command::new(program);
        command.args(args).env("LC_ALL", "C");
        Some(command)
    }
}

/// Which package manager `os` uses, found with `find` (program name ->
/// installed path). Linux distributions are tried in a fixed order.
pub fn detect(os: Os, find: &dyn Fn(&str) -> Option<PathBuf>) -> Option<(Manager, PathBuf)> {
    let managers: &[Manager] = match os {
        Os::Linux => &[Manager::Apt, Manager::Dnf, Manager::Pacman, Manager::Zypper],
        Os::Windows => &[Manager::Winget],
        Os::Mac => &[Manager::Brew],
        Os::Other => &[],
    };
    managers.iter().find_map(|&m| {
        let (program, query) = m.programs();
        let path = find(program)?;
        // APT's query program comes with it; require it anyway.
        query.is_none_or(|q| find(q).is_some()).then_some((m, path))
    })
}

/// What the repository query printed.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Answer {
    /// The repository has no package: the query succeeded without listing
    /// one, or failed saying so (pacman exits with an error).
    fn says_missing(&self) -> bool {
        self.success || format!("{}{}", self.stdout, self.stderr).contains("not found")
    }

    /// The first line of what a failed query printed.
    fn failure(&self) -> String {
        let line = [&self.stderr, &self.stdout]
            .into_iter()
            .flat_map(|text| text.lines())
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("the query failed");
        line.chars().take(200).collect()
    }
}

/// Builds the plan from what was found. `query` is the repository query's
/// answer, or why it could not run; `None` when there is no query.
pub fn plan_with(
    os: Os,
    manager: Manager,
    program: &Path,
    query: Option<Result<Answer, String>>,
    pkexec: Option<&Path>,
) -> Plan {
    let default = || manager.default_package().to_owned();
    let (package, repository) = match query {
        None => (default(), Repository::NotChecked),
        Some(Err(e)) => (default(), Repository::Unknown(e)),
        Some(Ok(answer)) => match manager.parse_query(&answer.stdout) {
            Some((package, v)) if v.supported() => (package, Repository::Has(v)),
            Some((package, v)) => (package, Repository::TooOld(v)),
            None if answer.says_missing() => (default(), Repository::Missing),
            None => (default(), Repository::Unknown(answer.failure())),
        },
    };
    let install = match &repository {
        Repository::Has(_) | Repository::NotChecked => {
            elevate::command_line(os, pkexec, manager.install_argv(program, &package))
        }
        Repository::TooOld(v) => Err(format!(
            "Your repositories have FFmpeg {v}; video import needs {} or newer.",
            version::MINIMUM_MAJOR
        )),
        Repository::Missing if manager == Manager::Dnf => Err(
            "Your repositories have no full FFmpeg. Fedora's ffmpeg-free cannot decode \
             H.264; the ffmpeg package comes from RPM Fusion."
                .into(),
        ),
        Repository::Missing => Err("Your repositories have no FFmpeg package.".into()),
        Repository::Unknown(e) => Err(format!("Deckly could not check your repositories: {e}")),
    };
    Plan {
        manager,
        terminal: manager.terminal_command(&package),
        package,
        repository,
        install,
    }
}

/// Finds a program the way the system route needs: absolute folders on PATH,
/// then the folders winget and Homebrew use, which an app started from the
/// desktop may not have on its PATH.
fn find_program(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|d| d.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Microsoft\WindowsApps"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    }
    dirs.into_iter()
        .map(|d| d.join(&file))
        .find(|f| f.is_file())
}

/// Detects the package manager and checks its repositories. Runs local
/// queries only; call from a worker thread. `None` without a known package
/// manager.
pub fn plan() -> Option<Plan> {
    let os = Os::CURRENT;
    let (manager, program) = detect(os, &find_program)?;
    let query = manager.programs().1.and_then(find_program).map(|query| {
        let mut command = Command::new(query);
        command
            .args(manager.query_args())
            .env("LC_ALL", "C")
            .env("LANG", "C");
        super::process::output(command, QUERY_TIMEOUT, MAX_QUERY_OUTPUT).map(|out| Answer {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    });
    Some(plan_with(
        os,
        manager,
        &program,
        query,
        elevate::pkexec().as_deref(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn answer(stdout: impl Into<String>) -> Answer {
        Answer {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    #[test]
    fn a_failed_query_is_not_a_missing_package() {
        let pkexec = Some(Path::new("/usr/bin/pkexec"));
        let pacman = Path::new("/usr/bin/pacman");
        // pacman without gpg cannot read its databases.
        let broken = Answer {
            success: false,
            stdout: String::new(),
            stderr:
                "error: GPGME error: Invalid crypto engine\nerror: database 'core' is not valid\n"
                    .into(),
        };
        let plan = plan_with(Os::Linux, Manager::Pacman, pacman, Some(Ok(broken)), pkexec);
        assert_eq!(
            plan.repository,
            Repository::Unknown("error: GPGME error: Invalid crypto engine".into())
        );
        assert!(plan.install.unwrap_err().contains("could not check"));
        // pacman also fails when the package does not exist.
        let missing = Answer {
            success: false,
            stdout: String::new(),
            stderr: "error: package 'ffmpeg' was not found\n".into(),
        };
        let plan = plan_with(
            Os::Linux,
            Manager::Pacman,
            pacman,
            Some(Ok(missing)),
            pkexec,
        );
        assert_eq!(plan.repository, Repository::Missing);
        let silent = Answer::default();
        assert_eq!(silent.failure(), "the query failed");
    }

    fn finder(programs: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let map: HashMap<String, PathBuf> = programs
            .iter()
            .map(|(n, p)| ((*n).to_owned(), PathBuf::from(p)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn package_managers_are_detected_per_system() {
        let debian = finder(&[
            ("apt-get", "/usr/bin/apt-get"),
            ("apt-cache", "/usr/bin/apt-cache"),
        ]);
        assert_eq!(
            detect(Os::Linux, &debian),
            Some((Manager::Apt, PathBuf::from("/usr/bin/apt-get")))
        );
        let fedora = finder(&[("dnf", "/usr/bin/dnf")]);
        assert_eq!(detect(Os::Linux, &fedora).map(|d| d.0), Some(Manager::Dnf));
        let arch = finder(&[("pacman", "/usr/bin/pacman")]);
        assert_eq!(detect(Os::Linux, &arch).map(|d| d.0), Some(Manager::Pacman));
        let suse = finder(&[("zypper", "/usr/bin/zypper")]);
        assert_eq!(detect(Os::Linux, &suse).map(|d| d.0), Some(Manager::Zypper));
        // apt-get without apt-cache cannot be checked.
        let broken = finder(&[("apt-get", "/usr/bin/apt-get")]);
        assert_eq!(detect(Os::Linux, &broken), None);
        let windows = finder(&[("winget", r"C:\winget.exe")]);
        assert_eq!(
            detect(Os::Windows, &windows).map(|d| d.0),
            Some(Manager::Winget)
        );
        assert_eq!(detect(Os::Linux, &windows), None);
        let mac = finder(&[("brew", "/opt/homebrew/bin/brew"), ("pacman", "/x")]);
        assert_eq!(detect(Os::Mac, &mac).map(|d| d.0), Some(Manager::Brew));
        assert_eq!(detect(Os::Other, &mac), None);
    }

    #[test]
    fn repository_versions_are_parsed() {
        let apt = "ffmpeg:\n  Installed: (none)\n  Candidate: 7:7.1.1-1+b1\n  Version table:\n";
        assert_eq!(Manager::Apt.parse_query(apt).unwrap().1.major, 7);
        let apt_old = "ffmpeg:\n  Installed: (none)\n  Candidate: 7:6.1.1-3ubuntu5\n";
        assert_eq!(Manager::Apt.parse_query(apt_old).unwrap().1.major, 6);
        assert_eq!(
            Manager::Apt.parse_query("ffmpeg:\n  Candidate: (none)\n"),
            None
        );
        assert_eq!(Manager::Apt.parse_query(""), None);

        let pacman =
            "Repository      : extra\nName            : ffmpeg\nVersion         : 2:8.0.1-1\n";
        assert_eq!(Manager::Pacman.parse_query(pacman).unwrap().1.major, 8);
        assert_eq!(
            Manager::Pacman.parse_query("error: package 'ffmpeg' was not found\n"),
            None
        );

        assert_eq!(
            Manager::Dnf.parse_query("ffmpeg 7.1.1\n").unwrap().1.minor,
            1
        );
        // Fedora without RPM Fusion lists only ffmpeg-free.
        assert_eq!(Manager::Dnf.parse_query("ffmpeg-free 7.1.1\n"), None);
        assert_eq!(Manager::Dnf.parse_query(""), None);

        let zypper = "Information for package ffmpeg-8:\nRepository     : Packman\n\
                      Name           : ffmpeg-8\nVersion        : 8.0-1.3\n\n\
                      Information for package ffmpeg-7:\nName           : ffmpeg-7\n\
                      Version        : 7.1.1-2.1\n";
        let (package, v) = Manager::Zypper.parse_query(zypper).unwrap();
        assert_eq!((package.as_str(), v.major), ("ffmpeg-8", 8));
        assert_eq!(
            Manager::Zypper.parse_query("package 'ffmpeg-8' not found.\n"),
            None
        );
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn linux_installs_through_pkexec_only_with_ffmpeg_7_in_the_repository() {
        let pkexec = Some(Path::new("/usr/bin/pkexec"));
        let apt = "  Candidate: 7:7.1.1-1\n".to_owned();
        let plan = plan_with(
            Os::Linux,
            Manager::Apt,
            Path::new("/usr/bin/apt-get"),
            Some(Ok(answer(apt.clone()))),
            pkexec,
        );
        assert_eq!(
            strings(plan.install.as_ref().unwrap()),
            [
                "/usr/bin/pkexec",
                "/usr/bin/env",
                "DEBIAN_FRONTEND=noninteractive",
                "/usr/bin/apt-get",
                "install",
                "-y",
                "ffmpeg"
            ]
        );
        assert_eq!(
            plan.shown_command().unwrap(),
            "/usr/bin/pkexec /usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get install -y ffmpeg"
        );
        assert_eq!(plan.terminal, "sudo apt-get install ffmpeg");

        // Without pkexec, only Copy is offered.
        let plan = plan_with(
            Os::Linux,
            Manager::Apt,
            Path::new("/usr/bin/apt-get"),
            Some(Ok(answer(apt))),
            None,
        );
        assert!(plan.install.unwrap_err().contains("pkexec"));

        let old = "  Candidate: 7:6.1.1-3ubuntu5\n".to_owned();
        let plan = plan_with(
            Os::Linux,
            Manager::Apt,
            Path::new("/usr/bin/apt-get"),
            Some(Ok(answer(old))),
            pkexec,
        );
        assert_eq!(
            plan.repository,
            Repository::TooOld(version::from_package("6.1").unwrap())
        );
        assert!(plan.install.unwrap_err().contains("FFmpeg 6.1"));
        assert_eq!(plan.terminal, "sudo apt-get install ffmpeg");

        let plan = plan_with(
            Os::Linux,
            Manager::Dnf,
            Path::new("/usr/bin/dnf"),
            Some(Ok(answer("ffmpeg-free 7.1.1"))),
            pkexec,
        );
        assert_eq!(plan.repository, Repository::Missing);
        assert!(plan.install.unwrap_err().contains("ffmpeg-free"));

        let plan = plan_with(
            Os::Linux,
            Manager::Pacman,
            Path::new("/usr/bin/pacman"),
            Some(Err("timed out".into())),
            pkexec,
        );
        assert!(plan.install.unwrap_err().contains("timed out"));

        let zypper = "Name : ffmpeg-7\nVersion : 7.1.1-2.1\n".to_owned();
        let plan = plan_with(
            Os::Linux,
            Manager::Zypper,
            Path::new("/usr/bin/zypper"),
            Some(Ok(answer(zypper))),
            pkexec,
        );
        assert_eq!(
            strings(&plan.install.unwrap()),
            [
                "/usr/bin/pkexec",
                "/usr/bin/zypper",
                "--non-interactive",
                "install",
                "ffmpeg-7"
            ]
        );
        assert_eq!(plan.terminal, "sudo zypper install ffmpeg-7");
    }

    #[test]
    fn winget_and_homebrew_run_as_the_user_without_an_offline_check() {
        let plan = plan_with(
            Os::Windows,
            Manager::Winget,
            Path::new(r"C:\winget.exe"),
            None,
            None,
        );
        assert_eq!(plan.repository, Repository::NotChecked);
        let argv = strings(&plan.install.unwrap());
        assert_eq!(argv.first().map(String::as_str), Some(r"C:\winget.exe"));
        assert_eq!(argv.last().map(String::as_str), Some("Gyan.FFmpeg"));
        assert!(argv.contains(&"--disable-interactivity".to_owned()));
        assert!(plan.terminal.starts_with("winget install"));

        let plan = plan_with(
            Os::Mac,
            Manager::Brew,
            Path::new("/opt/homebrew/bin/brew"),
            None,
            None,
        );
        assert_eq!(
            strings(&plan.install.unwrap()),
            ["/opt/homebrew/bin/brew", "install", "ffmpeg"]
        );
        assert_eq!(plan.terminal, "brew install ffmpeg");
    }

    #[test]
    fn shown_commands_quote_unusual_words() {
        let plan = Plan {
            manager: Manager::Brew,
            package: "ffmpeg".into(),
            repository: Repository::NotChecked,
            install: Ok(vec![
                "/Users/a b/brew".into(),
                "install".into(),
                "it's".into(),
            ]),
            terminal: String::new(),
        };
        let shown = plan.shown_command().unwrap();
        if cfg!(windows) {
            assert_eq!(shown, "\"/Users/a b/brew\" install it's");
        } else {
            assert_eq!(shown, r"'/Users/a b/brew' install 'it'\''s'");
        }
    }
}
