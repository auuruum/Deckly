//! Related plugin windows. The web engine preserves `window.opener` itself.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Theme, Window, WindowBuilder, WindowId};
use wry::{NewWindowFeatures, NewWindowResponse, WebView, WebViewBuilder};

use super::{BACKGROUND, Command, ORIGIN, build_webview, open_in_browser, resolve, serve};

const MAX_POPUPS: usize = 8;

struct Popup {
    // Drop the webview before the window that contains it.
    _webview: WebView,
    window: Window,
}

pub struct Popups {
    target: EventLoopWindowTarget<Command>,
    proxy: EventLoopProxy<Command>,
    root: PathBuf,
    title: String,
    debug: bool,
    owner: Option<isize>,
    windows: RefCell<HashMap<WindowId, Popup>>,
}

impl Popups {
    pub fn new(
        target: EventLoopWindowTarget<Command>,
        proxy: EventLoopProxy<Command>,
        root: PathBuf,
        title: String,
        debug: bool,
        owner: Option<isize>,
    ) -> Rc<Self> {
        Rc::new(Self {
            target,
            proxy,
            root,
            title,
            debug,
            owner,
            windows: RefCell::new(HashMap::new()),
        })
    }

    pub fn configure<'a>(self: &Rc<Self>, builder: WebViewBuilder<'a>) -> WebViewBuilder<'a> {
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        builder
            .with_custom_protocol("deckly".into(), move |_, request| {
                serve(&root, request.uri().path())
            })
            .with_new_window_req_handler(move |url, features| {
                let Some(popups) = weak.upgrade() else {
                    return NewWindowResponse::Deny;
                };
                match target(&popups.root, &url) {
                    Target::Plugin => match popups.open(features) {
                        Ok(response) => response,
                        Err(error) => {
                            eprintln!("deckly-web: cannot open plugin window: {error}");
                            NewWindowResponse::Deny
                        }
                    },
                    Target::Browser => {
                        open_in_browser(&url);
                        NewWindowResponse::Deny
                    }
                    Target::Deny => NewWindowResponse::Deny,
                }
            })
    }

    fn open(self: &Rc<Self>, features: NewWindowFeatures) -> Result<NewWindowResponse, String> {
        if self
            .windows
            .try_borrow()
            .map_or(true, |w| w.len() >= MAX_POPUPS)
        {
            return Err("too many plugin windows are open".into());
        }
        let size = popup_size(features.size.map(|s| (s.width, s.height)));
        let builder = WindowBuilder::new()
            .with_title(format!("{} - Deckly", self.title))
            .with_theme(Some(Theme::Dark))
            .with_inner_size(size);
        #[cfg(test)]
        let builder = builder.with_visible(false).with_focused(false);
        #[cfg(windows)]
        let builder = {
            use tao::platform::windows::WindowBuilderExtWindows;
            match self.owner {
                Some(owner) => builder.with_owner_window(owner),
                None => builder,
            }
        };
        #[cfg(not(windows))]
        let _ = self.owner;
        let window = builder.build(&self.target).map_err(|e| e.to_string())?;
        let id = window.id();
        let proxy = self.proxy.clone();
        let builder = self
            .configure(WebViewBuilder::new())
            .with_background_color(BACKGROUND)
            .with_devtools(self.debug)
            .with_document_title_changed_handler(move |title| {
                let title: String = title.chars().take(200).collect();
                let _ = proxy.send_event(Command::PopupTitle(id, title));
            });
        // The browser connects the new window to its opener and navigates it.
        // Do not set a URL or run the inspector's registration script here.
        #[cfg(windows)]
        let builder = {
            use wry::WebViewBuilderExtWindows;
            builder.with_environment(features.opener.environment)
        };
        #[cfg(target_os = "linux")]
        let builder = {
            use wry::WebViewBuilderExtUnix;
            builder.with_related_view(features.opener.webview)
        };
        #[cfg(target_os = "macos")]
        let builder = {
            use wry::WebViewBuilderExtMacos;
            builder.with_webview_configuration(features.opener.target_configuration)
        };
        let webview = build_webview(builder, &window)?;
        #[cfg(windows)]
        let response = {
            use wry::WebViewExtWindows;
            windows::close_shell_with_page(&webview, self.proxy.clone(), id)?;
            NewWindowResponse::Create {
                webview: webview.webview(),
            }
        };
        #[cfg(target_os = "linux")]
        let response = {
            use wry::WebViewExtUnix;
            NewWindowResponse::Create {
                webview: webview.webview(),
            }
        };
        #[cfg(target_os = "macos")]
        let response = {
            use wry::WebViewExtMacOS;
            NewWindowResponse::Create {
                webview: webview.webview().into_super(),
            }
        };
        self.windows
            .try_borrow_mut()
            .map_err(|_| "plugin window is busy")?
            .insert(
                id,
                Popup {
                    _webview: webview,
                    window,
                },
            );
        Ok(response)
    }

    pub fn close(&self, id: WindowId) {
        // Release the map borrow before dropping a window and its callbacks.
        let removed = self
            .windows
            .try_borrow_mut()
            .ok()
            .and_then(|mut w| w.remove(&id));
        drop(removed);
    }

    pub fn title(&self, id: WindowId, title: &str) {
        if let Ok(windows) = self.windows.try_borrow()
            && let Some(popup) = windows.get(&id)
        {
            popup.window.set_title(&format!("{title} - Deckly"));
        }
    }
}

