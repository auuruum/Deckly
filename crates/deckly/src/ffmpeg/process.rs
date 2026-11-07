//! Child processes for FFmpeg setup: short queries with a time and output
//! limit, and long installs whose output streams into a bounded log. Neither
//! ever blocks the UI thread; both are started from worker threads.

use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Lines a log keeps; older ones are dropped.
pub const MAX_LOG_LINES: usize = 400;
/// Characters a log line keeps; the rest of a longer line is dropped.
pub const MAX_LINE_CHARS: usize = 300;
const POLL: Duration = Duration::from_millis(50);

/// Hides the console window a Windows console program would open.
pub fn no_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// What a finished query printed, each stream cut to its limit.
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Reads `pipe` to its end on a thread and keeps its first `max_bytes`.
/// Reading goes on past the limit, so the program never blocks on a full
/// pipe.
fn collect(mut pipe: impl Read + Send + 'static, max_bytes: usize) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut chunk) {
            let Some(read) = chunk.get(..n).filter(|r| !r.is_empty()) else {
                break;
            };
            let room = max_bytes.saturating_sub(kept.len());
            kept.extend_from_slice(read.get(..room.min(n)).unwrap_or_default());
        }
        let _ = tx.send(kept);
    });
    rx
}

/// Runs `command` and returns its exit status and at most `max_bytes` of
/// each output stream. Gives up and kills it after `timeout`.
pub fn output(mut command: Command, timeout: Duration, max_bytes: usize) -> Result<Output, String> {
    let name = program_name(&command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot run {name}: {e}"))?;
    let stdout = child.stdout.take().map(|p| collect(p, max_bytes));
    let stderr = child.stderr.take().map(|p| collect(p, max_bytes));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                stop(&mut child);
                return Err(format!(
                    "{name} did not finish within {} seconds",
                    timeout.as_secs()
                ));
            }
            Err(e) => {
                stop(&mut child);
                return Err(format!("Cannot wait for {name}: {e}"));
            }
        }
    };
    // A program that left a child behind may keep a pipe open; do not wait
    // for that one.
    let take = |rx: Option<mpsc::Receiver<Vec<u8>>>| {
        rx.and_then(|rx| rx.recv_timeout(Duration::from_secs(2)).ok())
            .unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: take(stdout),
        stderr: take(stderr),
    })
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub fn program_name(command: &Command) -> String {
    let program = std::path::Path::new(command.get_program());
    program
        .file_name()
        .unwrap_or(program.as_os_str())
        .to_string_lossy()
        .chars()
        .take(64)
        .collect()
}

/// The last lines a program printed, each cut to a bounded length.
#[derive(Default)]
pub struct Log {
    lines: VecDeque<String>,
    partial: String,
}

impl Log {
    /// Adds printed bytes. Carriage returns end a line too, so progress bars
    /// that redraw one line do not grow it.
    pub fn push(&mut self, bytes: &[u8]) {
        for c in String::from_utf8_lossy(bytes).chars() {
            match c {
                '\n' | '\r' => self.end_line(),
                c if c.is_control() && c != '\t' => {}
                c if self.partial.chars().count() < MAX_LINE_CHARS => self.partial.push(c),
                _ => {}
            }
        }
    }

    /// Adds a line of Deckly's own.
    pub fn note(&mut self, line: &str) {
        self.end_line();
        self.partial = line.chars().take(MAX_LINE_CHARS).collect();
        self.end_line();
    }

    fn end_line(&mut self) {
        if self.partial.trim().is_empty() {
            self.partial.clear();
            return;
        }
        if self.lines.len() >= MAX_LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(std::mem::take(&mut self.partial));
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines
            .iter()
            .map(String::as_str)
            .chain((!self.partial.is_empty()).then_some(self.partial.as_str()))
    }

    #[cfg(test)]
    pub fn last(&self) -> Option<&str> {
        self.lines().last()
    }
}

#[derive(Default)]
struct State {
    log: Log,
    /// Bytes done and expected, for a download.
    progress: Option<(u64, u64)>,
    outcome: Option<Result<String, String>>,
}

/// Work on a background thread whose log and outcome the window shows.
#[derive(Clone)]
pub struct Job {
    state: Arc<Mutex<State>>,
    cancel: Arc<AtomicBool>,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

/// What the work sees of its job.
pub struct Reporter<'a>(&'a Job);

impl Reporter<'_> {
    pub fn cancelled(&self) -> bool {
        self.0.cancel.load(Ordering::Relaxed)
    }

    pub fn note(&self, line: &str) {
        self.0.lock().log.note(line);
        (self.0.repaint)();
    }

    pub fn output(&self, bytes: &[u8]) {
        self.0.lock().log.push(bytes);
        (self.0.repaint)();
    }

    pub fn progress(&self, done: u64, total: u64) {
        self.0.lock().progress = Some((done, total));
        (self.0.repaint)();
    }
}

