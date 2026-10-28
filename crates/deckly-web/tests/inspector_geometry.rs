#![cfg(windows)]

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::platform::windows::{EventLoopBuilderExtWindows, WindowExtWindows};
use tao::window::WindowBuilder;

struct Helper(Child);
impl Drop for Helper {
    fn drop(&mut self) {
        self.0.stdin.take();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn inspector_client_fills_the_requested_rectangle_without_a_native_frame() {
    let dir =
        std::env::temp_dir().join(format!("deckly-inspector-geometry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let page = dir.join("settings.html");
    std::fs::write(
        &page,
        "<!doctype html><body style='background:#2d2d2d'>Settings</body>",
    )
    .unwrap();
    let mut event_loop = EventLoopBuilder::<()>::with_user_event()
        .with_any_thread(true)
        .build();
    let parent = WindowBuilder::new()
        .with_inner_size(LogicalSize::new(852.0, 742.0))
        .with_visible(false)
        .with_focused(false)
        .build(&event_loop)
        .unwrap();
    let mut child = Helper(
        Command::new(env!("CARGO_BIN_EXE_deckly-web"))
            .arg("inspector")
            .arg(&page)
            .args([
                "1",
                "geometry",
                "registerPropertyInspector",
                "{}",
                "{}",
                "Geometry test",
            ])
            .arg(parent.hwnd().to_string())
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    for expected in [
        [150, 600, 400, 140],
        [25, 30, 720, 300],
        [150, 600, 320, 120],
    ] {
        let [x, y, w, h] = expected;
        writeln!(child.0.stdin.as_mut().unwrap(), "place {x} {y} {w} {h}").unwrap();
        let started = Instant::now();
        let mut geometry = None;
        let mut webview = None;
        event_loop.run_return(|_, _, flow| {
            *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(20));
            geometry = win32::child_geometry(parent.hwnd(), child.0.id(), false).or(geometry);
            webview = win32::child_geometry(parent.hwnd(), child.0.id(), true).or(webview);
            let filled = geometry.is_some_and(|g| g.outer == expected && g.client == expected)
                && webview.is_some_and(|g| g.outer == expected && g.client == expected);
            if (filled && started.elapsed() > Duration::from_millis(250))
                || started.elapsed() > Duration::from_secs(5)
            {
                *flow = ControlFlow::Exit;
            }
        });
        let geometry = geometry.expect("helper did not create its child window");
        let webview = webview.expect("helper did not create its webview container");
        println!("Inspector: {geometry:?}; webview: {webview:?}");
        assert_eq!(geometry.outer, expected);
        assert_eq!(geometry.client, expected);
        assert_eq!(webview.outer, expected);
        assert_eq!(webview.client, expected);
        assert_eq!(
            geometry.style & 0x00c4_0000,
            0,
            "caption or resize frame remained"
        );
        assert_eq!(
            geometry.extended_style & 0x0000_0301,
            0,
            "native edge remained"
        );
    }
    drop(child);
    drop(parent);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Read-only Win32 geometry checks for our own hidden test windows.
mod win32 {
    use std::ffi::c_void;
    type Hwnd = *mut c_void;
    #[derive(Default)]
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[derive(Default)]
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[derive(Clone, Copy, Debug)]
    pub struct Geometry {
        pub outer: [i32; 4],
        pub client: [i32; 4],
        pub style: isize,
        pub extended_style: isize,
    }
    struct Search {
        parent: Hwnd,
        pid: u32,
        result: Option<Geometry>,
        webview: bool,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumChildWindows(
            parent: Hwnd,
            callback: unsafe extern "system" fn(Hwnd, isize) -> i32,
            data: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
        fn GetWindowRect(window: Hwnd, rect: *mut Rect) -> i32;
        fn GetClientRect(window: Hwnd, rect: *mut Rect) -> i32;
        fn ClientToScreen(window: Hwnd, point: *mut Point) -> i32;
        fn ScreenToClient(window: Hwnd, point: *mut Point) -> i32;
        fn GetParent(window: Hwnd) -> Hwnd;
        fn GetWindowLongPtrW(window: Hwnd, index: i32) -> isize;
        fn GetClassNameW(window: Hwnd, text: *mut u16, size: i32) -> i32;
    }
    unsafe extern "system" fn inspect(window: Hwnd, data: isize) -> i32 {
        // Enumeration is synchronous; data and all output buffers stay valid for this callback.
        unsafe {
            let search = &mut *(data as *mut Search);
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            if pid != search.pid {
                return 1;
            }
            if search.webview {
                let mut name = [0u16; 64];
                let len = GetClassNameW(window, name.as_mut_ptr(), name.len() as i32);
                if len <= 0 || String::from_utf16_lossy(&name[..len as usize]) != "WRY_WEBVIEW" {
                    return 1;
                }
            } else if GetParent(window) != search.parent {
                return 1;
            }
            let mut outer = Rect::default();
            let mut client = Rect::default();
            let mut origin = Point::default();
            if GetWindowRect(window, &mut outer) == 0
                || GetClientRect(window, &mut client) == 0
                || ClientToScreen(window, &mut origin) == 0
            {
                return 1;
            }
            let mut outer_origin = Point {
                x: outer.left,
                y: outer.top,
            };
            if ScreenToClient(search.parent, &mut origin) == 0
                || ScreenToClient(search.parent, &mut outer_origin) == 0
            {
                return 1;
            }
            search.result = Some(Geometry {
                outer: [
                    outer_origin.x,
                    outer_origin.y,
                    outer.right - outer.left,
                    outer.bottom - outer.top,
                ],
                client: [
                    origin.x,
                    origin.y,
                    client.right - client.left,
                    client.bottom - client.top,
                ],
                style: GetWindowLongPtrW(window, -16),
                extended_style: GetWindowLongPtrW(window, -20),
            });
        }
        1
    }
    pub fn child_geometry(parent: isize, pid: u32, webview: bool) -> Option<Geometry> {
        let mut search = Search {
            parent: parent as Hwnd,
            pid,
            result: None,
            webview,
        };
        // Our hidden parent is alive; the synchronous callback borrows only this local search.
        unsafe {
            EnumChildWindows(search.parent, inspect, &mut search as *mut _ as isize);
        }
        search.result
    }
}
