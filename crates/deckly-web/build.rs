//! Windows builds with the GNU toolchain load the WebView2 loader from
//! `WebView2Loader.dll` next to the program (MSVC builds link it in). This
//! copies Microsoft's redistributable loader, shipped in the
//! `webview2-com-sys` package, next to `deckly-web.exe`.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let gnu = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "gnu");
    if !(windows && gnu) {
        return;
    }
    if let Err(e) = copy_loader() {
        println!("cargo:warning=WebView2Loader.dll was not copied: {e}");
    }
}

fn copy_loader() -> Result<(), String> {
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => "x64",
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        other => return Err(format!("unknown architecture {other:?}")),
    };
    let cargo = std::env::var("CARGO").map_err(|e| e.to_string())?;
    let manifest = std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?;
    let out = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(Path::new(&manifest).join("Cargo.toml"))
        .output()
        .map_err(|e| e.to_string())?;
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let package = meta["packages"]
        .as_array()
        .and_then(|list| {
            list.iter()
                .find(|p| p["name"] == "webview2-com-sys")
                .and_then(|p| p["manifest_path"].as_str())
        })
        .ok_or("webview2-com-sys not found")?;
    let dll = Path::new(package)
        .parent()
        .ok_or("bad manifest path")?
        .join(arch)
        .join("WebView2Loader.dll");
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    let profile = out_dir.ancestors().nth(3).ok_or("unexpected OUT_DIR")?;
    let target = profile.join("WebView2Loader.dll");
    // A running deckly-web keeps the copy locked; an equal one can stay.
    let source = std::fs::read(&dll).map_err(|e| format!("{}: {e}", dll.display()))?;
    if std::fs::read(&target).is_ok_and(|bytes| bytes == source) {
        return Ok(());
    }
    std::fs::copy(&dll, &target).map_err(|e| format!("{}: {e}", dll.display()))?;
    Ok(())
}