impl Job {
    /// Runs `work` on its own thread. Its `Ok` text says what was done.
    pub fn start(
        repaint: Arc<dyn Fn() + Send + Sync>,
        work: impl FnOnce(&Reporter<'_>) -> Result<String, String> + Send + 'static,
    ) -> Job {
        let job = Job {
            state: Arc::default(),
            cancel: Arc::default(),
            repaint,
        };
        let worker = job.clone();
        let spawned = std::thread::Builder::new()
            .name("deckly-ffmpeg-setup".into())
            .spawn(move || {
                let outcome = work(&Reporter(&worker));
                worker.lock().outcome = Some(outcome);
                (worker.repaint)();
            });
        if let Err(e) = spawned {
            job.lock().outcome = Some(Err(format!("Cannot start: {e}")));
        }
        job
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn outcome(&self) -> Option<Result<String, String>> {
        self.lock().outcome.clone()
    }

    pub fn progress(&self) -> Option<(u64, u64)> {
        self.lock().progress
    }

    /// Calls `f` with the log's lines.
    pub fn with_log<R>(&self, f: impl FnOnce(&Log) -> R) -> R {
        f(&self.lock().log)
    }
}

/// Runs `command`, streams what it prints into the job's log and waits for
/// it. Cancelling kills it; a program that now runs as administrator cannot
/// be killed by Deckly, so the job then stops waiting and says so.
pub fn stream(mut command: Command, reporter: &Reporter<'_>) -> Result<(), String> {
    let name = program_name(&command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot run {name}: {e}"))?;
    let job = reporter.0.clone();
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ];
    for mut pipe in pipes.into_iter().flatten() {
        let job = job.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut chunk) {
                match chunk.get(..n) {
                    Some(read) if !read.is_empty() => Reporter(&job).output(read),
                    _ => break,
                }
            }
        });
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(exit_text(&name, status)),
            Ok(None) if reporter.cancelled() => {
                return match child.kill() {
                    Ok(()) => {
                        let _ = child.wait();
                        Err("Cancelled.".into())
                    }
                    Err(_) => Err(format!(
                        "{name} now runs as administrator and cannot be stopped from \
                         Deckly. It finishes on its own; check again afterwards."
                    )),
                };
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => return Err(format!("Cannot wait for {name}: {e}")),
        }
    }
}

fn exit_text(name: &str, status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("{name} stopped with exit code {code}."),
        None => format!("{name} was stopped."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_bounded_lines() {
        let mut log = Log::default();
        log.push(b"one\ntwo\r\nthr");
        log.push(b"ee\n\n\n");
        assert_eq!(log.lines().collect::<Vec<_>>(), ["one", "two", "three"]);
        log.push("x".repeat(MAX_LINE_CHARS * 3).as_bytes());
        log.push(b"\n");
        assert_eq!(log.last().map(str::len), Some(MAX_LINE_CHARS));
        for i in 0..MAX_LOG_LINES * 2 {
            log.push(format!("line {i}\n").as_bytes());
        }
        assert_eq!(log.lines().count(), MAX_LOG_LINES);
        assert_eq!(
            log.last(),
            Some(format!("line {}", MAX_LOG_LINES * 2 - 1).as_str())
        );
        // Invalid UTF-8 and control characters do not break it.
        log.push(&[0xff, 0x1b, b'[', b'1', b'm', b'o', b'k', b'\n']);
        assert_eq!(log.last(), Some("\u{fffd}[1mok"));
        log.note("Deckly says hi");
        assert_eq!(log.last(), Some("Deckly says hi"));
    }

    #[cfg(unix)]
    #[test]
    fn queries_are_bounded_in_size_and_time() {
        let mut yes = Command::new("sh");
        yes.args(["-c", "head -c 100000 /dev/zero"]);
        let out = output(yes, Duration::from_secs(10), 1000).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 1000);
        let mut both = Command::new("sh");
        both.args([
            "-c",
            "echo out; echo err >&2; head -c 100000 /dev/zero >&2; exit 2",
        ]);
        let out = output(both, Duration::from_secs(10), 100).unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr.len(), 100);
        assert!(out.stderr.starts_with(b"err\n"));
        let mut slow = Command::new("sleep");
        slow.arg("30");
        let started = Instant::now();
        let error = output(slow, Duration::from_millis(200), 10).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        let missing = Command::new("/nonexistent/deckly-test-program");
        assert!(output(missing, Duration::from_secs(1), 10).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_streamed_install_reports_output_failure_and_cancel() {
        let wait = |job: &Job| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(outcome) = job.outcome() {
                    return outcome;
                }
                assert!(Instant::now() < deadline, "the job did not finish");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let job = Job::start(Arc::new(|| {}), |r| {
            let mut c = Command::new("sh");
            c.args(["-c", "echo installing; echo oops >&2; exit 3"]);
            stream(c, r).map(|()| String::new())
        });
        let error = wait(&job).unwrap_err();
        assert!(error.contains("exit code 3"), "{error}");
        // The readers may still be adding the last lines.
        std::thread::sleep(Duration::from_millis(100));
        let lines = job.with_log(|log| log.lines().map(str::to_owned).collect::<Vec<_>>());
        assert!(lines.contains(&"installing".to_owned()), "{lines:?}");
        assert!(lines.contains(&"oops".to_owned()), "{lines:?}");

        let job = Job::start(Arc::new(|| {}), |r| {
            let mut c = Command::new("sleep");
            c.arg("30");
            stream(c, r).map(|()| String::new())
        });
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        job.cancel();
        assert_eq!(wait(&job), Err("Cancelled.".to_owned()));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
