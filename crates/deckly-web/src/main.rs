//! deckly-web: runs one plugin web page for Deckly in the system web view.
//!
//! Two kinds of pages use it: plugins whose code is an HTML page (run in a
//! hidden window) and property inspectors, the settings pages of plugin
//! actions (placed in the editor on Windows, standalone elsewhere). Deckly starts one process per
//! page and never loads a web engine into the editor itself.
//!
//! Usage (Deckly passes these):
//! `deckly-web plugin <page> <port> <uuid> <register-event> <info>`
//! `deckly-web inspector <page> <port> <uuid> <register-event> <info> <action-info> <title> [<owner>]`
//!
//! After the page has loaded, its `connectElgatoStreamDeckSocket` function is
//! called with those values, as the SDK documents; the page then connects to
//! Deckly's local WebSocket by itself. The helper exits when its window is
//! closed or when Deckly closes its end of the standard input pipe.
//!
//! With a parent window (Windows), an inspector is a borderless child window
//! of the editor, clipped to its client area and moving with it:
//! Deckly sends `place <x> <y> <width> <height>` (physical client pixels) or
//! `hide` on standard input whenever that area changes.
//!
//! Deckly is an independent app and is not made, approved or supported by
//! Elgato or Corsair.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::borrow::Cow;
use std::io::BufRead;
use std::path::{Component, Path, PathBuf};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Theme, Window, WindowBuilder, WindowId};
use wry::http::{Response, header};
use wry::{WebView, WebViewBuilder};

mod popups;

/// Matches the editor's inspector background, which many pages expect.
const BACKGROUND: (u8, u8, u8, u8) = (0x2D, 0x2D, 0x2D, 0xFF);

/// Pages are served from the plugin's folder under this origin instead of
/// `file://`, where web views block ES modules and `fetch` of local files.
/// (WebView2 maps custom schemes to `http://<scheme>.<host>`.)
#[cfg(windows)]
const ORIGIN: &str = "http://deckly.localhost/";
#[cfg(not(windows))]
const ORIGIN: &str = "deckly://localhost/";
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

fn main() {
    if let Err(e) = run() {
        eprintln!("deckly-web: {e}");
        std::process::exit(1);
    }
}

struct Page {
    inspector: bool,
    /// The editor window this inspector belongs to (a Windows HWND).
    owner: Option<isize>,
    /// The plugin folder, the only place files are served from.
    root: PathBuf,
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
    // Deckly starts the helper in the plugin's folder.
    let root = std::env::current_dir()
        .and_then(|d| d.canonicalize())
        .map_err(|e| e.to_string())?;
    let url = page_url(&root, &path)?;
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
    let owner = rest.get(2).and_then(|o| o.parse().ok());
    Ok(Page {
        inspector,
        owner,
        root,
        url,
        connect,
        title: rest.get(1).cloned().unwrap_or_else(|| "Deckly".into()),
    })
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let page = parse_args(&args)?;
    // DECKLY_PLUGIN_DEBUG=1 shows hidden plugin pages and allows dev tools.
    let debug = std::env::var_os("DECKLY_PLUGIN_DEBUG").is_some();
    let event_loop = EventLoopBuilder::<Command>::with_user_event().build();
    listen_to_parent(event_loop.create_proxy());
    // An inspector placed in Deckly's window waits for its first place.
    let placed = cfg!(windows) && page.inspector && page.owner.is_some();
    let builder = WindowBuilder::new()
        .with_title(&page.title)
        .with_inner_size(LogicalSize::new(480.0, 560.0))
        .with_theme(Some(Theme::Dark))
        .with_decorations(!placed)
        // No resize frame either, so the page fills exactly the place given.
        .with_resizable(!placed)
        .with_visible(if page.inspector { !placed } else { debug });
    #[cfg(windows)]
    let builder = match page.owner.filter(|_| placed) {
        Some(owner) => {
            use tao::platform::windows::WindowBuilderExtWindows;
            // Tao enables a shadow by default, reserving an 8 px non-client inset.
            builder
                .with_parent_window(owner)
                .with_undecorated_shadow(false)
                .with_skip_taskbar(true)
        }
        None => builder,
    };
    let window = builder.build(&event_loop).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let owner = {
        use tao::platform::windows::WindowExtWindows;
        page.owner.or(Some(window.hwnd()))
    };
    #[cfg(not(windows))]
    let owner = None;
    let popups = popups::Popups::new(
        (*event_loop).clone(),
        event_loop.create_proxy(),
        page.root,
        page.title,
        debug,
        owner,
    );
    let builder = popups
        .configure(WebViewBuilder::new())
        .with_url(page.url)
        .with_initialization_script(page.connect)
        .with_background_color(BACKGROUND)
        .with_devtools(debug);
    let webview = build_webview(builder, &window)?;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        // The web view lives as long as the loop.
        let _ = &webview;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } => {
                if window_id == window.id() {
                    *flow = ControlFlow::Exit;
                } else {
                    popups.close(window_id);
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Destroyed,
                window_id,
                ..
            } => popups.close(window_id),
            Event::UserEvent(Command::ClosePopup(id)) => popups.close(id),
            Event::UserEvent(Command::PopupTitle(id, title)) => popups.title(id, &title),
            Event::UserEvent(Command::Place { x, y, w, h }) if placed => {
                window.set_inner_size(PhysicalSize::new(w, h));
                // Child coordinates are relative to the editor's client area.
                window.set_outer_position(PhysicalPosition::new(x, y));
                window.set_visible(true);
            }
            Event::UserEvent(Command::Hide) if placed => window.set_visible(false),
            _ => {}
        }
    })
}

