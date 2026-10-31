//! Text that keys cannot carry. Chromium and Electron apps on Wayland keep a
//! key's character in 16 bits, so an emoji typed into them turns into another
//! symbol, and nothing tells which kind of client has focus. So such text goes
//! through the clipboard with the wlr data control protocol: what is there is
//! saved, the text takes its place, Shift+Insert pastes it, and the saved
//! content goes back. Shift+Insert because Ctrl+V does nothing in a terminal.
//!
//! Terminals paste the primary selection on that key, so it is treated the same
//! way. A Wayland clipboard lives in the program that offers it, so a thread
//! keeps offering the saved content until something else is copied.

use super::Failure;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::pipe::{PipeFlags, pipe_with};
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_client::backend::WaylandError;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::{
    self, EVT_DATA_OFFER_OPCODE, ZwlrDataControlDeviceV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::{
    self, ZwlrDataControlOfferV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::{
    self, ZwlrDataControlSourceV1,
};

const CLIPBOARD: usize = 0;
const PRIMARY: usize = 1;
/// The names clients ask for text by.
const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];
/// Asks clipboard histories not to keep what is only passing through.
const HISTORY_HINT: &str = "x-kde-passwordManagerHint";
/// The most types of one clipboard entry that are looked at.
const MAX_TYPES: usize = 64;
/// The most clipboard content kept. A type that does not fit is not restored.
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// How long saving the clipboard may hold up a key press.
const SAVE_TIME: Duration = Duration::from_millis(300);
/// Lets the focused client hear of the new clipboard before the key.
const OFFER_GAP: Duration = Duration::from_millis(30);
/// How long a client gets to ask for the text.
const ANSWER_TIME: Duration = Duration::from_millis(400);
/// A client may ask more than once; the old content returns after this.
const READ_GAP: Duration = Duration::from_millis(100);

/// One piece of clipboard content in each of its types.
type Content = Vec<(String, Arc<[u8]>)>;
type Types = Mutex<Vec<String>>;

/// One paste at a time: a second would save the first one's text as the
/// user's clipboard.
static BUSY: Mutex<()> = Mutex::new(());

/// What a source of ours offers, and in which selection.
struct Held {
    content: Content,
    which: usize,
}

#[derive(Default)]
struct State {
    /// The current offers, by selection.
    offers: [Option<ZwlrDataControlOfferV1>; 2],
    /// Sources of ours that are still offered.
    sources: usize,
    /// A client asked for what a source offers.
    asked: bool,
    /// Something else replaced a source of ours, by selection.
    taken: [bool; 2],
    /// The compositor ended this device.
    finished: bool,
}

impl State {
    fn set_offer(&mut self, which: usize, offer: Option<ZwlrDataControlOfferV1>) {
        if let Some(old) = std::mem::replace(&mut self.offers[which], offer) {
            old.destroy();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ZwlrDataControlManagerV1);

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_data_control_device_v1::Event;
        match event {
            Event::Selection { id } => state.set_offer(CLIPBOARD, id),
            Event::PrimarySelection { id } => state.set_offer(PRIMARY, id),
            Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(State, ZwlrDataControlDeviceV1, [
        EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, Types::default()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, Types> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        types: &Types,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            let mut types = types.lock().unwrap_or_else(|e| e.into_inner());
            if types.len() < MAX_TYPES {
                types.push(mime_type);
            }
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, Held> for State {
    fn event(
        state: &mut Self,
        source: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        held: &Held,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_data_control_source_v1::Event;
        match event {
            Event::Send { mime_type, fd } => {
                state.asked = true;
                if let Some((_, bytes)) = held.content.iter().find(|(t, _)| *t == mime_type) {
                    write_out(fd, bytes.clone());
                }
            }
            Event::Cancelled => {
                source.destroy();
                state.sources = state.sources.saturating_sub(1);
                state.taken[held.which] = true;
            }
            _ => {}
        }
    }
}

fn unavailable(e: impl std::fmt::Display) -> Failure {
    Failure::Unavailable(e.to_string())
}

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure::Failed(e.to_string())
}

/// Writes on a thread of its own: a client may be slow to read, or never read.
fn write_out(fd: OwnedFd, bytes: Arc<[u8]>) {
    let _ = std::thread::Builder::new()
        .name("deckly-clipboard-write".into())
        .spawn(move || {
            let _ = File::from(fd).write_all(&bytes);
        });
}

/// Reads to the end, `None` past the limit or the deadline.
fn read_all(mut reader: File, limit: usize, deadline: Instant) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let timeout = Timespec::try_from(left).ok()?;
        let mut fds = [PollFd::new(&reader, PollFlags::IN)];
        if poll(&mut fds, Some(&timeout)).ok()? == 0 {
            return None;
        }
        match reader.read(&mut chunk).ok()? {
            0 => return Some(out),
            n if out.len() + n > limit => return None,
            n => out.extend_from_slice(chunk.get(..n)?),
        }
    }
}

/// Text first: it is what the user is most likely to miss, and an image of
/// many megabytes must not use up the time for it.
fn in_saving_order(mut types: Vec<String>) -> Vec<String> {
    types.sort_by_key(|t| !(TEXT_TYPES.contains(&t.as_str()) || t.starts_with("text/")));
    types
}

fn save(connection: &Connection, offer: &ZwlrDataControlOfferV1, deadline: Instant) -> Content {
    let types = offer
        .data::<Types>()
        .map(|t| t.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_default();
    let mut left = MAX_BYTES;
    let mut content = Content::new();
    for mime in in_saving_order(types) {
        let Ok((reader, writer)) = pipe_with(PipeFlags::CLOEXEC) else {
            break;
        };
        offer.receive(mime.clone(), writer.as_fd());
        if connection.flush().is_err() {
            break;
        }
        drop(writer);
        if let Some(bytes) = read_all(File::from(reader), left, deadline) {
            left -= bytes.len();
            content.push((mime, Arc::from(bytes)));
        }
    }
    content
}

fn text_content(text: &str) -> Content {
    let bytes: Arc<[u8]> = Arc::from(text.as_bytes());
    let mut content: Content = TEXT_TYPES
        .iter()
        .map(|&mime| (mime.to_owned(), bytes.clone()))
        .collect();
    content.push((HISTORY_HINT.to_owned(), Arc::from(&b"secret"[..])));
    content
}

/// The clipboard, seen through a data control device.
struct Selections {
    queue: EventQueue<State>,
    state: State,
    manager: ZwlrDataControlManagerV1,
    device: ZwlrDataControlDeviceV1,
}

impl Selections {
    /// Puts `content` into a selection; empty content clears it.
    fn offer(&mut self, which: usize, content: Content) {
        let source = (!content.is_empty()).then(|| {
            let held = Held {
                content: content.clone(),
                which,
            };
            let source = self.manager.create_data_source(&self.queue.handle(), held);
            for (mime, _) in content {
                source.offer(mime);
            }
            self.state.sources += 1;
            source
        });
        match which {
            CLIPBOARD => self.device.set_selection(source.as_ref()),
            _ => self.device.set_primary_selection(source.as_ref()),
        }
    }

    /// Answers requests until `done` or the deadline.
    fn pump(&mut self, until: Instant, done: impl Fn(&State) -> bool) -> Result<(), Failure> {
        loop {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(failed)?;
            self.queue.flush().map_err(failed)?;
            if done(&self.state) {
                return Ok(());
            }
            let Some(left) = until.checked_duration_since(Instant::now()) else {
                return Ok(());
            };
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let timeout = Timespec::try_from(left).map_err(failed)?;
            let fd = guard.connection_fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            if poll(&mut fds, Some(&timeout)).map_err(failed)? == 0 {
                continue;
            }
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => return Err(failed(e)),
            }
        }
    }

    /// Keeps offering the restored content from now on.
    fn keep_offering(mut self) -> Result<(), Failure> {
        self.queue.roundtrip(&mut self.state).map_err(failed)?;
        if self.state.sources == 0 {
            return Ok(());
        }
        std::thread::Builder::new()
            .name("deckly-clipboard".into())
            .spawn(move || {
                while self.state.sources > 0
                    && !self.state.finished
                    && self.queue.blocking_dispatch(&mut self.state).is_ok()
                {}
            })
            .map(|_| ())
            .map_err(failed)
    }
}

/// Pastes `text` with `press`, which sends the key. `Unavailable` means the
/// clipboard was not touched and nothing was pressed.
pub fn paste(text: &str, press: impl FnOnce() -> Result<(), Failure>) -> Result<(), Failure> {
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    let connection = Connection::connect_to_env().map_err(unavailable)?;
    let (globals, queue) = registry_queue_init::<State>(&connection).map_err(unavailable)?;
    let handle = queue.handle();
    let seat: WlSeat = globals.bind(&handle, 1..=1, ()).map_err(unavailable)?;
    let manager: ZwlrDataControlManagerV1 =
        globals.bind(&handle, 1..=2, ()).map_err(unavailable)?;
    let device = manager.get_data_device(&seat, &handle, ());
    let which: &[usize] = if device.version() >= 2 {
        &[CLIPBOARD, PRIMARY]
    } else {
        &[CLIPBOARD]
    };
    let mut selections = Selections {
        queue,
        state: State::default(),
        manager,
        device,
    };
    let Selections { queue, state, .. } = &mut selections;
    queue.roundtrip(state).map_err(unavailable)?;

    let deadline = Instant::now() + SAVE_TIME;
    let mut saved = [Content::new(), Content::new()];
    for &w in which {
        if let Some(offer) = &state.offers[w] {
            saved[w] = save(&connection, offer, deadline);
        }
    }
    // The clipboard would be lost for good; a wrong emoji is the lesser harm.
    if state.offers[CLIPBOARD].is_some() && saved[CLIPBOARD].is_empty() {
        return Err(unavailable("the clipboard could not be saved"));
    }

    for &w in which {
        selections.offer(w, text_content(text));
    }
    selections
        .queue
        .roundtrip(&mut selections.state)
        .map_err(unavailable)?;
    std::thread::sleep(OFFER_GAP);

    let pressed = press();
    let answered = if pressed.is_ok() {
        selections
            .pump(Instant::now() + ANSWER_TIME, |state| state.asked)
            .and_then(|()| selections.pump(Instant::now() + READ_GAP, |_| false))
    } else {
        Ok(())
    };

    for &w in which {
        // Something copied in the meantime is newer than what was saved.
        if !selections.state.taken[w] {
            let content = std::mem::take(&mut saved[w]);
            selections.offer(w, content);
        }
    }
    pressed.and(answered).and(selections.keep_offering())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> (File, File) {
        let (reader, writer) = pipe_with(PipeFlags::CLOEXEC).unwrap();
        (File::from(reader), File::from(writer))
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn reads_to_the_end() {
        let (reader, mut writer) = pipe();
        writer.write_all(b"hello").unwrap();
        drop(writer);
        assert_eq!(
            read_all(reader, 100, soon()).as_deref(),
            Some(&b"hello"[..])
        );
    }

    #[test]
    fn content_past_the_limit_is_refused() {
        let (reader, mut writer) = pipe();
        writer.write_all(b"hello").unwrap();
        drop(writer);
        assert_eq!(read_all(reader, 4, soon()), None);
    }

    #[test]
    fn a_silent_program_cannot_hold_up_the_key() {
        let (reader, writer) = pipe();
        let deadline = Instant::now() + Duration::from_millis(20);
        assert_eq!(read_all(reader, 100, deadline), None);
        drop(writer);
    }

    #[test]
    fn text_is_saved_before_anything_large() {
        let types = ["image/png", "text/html", "UTF8_STRING", "x-special/thing"];
        let ordered = in_saving_order(types.map(str::to_owned).to_vec());
        assert_eq!(
            ordered,
            ["text/html", "UTF8_STRING", "image/png", "x-special/thing"]
        );
    }

    #[test]
    fn pasted_text_stays_out_of_clipboard_histories() {
        let content = text_content("\u{1f600}");
        let find = |mime: &str| content.iter().find(|(t, _)| t == mime).map(|(_, b)| &**b);
        assert_eq!(
            find("text/plain;charset=utf-8"),
            Some("\u{1f600}".as_bytes())
        );
        assert_eq!(find(HISTORY_HINT), Some(&b"secret"[..]));
    }
}
