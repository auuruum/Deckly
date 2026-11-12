//! The real plugin host runs the built counter plugin: the handshake
//! completes, a key press reaches the plugin, and its face comes back as a
//! host command. Everything lives in a temp folder; the user's Deckly
//! folders are never touched.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use deckly_plugin_host::{AppInfo, Command, Config, Host, HostEvent, Instance, Status};
use deckly_sdk::protocol::MANIFEST_FILE;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);
const PLUGIN: &str = counter_plugin::PLUGIN_ID;
const ACTION: &str = "com.deckly.example.counter.count";

/// A plugins folder holding the counter plugin as Deckly installs it.
fn install(root: &Path) -> PathBuf {
    let dir = root.join("plugins").join(PLUGIN);
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    let program = PathBuf::from(env!("CARGO_BIN_EXE_counter-plugin"));
    let name = program.file_name().unwrap();
    std::fs::copy(&program, dir.join(name)).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/counter.svg"),
        dir.join("assets/counter.svg"),
    )
    .unwrap();
    // The manifest as the plugin itself describes it.
    let described = std::process::Command::new(&program)
        .arg("--describe")
        .output()
        .unwrap();
    assert!(described.status.success());
    std::fs::write(dir.join(MANIFEST_FILE), described.stdout).unwrap();
    dir
}

fn start(root: &Path) -> (Host, Receiver<HostEvent>) {
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
    (host, rx)
}

