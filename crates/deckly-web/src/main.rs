//! deckly-web: runs one plugin web page for Deckly in the system web view.
//!
//! Two kinds of pages use it: plugins whose code is an HTML page (run in a
//! hidden window) and property inspectors, the settings pages of plugin
//! actions (run in a window of their own). Deckly starts one process per
//! page and never loads a web engine into the editor itself.
//!
//! Usage (Deckly passes these):
//! `deckly-web plugin <page> <port> <uuid> <register-event> <info>`
//! `deckly-web inspector <page> <port> <uuid> <register-event> <info> <action-info> <title>`
//!
//! After the page has loaded, its `connectElgatoStreamDeckSocket` function is
//! called with those values, as the SDK documents; the page then connects to
//! Deckly's local WebSocket by itself. The helper exits when its window is
//! closed or when Deckly closes its end of the standard input pipe.
//!
//! Deckly is an independent app and is not made, approved or supported by
//! Elgato or Corsair.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::io::Read;
use std::path::Path;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::{Theme, WindowBuilder};
use wry::{NewWindowResponse, WebViewBuilder};

/// Matches the editor's inspector background, which many pages expect.
const BACKGROUND: (u8, u8, u8, u8) = (0x2D, 0x2D, 0x2D, 0xFF);

fn main() {
    if let Err(e) = run() {
        eprintln!("deckly-web: {e}");
        std::process::exit(1);
    }
}

struct Page {
    inspector: bool,
    url: String,
    connect: String,
    title: String,
}

fn parse_args(args: &[String]) -> Result<Page, String> {
    let [mode, page, port, uuid, event, info, rest @ ..] = args else {
        return Err(
            "usage: deckly-web plugin|inspector <page> <port> <uuid> <event> <info> \
                    [<action-info> <title>]"
                .into(),
        );
    };
    let inspector = match mode.as_str() {
        "plugin" => false,
        "inspector" => true,
        other => return Err(format!("unknown mode {other}")),
    };
    let port: u16 = port.parse().map_err(|_| format!("bad port {port}"))?;
    let path = Path::new(page)
        .canonicalize()
        .map_err(|e| format!("{page}: {e}"))?;
    // Arguments go into the page's script as JSON strings, so nothing in them
    // can break out of the call.
    let quote = |s: &str| serde_json::Value::from(s).to_string();
    let mut call_args = vec![
        quote(&port.to_string()),
        quote(uuid),
        quote(event),
        quote(info),
    ];
    if inspector {
        call_args.push(quote(rest.first().map_or("{}", String::as_str)));
    }
    let connect = format!(
        "window.addEventListener('load', function () {{\n\
         var connect = window.connectElgatoStreamDeckSocket || window.connectSocket;\n\
         if (typeof connect === 'function') {{ connect({}); }}\n\
         }});",
        call_args.join(", ")
    );
    Ok(Page {
        inspector,
        url: file_url(&path),
        connect,
        title: rest.get(1).cloned().unwrap_or_else(|| "Deckly".into()),
    })
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let page = parse_args(&args)?;
    exit_with_parent();
    // DECKLY_PLUGIN_DEBUG=1 shows hidden plugin pages and allows dev tools.
    let debug = std::env::var_os("DECKLY_PLUGIN_DEBUG").is_some();
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title(&page.title)
        .with_inner_size(LogicalSize::new(480.0, 560.0))
        .with_theme(Some(Theme::Dark))
        .with_visible(page.inspector || debug)
        .build(&event_loop)
        .map_err(|e| e.to_string())?;
    let builder = WebViewBuilder::new()
        .with_url(page.url)
        .with_initialization_script(page.connect)
        .with_background_color(BACKGROUND)
        .with_devtools(debug)
        // shortcut: pages cannot open windows of their own (some sign-in
        // flows use them); web links open in the browser instead. Add real
        // popup windows when a plugin needs them.
        .with_new_window_req_handler(|url, _| {
            open_in_browser(&url);
            NewWindowResponse::Deny
        });
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().ok_or("no GTK container")?;
        builder.build_gtk(vbox).map_err(|e| e.to_string())?
    };
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        // The web view lives as long as the loop.
        let _ = &webview;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *flow = ControlFlow::Exit;
        }
    })
}

/// Deckly keeps the write end of our standard input open; when Deckly exits
/// (or crashes) the read returns and so does this helper.
fn exit_with_parent() {
    let _ = std::thread::Builder::new()
        .name("deckly-web-parent".into())
        .spawn(|| {
            let mut buf = [0u8; 64];
            let mut stdin = std::io::stdin();
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
            std::process::exit(0);
        });
}

/// Opens web links in the default browser; anything else is ignored.
fn open_in_browser(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    // The URL is passed as one argument, never through a shell.
    let opener = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

/// A `file://` URL for an absolute path, percent-encoding what URLs do not
/// allow.
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    // Windows canonical paths start with the verbatim prefix.
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_encoded() {
        if cfg!(windows) {
            assert_eq!(
                file_url(Path::new(r"\\?\C:\Users\A B\pi.html")),
                "file:///C:/Users/A%20B/pi.html"
            );
        } else {
            assert_eq!(
                file_url(Path::new("/home/a b/pi#1.html")),
                "file:///home/a%20b/pi%231.html"
            );
        }
    }

    #[test]
    fn arguments_cannot_escape_the_script() {
        let page = std::env::current_exe().unwrap();
        let args: Vec<String> = [
            "inspector",
            page.to_str().unwrap(),
            "1234",
            "tok');alert(1);//",
            "registerPropertyInspector",
            "{\"a\":1}",
            "{}",
            "Title",
        ]
        .map(String::from)
        .to_vec();
        let p = parse_args(&args).unwrap();
        assert!(p.inspector);
        assert!(p.connect.contains(r#""tok');alert(1);//""#));
        assert!(p.connect.contains(r#""{\"a\":1}""#));
        assert_eq!(p.title, "Title");
        assert!(parse_args(&args[..3]).is_err());
        let mut bad = args.clone();
        bad[0] = "other".into();
        assert!(parse_args(&bad).is_err());
    }
}
