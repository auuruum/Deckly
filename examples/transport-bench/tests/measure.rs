//! Measurements for docs/plugin-sdk.md: the real plugin host runs one of
//! the two benchmark plugins at a time from a temp folder.
//!
//! - Latency: from `Host::send` of a key-down to the plugin's handler (the
//!   wall-clock time the handler puts in its title), and the round trip
//!   until the title command reaches the host's event channel.
//! - CPU: 32 keys whose plugin sets a new title once a second, for 60 s;
//!   CPU time of this process (host, standing in for the app) and of the
//!   plugin process, read from /proc.
//!
//! Ignored by default; run on an otherwise idle machine with
//! `cargo test -p transport-bench --release -- --ignored --nocapture --test-threads 1`.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use deckly_plugin_host::{AppInfo, Command, Config, Host, HostEvent, Instance};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};
use transport_bench::{PIPE_PLUGIN, WS_PLUGIN, now_micros};

const PRESSES: usize = 2000;
const KEYS: usize = 32;
const CPU_SECONDS: u64 = 60;

#[derive(Clone, Copy)]
enum Kind {
    Pipe,
    Ws,
}

impl Kind {
    fn id(self) -> &'static str {
        match self {
            Kind::Pipe => PIPE_PLUGIN,
            Kind::Ws => WS_PLUGIN,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Pipe => "pipe (Deckly SDK)",
            Kind::Ws => "WebSocket (Stream Deck protocol)",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Kind::Pipe => "bench-pipe",
            Kind::Ws => "bench-ws",
        }
    }
}

fn bin(name: &str) -> PathBuf {
    // The binaries sit next to this test's directory in the target folder.
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let path = dir.join(name);
    assert!(path.is_file(), "build it first: {}", path.display());
    path
}