/// What Deckly tells the helper on its standard input.
#[derive(Clone, Debug, PartialEq)]
enum Command {
    Place { x: i32, y: i32, w: u32, h: u32 },
    Hide,
    ClosePopup(WindowId),
    PopupTitle(WindowId, String),
}

fn build_webview(builder: WebViewBuilder<'_>, window: &Window) -> Result<WebView, String> {
    #[cfg(not(target_os = "linux"))]
    {
        builder.build(window).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().ok_or("no GTK container")?;
        builder.build_gtk(vbox).map_err(|e| e.to_string())
    }
}

fn parse_command(line: &str) -> Option<Command> {
    let words: Vec<&str> = line
        .trim_start_matches('\u{feff}')
        .split_whitespace()
        .collect();
    match words.as_slice() {
        ["hide"] => Some(Command::Hide),
        ["place", x, y, w, h] => {
            let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
            let sane = (1..=16384).contains(&w) && (1..=16384).contains(&h);
            sane.then_some(Command::Place {
                x: x.parse().ok()?,
                y: y.parse().ok()?,
                w,
                h,
            })
        }
        _ => None,
    }
}

/// Reads Deckly's commands from standard input. Deckly keeps the write end
/// open; when Deckly exits or crashes the input ends, and so does this
/// helper.
fn listen_to_parent(proxy: EventLoopProxy<Command>) {
    let _ = std::thread::Builder::new()
        .name("deckly-web-parent".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if let Some(command) = parse_command(&line)
                    && proxy.send_event(command).is_err()
                {
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

/// The URL of a page inside the plugin folder.
fn page_url(root: &Path, page: &Path) -> Result<String, String> {
    let rel = page
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside the plugin folder", page.display()))?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| encode(&c.as_os_str().to_string_lossy()))
        .collect();
    Ok(format!("{ORIGIN}{}", parts.join("/")))
}

fn encode(part: &str) -> String {
    part.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |at: usize| {
            bytes
                .get(at)
                .and_then(|&c| (c as char).to_digit(16))
                .map(|d| d as u8)
        };
        match (b, hex(i + 1), hex(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                out.push(h * 16 + l);
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// The file a request path names, if it is inside the plugin folder.
/// Symbolic links are resolved first, so none can lead out of it.
fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = String::from_utf8(decode(url_path)).ok()?;
    let mut path = root.to_path_buf();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(p) => path.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    let path = path.canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Answers the web view's request for a file of the plugin.
fn serve(root: &Path, url_path: &str) -> Response<Cow<'static, [u8]>> {
    let file = resolve(root, url_path)
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= MAX_FILE_BYTES));
    let (status, kind, body) = match file.and_then(|p| Some((std::fs::read(&p).ok()?, p))) {
        Some((bytes, p)) => (200, mime(&p), bytes),
        None => (404, "text/plain", b"not found".to_vec()),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, kind)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_only_files_inside_the_plugin() {
        let dir = std::env::temp_dir().join(format!("deckly-web-{}", std::process::id()));
        let root = dir.join("p.sdPlugin");
        std::fs::create_dir_all(root.join("pi dir")).unwrap();
        std::fs::write(root.join("pi dir/a b.js"), "x").unwrap();
        std::fs::write(dir.join("secret.txt"), "s").unwrap();
        let root = root.canonicalize().unwrap();
        let page = root.join("pi dir").join("a b.js");
        assert_eq!(
            page_url(&root, &page).unwrap(),
            format!("{ORIGIN}pi%20dir/a%20b.js")
        );
        assert_eq!(resolve(&root, "/pi%20dir/a%20b.js"), Some(page.clone()));
        assert_eq!(resolve(&root, "/pi dir/./a b.js"), Some(page));
        assert_eq!(resolve(&root, "/../secret.txt"), None);
        assert_eq!(resolve(&root, "/%2E%2E/secret.txt"), None);
        assert_eq!(resolve(&root, "/%2e%2e%2fsecret.txt"), None);
        #[cfg(windows)]
        assert_eq!(resolve(&root, "/pi dir%5c..%5c..%5csecret.txt"), None);
        assert_eq!(resolve(&root, "/pi dir"), None);
        assert_eq!(resolve(&root, "/missing.js"), None);
        assert!(page_url(&root, &dir.join("secret.txt")).is_err());
        let ok = serve(&root, "/pi%20dir/a%20b.js");
        assert_eq!(ok.status(), 200);
        assert_eq!(
            ok.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(serve(&root, "/../secret.txt").status(), 404);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("secret.txt"), root.join("escape.txt")).unwrap();
            assert_eq!(serve(&root, "/escape.txt").status(), 404);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_place_commands() {
        assert_eq!(
            parse_command("place -1900 40 600 300"),
            Some(Command::Place {
                x: -1900,
                y: 40,
                w: 600,
                h: 300
            })
        );
        assert_eq!(parse_command("hide"), Some(Command::Hide));
        assert_eq!(parse_command("\u{feff}hide\r"), Some(Command::Hide));
        assert_eq!(parse_command("place 1 2 0 5"), None);
        assert_eq!(parse_command("place 1 2 3"), None);
        assert_eq!(parse_command("place a b 3 4"), None);
        assert_eq!(parse_command("explode"), None);
    }

    #[test]
    fn arguments_cannot_escape_the_script() {
        // Tests run in the crate folder, which stands in for the plugin.
        let page = std::env::current_dir().unwrap().join("Cargo.toml");
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