fn wait_for<T>(rx: &Receiver<HostEvent>, mut pick: impl FnMut(HostEvent) -> Option<T>) -> T {
    let end = Instant::now() + WAIT;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        let event = rx
            .recv_timeout(left)
            .expect("the host sent nothing in time");
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

fn key(context: &str, settings: serde_json::Value) -> Instance {
    Instance {
        action: ACTION.into(),
        context: context.into(),
        device: String::new(),
        settings,
        column: 0,
        row: 0,
        state: None,
        in_multi_action: false,
        desired_state: None,
    }
}

fn face_of(event: HostEvent, context: &str) -> Option<deckly_plugin_host::Face> {
    match event {
        HostEvent::Command {
            command: Command::SetFace { context: c, face },
            ..
        } if c == context => face,
        _ => None,
    }
}

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("deckly-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

#[test]
fn the_host_runs_the_counter_plugin_over_pipes() {
    let root = temp_root("counter-host");
    install(&root);
    let (host, rx) = start(&root);

    let started = wait_for(&rx, |e| match e {
        HostEvent::Started(id) => Some(id),
        _ => None,
    });
    assert_eq!(started, PLUGIN);
    let plugin = host.plugins().into_iter().find(|p| p.id == PLUGIN).unwrap();
    assert_eq!(plugin.status, Status::Running);
    assert!(plugin.manifest.unwrap().action(ACTION).is_some());
    assert_eq!(host.plugin_of(ACTION).as_deref(), Some(PLUGIN));

    let k = key("page.0", json!({"step": 3}));
    assert!(host.send(PLUGIN, k.event("willAppear")));
    let face = wait_for(&rx, |e| face_of(e, "page.0"));
    assert_eq!(face.value.as_deref(), Some("0"));

    let pressed_at = Instant::now();
    assert!(host.send(PLUGIN, k.event("keyDown")));
    assert!(host.send(PLUGIN, k.event("keyUp")));
    let settings = wait_for(&rx, |e| match e {
        HostEvent::Command {
            command: Command::SetSettings { context, settings },
            ..
        } if context == "page.0" => Some(settings),
        _ => None,
    });
    assert_eq!(
        (settings["step"].clone(), settings["count"].clone()),
        (json!(3), json!(3))
    );
    let face = wait_for(&rx, |e| face_of(e, "page.0"));
    let round_trip = pressed_at.elapsed();
    assert_eq!(face.value.as_deref(), Some("3"));
    assert_eq!(face.icon.as_deref(), Some("assets/counter.svg"));
    eprintln!("key press to face update: {round_trip:?}");

    host.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_plugin_that_prints_to_standard_output_is_restarted_with_a_plain_note() {
    let root = temp_root("counter-println");
    let dir = install(&root);
    // A plugin program that prints text instead of speaking the protocol.
    let manifest = std::fs::read(dir.join(MANIFEST_FILE)).unwrap();
    let mut m = deckly_sdk::protocol::Manifest::parse(&manifest).unwrap();
    let target = deckly_sdk::protocol::current_target();
    let program = if cfg!(windows) {
        std::fs::write(dir.join("chatty.cmd"), "@echo hello from a plugin\r\n").unwrap();
        "chatty.cmd"
    } else {
        std::fs::write(
            dir.join("chatty"),
            "#!/bin/sh\necho hello from a plugin\nsleep 5\n",
        )
        .unwrap();
        "chatty"
    };
    m.programs.insert(target, program.into());
    std::fs::write(dir.join(MANIFEST_FILE), serde_json::to_vec(&m).unwrap()).unwrap();

    let (host, rx) = start(&root);
    let log = root.join("logs").join(format!("{PLUGIN}.log"));
    let end = Instant::now() + WAIT;
    let text = loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("restarting") || Instant::now() > end {
            break text;
        }
        // Drain events so the channel does not grow.
        while rx.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(50));
    };
    host.shutdown();
    assert!(text.contains("standard output"), "{text}");
    assert!(text.contains("hello from a plugin"), "{text}");
    assert!(text.contains("restarting"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn closing_the_input_stops_the_plugin() {
    let root = temp_root("counter-exit");
    install(&root);
    let (host, rx) = start(&root);
    wait_for(&rx, |e| matches!(e, HostEvent::Started(_)).then_some(()));
    let began = Instant::now();
    host.shutdown();
    // The SDK exits on end of input, well before the host would kill it.
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn forms_reach_the_plugin_through_the_real_host() {
    let root = temp_root("counter-forms");
    install(&root);
    let (host, rx) = start(&root);
    // The app puts secrets back here; this filter marks the label instead.
    host.set_settings_filter(std::sync::Arc::new(|plugin, settings| {
        assert_eq!(plugin, PLUGIN);
        if let Some(o) = settings.as_object_mut() {
            o.insert("label".into(), json!("Filtered"));
        }
    }));
    wait_for(&rx, |e| matches!(e, HostEvent::Started(_)).then_some(()));
    let manifest = host
        .plugins()
        .into_iter()
        .find(|p| p.id == PLUGIN)
        .and_then(|p| p.manifest)
        .and_then(|m| m.deckly.clone())
        .unwrap();
    assert!(manifest.action(ACTION).unwrap().form.is_some());
    assert!(manifest.form.is_some());

    let k = key("page.1", json!({"count": 9, "start": "10"}));
    assert!(host.send(PLUGIN, k.event("willAppear")));
    let face = wait_for(&rx, |e| face_of(e, "page.1"));
    assert_eq!(face.label.as_deref(), Some("Filtered"));

    assert!(host.ask_choices(PLUGIN, 41, "start", Some(&k)));
    let options = wait_for(&rx, |e| match e {
        HostEvent::Command {
            command:
                Command::Choices {
                    request: 41,
                    options,
                    error: None,
                },
            ..
        } => Some(options),
        _ => None,
    });
    assert_eq!(options.len(), 3);
    assert!(host.ask_choices(PLUGIN, 42, "nothing", None));
    let error = wait_for(&rx, |e| match e {
        HostEvent::Command {
            command: Command::Choices {
                request: 42, error, ..
            },
            ..
        } => Some(error),
        _ => None,
    });
    assert!(error.is_some());

    assert!(host.press_button(PLUGIN, "reset", Some(&k)));
    let settings = wait_for(&rx, |e| match e {
        HostEvent::Command {
            command: Command::SetSettings { context, settings },
            ..
        } if context == "page.1" => Some(settings),
        _ => None,
    });
    assert_eq!(settings["count"], 10);
    // A plugin that is not running is not asked.
    assert!(!host.ask_choices("com.example.none", 43, "start", None));
    host.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