fn install(root: &Path, kind: Kind) {
    let program = bin(kind.program());
    match kind {
        Kind::Pipe => {
            let dir = root.join("plugins").join(PIPE_PLUGIN);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(&program, dir.join(kind.program())).unwrap();
            let out = std::process::Command::new(&program)
                .arg("--describe")
                .output()
                .unwrap();
            std::fs::write(dir.join("deckly-plugin.json"), out.stdout).unwrap();
        }
        Kind::Ws => {
            let dir = root.join("plugins").join(format!("{WS_PLUGIN}.sdPlugin"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::copy(&program, dir.join(kind.program())).unwrap();
            let manifest = serde_json::json!({
                "UUID": WS_PLUGIN, "Name": "Bench ws", "Version": "1",
                "CodePath": kind.program(), "SDKVersion": 2,
                "Actions": [
                    {"UUID": format!("{WS_PLUGIN}.probe"), "Name": "Probe", "States": [{}]},
                    {"UUID": format!("{WS_PLUGIN}.tick"), "Name": "Tick", "States": [{}]}
                ]
            });
            std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
        }
    }
}

fn start(kind: Kind) -> Result<(Host, Receiver<HostEvent>, PathBuf), String> {
    let root = std::env::temp_dir().join(format!(
        "deckly-bench-{}-{}",
        kind.program(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    install(&root, kind);
    let (events, rx) = channel();
    let host = Host::start(
        Config {
            plugins_dir: root.join("plugins"),
            logs_dir: root.join("logs"),
            web_helper: None,
            node: None,
            wine: None,
            runtimes_dir: root.join("runtimes"),
            guard: None,
            app: AppInfo {
                language: "en".into(),
                platform_version: String::new(),
                font: String::new(),
            },
        },
        events,
    )
    .unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(HostEvent::Started(id)) if id == kind.id() => break,
            Ok(_) => {}
            Err(_) => return Err(format!("{} did not start", kind.name())),
        }
    }
    Ok((host, rx, root))
}

fn key(kind: Kind, action: &str, context: &str) -> Instance {
    Instance {
        action: format!("{}.{action}", kind.id()),
        context: context.into(),
        device: "bench".into(),
        settings: serde_json::json!({}),
        column: 0,
        row: 0,
        state: None,
        in_multi_action: false,
        desired_state: None,
    }
}

fn title_of(rx: &Receiver<HostEvent>, context: &str) -> Result<String, String> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(HostEvent::Command {
                command:
                    Command::SetTitle {
                        context: c,
                        title: Some(t),
                        ..
                    },
                ..
            }) if c == context => return Ok(t),
            Ok(_) => {}
            Err(_) => return Err(format!("no title for {context}")),
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn summary(name: &str, mut v: Vec<f64>) {
    v.sort_by(f64::total_cmp);
    println!(
        "  {name}: median {:.1} us, p95 {:.1} us, p99 {:.1} us, max {:.1} us",
        percentile(&v, 0.5),
        percentile(&v, 0.95),
        percentile(&v, 0.99),
        v[v.len() - 1]
    );
}

fn latency(kind: Kind) -> Result<(), String> {
    let (host, rx, root) = start(kind)?;
    let probe = key(kind, "probe", "probe.0");
    host.send(kind.id(), probe.event("willAppear"));
    let mut one_way = Vec::new();
    let mut round_trip = Vec::new();
    for i in 0..PRESSES + 100 {
        let sent_at = now_micros();
        let began = Instant::now();
        host.send(kind.id(), probe.event("keyDown"));
        let handled: u128 = title_of(&rx, "probe.0")?
            .parse()
            .map_err(|e| format!("{e}"))?;
        let rt = began.elapsed();
        host.send(kind.id(), probe.event("keyUp"));
        // The first presses warm up caches and threads.
        if i >= 100 {
            one_way.push(handled.saturating_sub(sent_at) as f64);
            round_trip.push(rt.as_secs_f64() * 1e6);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    println!("{}: {PRESSES} key presses", kind.name());
    summary("send to handler", one_way);
    summary("round trip to title command", round_trip);
    host.shutdown();
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

/// User plus system CPU time of a process, in clock ticks.
fn cpu_ticks(pid: u32) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    // Fields after the command name, which is in parentheses.
    let rest = &stat[stat.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap()
}

/// The plugin process: a child of this process running the program.
fn plugin_pid(program: &str) -> Result<u32, String> {
    let me = std::process::id().to_string();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let comm = &stat[stat.find('(').unwrap() + 1..close];
        let ppid = stat[close + 2..].split_whitespace().nth(1).unwrap_or("");
        if ppid == me && comm.starts_with(&program[..program.len().min(15)]) {
            return Ok(pid);
        }
    }
    Err(format!("no {program} process"))
}

fn cpu(kind: Kind) -> Result<(), String> {
    let (host, rx, root) = start(kind)?;
    let pid = plugin_pid(kind.program())?;
    // Drain events as the app's plugin thread would.
    let drain = std::thread::spawn(move || {
        let mut titles = 0u64;
        while let Ok(e) = rx.recv() {
            if matches!(
                e,
                HostEvent::Command {
                    command: Command::SetTitle { .. },
                    ..
                }
            ) {
                titles += 1;
            }
        }
        titles
    });
    for i in 0..KEYS {
        let k = key(kind, "tick", &format!("tick.{i}"));
        host.send(kind.id(), k.event("willAppear"));
    }
    std::thread::sleep(Duration::from_secs(3));
    let me = std::process::id();
    let (host0, plugin0) = (cpu_ticks(me), cpu_ticks(pid));
    let began = Instant::now();
    std::thread::sleep(Duration::from_secs(CPU_SECONDS));
    let (host1, plugin1) = (cpu_ticks(me), cpu_ticks(pid));
    let wall = began.elapsed().as_secs_f64();
    host.shutdown();
    drop(host);
    let titles = drain.join().unwrap_or(0);
    // Linux reports CPU time in 1/100 s ticks on every common setup.
    let ms = |t: u64| t as f64 * 10.0;
    println!(
        "{}: {KEYS} keys, one title each per second, {wall:.0} s",
        kind.name()
    );
    println!(
        "  host process CPU {:.0} ms ({:.3} %), plugin CPU {:.0} ms ({:.3} %), titles received {titles}",
        ms(host1 - host0),
        ms(host1 - host0) / (wall * 10.0),
        ms(plugin1 - plugin0),
        ms(plugin1 - plugin0) / (wall * 10.0),
    );
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
#[ignore = "a measurement, run by hand"]
fn latency_pipe() {
    latency(Kind::Pipe).unwrap();
}

#[test]
#[ignore = "a measurement, run by hand"]
fn latency_ws() {
    latency(Kind::Ws).unwrap();
}

#[test]
#[ignore = "a measurement, run by hand"]
fn cpu_pipe() {
    cpu(Kind::Pipe).unwrap();
}

#[test]
#[ignore = "a measurement, run by hand"]
fn cpu_ws() {
    cpu(Kind::Ws).unwrap();
}