#[derive(Debug, PartialEq)]
enum Target {
    Plugin,
    Browser,
    Deny,
}

fn target(root: &std::path::Path, url: &str) -> Target {
    if url == "about:blank" {
        return Target::Plugin;
    }
    let plain = url.split('#').next().unwrap_or(url);
    let Ok(uri) = plain.parse::<wry::http::Uri>() else {
        return Target::Deny;
    };
    let Ok(origin) = ORIGIN.parse::<wry::http::Uri>() else {
        return Target::Deny;
    };
    let our_host = uri
        .authority()
        .zip(origin.authority())
        .is_some_and(|(a, b)| a.as_str().eq_ignore_ascii_case(b.as_str()));
    if our_host {
        if uri.scheme_str() == origin.scheme_str() && resolve(root, uri.path()).is_some() {
            Target::Plugin
        } else {
            Target::Deny
        }
    } else if matches!(uri.scheme_str(), Some("http" | "https")) && uri.authority().is_some() {
        Target::Browser
    } else {
        Target::Deny
    }
}

fn popup_size(requested: Option<(f64, f64)>) -> LogicalSize<f64> {
    let (w, h) = requested
        .filter(|(w, h)| w.is_finite() && h.is_finite())
        .unwrap_or((560.0, 760.0));
    LogicalSize::new(w.clamp(200.0, 2048.0), h.clamp(150.0, 2048.0))
}

/// WebView2 closes its container on window.close(); also close the Tao shell.
#[cfg(windows)]
mod windows {
    use super::*;
    use webview2_com::WindowCloseRequestedEventHandler;
    use wry::WebViewExtWindows;

