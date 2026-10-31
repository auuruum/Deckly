//! A Windows plugin on Linux or macOS starts through Wine. The Wine here is a
//! script that records how it was started.
#![cfg(unix)]

use deckly_plugin_host::{AppInfo, Config, Host};
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

const MANIFEST: &str = r#"{
    "UUID": "com.example.windows", "Name": "Windows Only", "Version": "1.0",
    "CodePath": "Tool", "OS": [{"Platform": "windows"}],
    "Actions": [{"UUID": "com.example.windows.go", "States": []}]
}"#;

fn wait_for_lines(path: &Path) -> Option<Vec<String>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.ends_with("done\n")
        {
            return Some(text.lines().map(str::to_owned).collect());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn a_windows_program_starts_through_wine_and_is_told_it_runs_on_windows() {
    let root = std::env::temp_dir().join(format!("deckly-wine-run-{}", std::process::id()));
    let plugin = root.join("plugins/com.example.windows.sdPlugin");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    std::fs::write(plugin.join("manifest.json"), MANIFEST).unwrap();
    // The manifest leaves ".exe" out and spells the name in another case.
    std::fs::write(plugin.join("tool.exe"), b"MZ\x90\x00").unwrap();

    let record = root.join("record");
    let wine = root.join("fake-wine");
    std::fs::write(
        &wine,
        format!(
            "#!/bin/sh\n{{ pwd; echo \"$WINEPREFIX\"; for a in \"$@\"; do echo \"$a\"; done; \
             echo done; }} > {}\nexec sleep 30\n",
            record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wine, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (events, _rx) = channel();
    let host = Host::start(
        Config {
            plugins_dir: root.join("plugins"),
            logs_dir: root.join("logs"),
            web_helper: None,
            node: None,
            wine: Some(wine),
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

    let lines = wait_for_lines(&record);
    host.shutdown();
    let lines = lines.expect("the plugin was not started");

    let plugin = plugin.canonicalize().unwrap();
    assert_eq!(Path::new(&lines[0]).canonicalize().unwrap(), plugin);
    assert_eq!(lines[1], root.join("runtimes/wine").to_str().unwrap());
    assert_eq!(Path::new(&lines[2]).file_name().unwrap(), "tool.exe");
    assert_eq!(lines[3], "-port");
    assert_eq!(lines[9], "-info");
    let info: Value = serde_json::from_str(&lines[10]).unwrap();
    assert_eq!(info["application"]["platform"], "windows");
    let _ = std::fs::remove_dir_all(root);
}