    pub fn close_shell_with_page(
        webview: &WebView,
        proxy: EventLoopProxy<Command>,
        id: WindowId,
    ) -> Result<(), String> {
        let handler = WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
            let _ = proxy.send_event(Command::ClosePopup(id));
            Ok(())
        }));
        let mut token = 0;
        // Safety: this live WebView2 belongs to the current event-loop thread.
        // WebView2 retains the handler; token is writable for this synchronous call.
        unsafe {
            webview
                .webview()
                .add_WindowCloseRequested(&handler, &mut token)
        }
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_urls_stay_inside_the_plugin_or_go_to_the_browser() {
        let root = std::env::temp_dir().join(format!("deckly-popup-urls-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("settings.html"), "page").unwrap();
        let root = root.canonicalize().unwrap();
        assert_eq!(
            target(&root, &format!("{ORIGIN}settings.html?mode=general#form")),
            Target::Plugin
        );
        assert_eq!(target(&root, "about:blank"), Target::Plugin);
        for suffix in ["../secret", "%2e%2e/secret", "missing.html"] {
            assert_eq!(target(&root, &format!("{ORIGIN}{suffix}")), Target::Deny);
        }
        assert_eq!(target(&root, "https://example.com/help"), Target::Browser);
        assert_eq!(
            target(&root, "http://deckly.localhost.evil/settings.html"),
            Target::Browser
        );
        assert_eq!(target(&root, "javascript:alert(1)"), Target::Deny);
        assert_eq!(target(&root, "file:///etc/passwd"), Target::Deny);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn popup_dimensions_are_bounded() {
        assert_eq!(
            popup_size(Some((480.0, 700.0))),
            LogicalSize::new(480.0, 700.0)
        );
        assert_eq!(
            popup_size(Some((-1.0, 1e20))),
            LogicalSize::new(200.0, 2048.0)
        );
        assert_eq!(popup_size(Some((f64::NAN, 10.0))), popup_size(None));
    }

    #[cfg(windows)]
    #[test]
    fn repeated_popups_keep_their_opener_and_release_windows_on_close() {
        use std::cell::Cell;
        use std::time::{Duration, Instant};
        use tao::event::{Event, WindowEvent};
        use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
        use tao::platform::run_return::EventLoopExtRunReturn;
        use tao::platform::windows::{EventLoopBuilderExtWindows, WindowExtWindows};
        use wry::PageLoadEvent;

        fn wait(
            stage: &str,
            event_loop: &mut EventLoop<Command>,
            popups: &Popups,
            mut done: impl FnMut() -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(15);
            event_loop.run_return(|event, _, flow| {
                *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(20));
                match event {
                    Event::UserEvent(Command::ClosePopup(id)) => popups.close(id),
                    Event::UserEvent(Command::PopupTitle(id, title)) => popups.title(id, &title),
                    Event::WindowEvent {
                        event: WindowEvent::CloseRequested | WindowEvent::Destroyed,
                        window_id,
                        ..
                    } => popups.close(window_id),
                    _ => {}
                }
                if done() || Instant::now() >= deadline {
                    *flow = ControlFlow::Exit;
                }
            });
            assert!(
                done(),
                "timed out waiting for the real popup webview: {stage}"
            );
        }

        let root =
            std::env::temp_dir().join(format!("deckly-popup-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("parent.html"),
            "<!doctype html><title>Parent</title>",
        )
        .unwrap();
        std::fs::write(
            root.join("child.html"),
            "<!doctype html><title>Fixture child</title><body>Settings</body>",
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        let mut event_loop = EventLoopBuilder::<Command>::with_user_event()
            .with_any_thread(true)
            .build();
        let window = WindowBuilder::new()
            .with_visible(false)
            .with_focused(false)
            .build(&event_loop)
            .unwrap();
        let popups = Popups::new(
            (*event_loop).clone(),
            event_loop.create_proxy(),
            root.clone(),
            "Fixture".into(),
            false,
            Some(window.hwnd()),
        );
        let ready = Rc::new(Cell::new(false));
        let loaded = ready.clone();
        let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let builder = popups
            .configure(WebViewBuilder::new())
            .with_url(format!("{ORIGIN}parent.html"))
            .with_on_page_load_handler(move |event, _| {
                if matches!(event, PageLoadEvent::Finished) {
                    loaded.set(true);
                }
            });
        let webview = build_webview(builder, &window).unwrap();
        wait("parent load", &mut event_loop, &popups, || ready.get());
        webview
            .evaluate_script("window.fixtureSettings = { value: 7 };")
            .unwrap();

        for expected in 8..=10 {
            webview
                .evaluate_script("window.open('./child.html');")
                .unwrap();
            wait("popup load", &mut event_loop, &popups, || {
                popups
                    .windows
                    .borrow()
                    .values()
                    .any(|p| p.window.title() == "Fixture child - Deckly")
            });
            assert_eq!(popups.windows.borrow().len(), 1);
            {
                let windows = popups.windows.borrow();
                let popup = windows.values().next().unwrap();
                let reply = messages.clone();
                popup._webview.evaluate_script_with_callback(
                    "(function () { try { window.opener.fixtureSettings.value += 1; return { value: window.opener.fixtureSettings.value, url: location.href }; } catch (error) { return { error: String(error), url: location.href, opener: !!window.opener }; } })()",
                    move |result| reply.lock().unwrap().push(result),
                ).unwrap();
            }
            wait("opener round trip", &mut event_loop, &popups, || {
                !messages.lock().unwrap().is_empty()
            });
            let message: serde_json::Value =
                serde_json::from_str(&messages.lock().unwrap().remove(0)).unwrap();
            println!("Popup round trip: {message}");
            assert_eq!(message["value"], expected);
            assert_eq!(message["url"], format!("{ORIGIN}child.html"));
            {
                let windows = popups.windows.borrow();
                windows
                    .values()
                    .next()
                    .unwrap()
                    ._webview
                    .evaluate_script("window.close();")
                    .unwrap();
            }
            wait("popup close", &mut event_loop, &popups, || {
                popups.windows.borrow().is_empty()
            });
            assert_eq!(
                Rc::strong_count(&popups),
                1,
                "closed views retained the popup owner"
            );
        }
        drop(webview);
        drop(popups);
        drop(window);
        std::fs::remove_dir_all(root).unwrap();
    }
}
