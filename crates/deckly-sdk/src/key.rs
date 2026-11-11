//! Key handles and the shared output to Deckly.

use crate::Action;
use deckly_plugin_protocol::limits::{MAX_SETTINGS_BYTES, MAX_TITLE_CHARS};
use deckly_plugin_protocol::{
    Face, ImageFormat, KeyInfo, PluginMessage, Validate, encode_blob, encode_message, json_len,
    truncate,
};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// What was last sent for one key, so an update equal to it is skipped.
#[derive(Default)]
struct Sent {
    title: Option<Option<String>>,
    state: Option<u8>,
    face: Option<Option<Face>>,
    image: Option<Option<u64>>,
}

/// The plugin's standard output, shared by every key.
pub(crate) struct Output {
    writer: Mutex<Box<dyn Write + Send>>,
    sent: Mutex<HashMap<String, Sent>>,
    broken: AtomicBool,
}

impl Output {
    pub(crate) fn new(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Mutex::new(writer),
            sent: Mutex::new(HashMap::new()),
            broken: AtomicBool::new(false),
        }
    }

    /// Writes a message, with a blob right before it. Both go out under
    /// one lock, so nothing else lands between them.
    pub(crate) fn send(&self, message: &PluginMessage, blob: Option<&[u8]>) {
        if self.broken.load(Ordering::Relaxed) {
            return;
        }
        // The SDK never sends what the host would refuse.
        if let Err(e) = message.validate() {
            eprintln!("deckly-sdk: not sent: {e}");
            return;
        }
        let frames = blob
            .map(encode_blob)
            .transpose()
            .and_then(|b| Ok((b, encode_message(message)?)));
        let (blob, frame) = match frames {
            Ok(f) => f,
            Err(e) => {
                eprintln!("deckly-sdk: not sent: {e}");
                return;
            }
        };
        let mut w = lock(&self.writer);
        let result = blob
            .map_or(Ok(()), |b| w.write_all(&b))
            .and_then(|()| w.write_all(&frame))
            .and_then(|()| w.flush());
        if result.is_err() {
            // Deckly is gone; the reader sees the end of input and stops.
            self.broken.store(true, Ordering::Relaxed);
        }
    }

    /// Runs `update` on what was last sent for `context`; sends only when
    /// it returns true.
    fn changed(&self, context: &str, update: impl FnOnce(&mut Sent) -> bool) -> bool {
        update(lock(&self.sent).entry(context.to_owned()).or_default())
    }

    pub(crate) fn forget(&self, context: &str) {
        lock(&self.sent).remove(context);
    }
}

/// Tells the timer thread what to do.
pub(crate) enum TimerCtl {
    Set {
        context: String,
        generation: u64,
        period: Duration,
    },
    Cancel {
        context: String,
    },
}

pub(crate) struct KeyInner<S> {
    pub(crate) context: String,
    pub(crate) info: Mutex<KeyInfo>,
    pub(crate) settings: Mutex<S>,
    pub(crate) alive: AtomicBool,
    pub(crate) timer: AtomicU64,
    pub(crate) out: Arc<Output>,
    pub(crate) timers: Sender<TimerCtl>,
}

/// One key showing an action. Cheap to clone, and `Send`: hand it to a
/// worker thread to update the key from there. Calls on a key that has
/// disappeared do nothing.
pub struct Key<A: Action> {
    pub(crate) inner: Arc<KeyInner<A::Settings>>,
    _action: PhantomData<fn() -> A>,
}

impl<A: Action> Clone for Key<A> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _action: PhantomData,
        }
    }
}

/// An image for a key, sent as raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    format: ImageFormat,
    bytes: Arc<[u8]>,
}

impl Image {
    pub fn png(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::of(ImageFormat::Png, bytes)
    }

    pub fn jpeg(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::of(ImageFormat::Jpeg, bytes)
    }

    pub fn svg(text: impl Into<Arc<[u8]>>) -> Self {
        Self::of(ImageFormat::Svg, text)
    }

    /// Raw pixels, four bytes (red, green, blue, alpha) each, row by row.
    pub fn rgba(width: u32, height: u32, pixels: impl Into<Arc<[u8]>>) -> Self {
        Self::of(ImageFormat::Rgba { width, height }, pixels)
    }

    fn of(format: ImageFormat, bytes: impl Into<Arc<[u8]>>) -> Self {
        Self {
            format,
            bytes: bytes.into(),
        }
    }

    fn id(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.format.hash(&mut h);
        self.bytes.hash(&mut h);
        h.finish()
    }
}

impl<A: Action> Key<A> {
    pub(crate) fn new(inner: KeyInner<A::Settings>) -> Self {
        Self {
            inner: Arc::new(inner),
            _action: PhantomData,
        }
    }

    fn live(&self) -> bool {
        self.inner.alive.load(Ordering::Relaxed)
    }

    fn info(&self) -> KeyInfo {
        lock(&self.inner.info).clone()
    }

    /// Deckly's id for this key; stays the same while the key exists.
    pub fn context(&self) -> &str {
        &self.inner.context
    }

    /// The device showing the key; empty in the editor only.
    pub fn device(&self) -> String {
        self.info().device
    }

    pub fn column(&self) -> u8 {
        self.info().column
    }

    pub fn row(&self) -> u8 {
        self.info().row
    }

    /// The state Deckly last reported, for actions with several states.
    pub fn state(&self) -> Option<u8> {
        self.info().state
    }

    /// Whether the key is part of a Multi Action rather than on the device.
    pub fn in_multi_action(&self) -> bool {
        self.info().in_multi_action
    }

    /// Whether the key is still shown; false after it disappeared.
    pub fn is_visible(&self) -> bool {
        self.live()
    }

    /// The key's settings.
    pub fn settings(&self) -> A::Settings {
        lock(&self.inner.settings).clone()
    }

    /// Stores new settings for the key; Deckly saves them in the profile.
    pub fn set_settings(&self, settings: A::Settings) {
        let value = match serde_json::to_value(&settings) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                eprintln!("deckly-sdk: settings must serialize to a JSON object; not saved");
                return;
            }
            Err(e) => {
                eprintln!("deckly-sdk: cannot serialize settings: {e}");
                return;
            }
        };
        if json_len(&value) > MAX_SETTINGS_BYTES {
            eprintln!("deckly-sdk: settings are larger than {MAX_SETTINGS_BYTES} bytes; not saved");
            return;
        }
        *lock(&self.inner.settings) = settings;
        if self.live() {
            self.inner.out.send(
                &PluginMessage::SetSettings {
                    context: self.inner.context.clone(),
                    settings: value,
                },
                None,
            );
        }
    }

    /// Changes the settings in place and stores them.
    pub fn update_settings(&self, change: impl FnOnce(&mut A::Settings)) {
        let mut settings = self.settings();
        change(&mut settings);
        self.set_settings(settings);
    }

    /// Shows a title on the key in every state. The user's own title, when
    /// they set one, wins.
    pub fn set_title(&self, title: impl AsRef<str>) {
        let title = truncate(title.as_ref(), MAX_TITLE_CHARS).to_owned();
        self.send_title(Some(title));
    }

    /// Goes back to the title from the manifest.
    pub fn clear_title(&self) {
        self.send_title(None);
    }

    fn send_title(&self, title: Option<String>) {
        if !self.live() {
            return;
        }
        let context = &self.inner.context;
        let new = self.inner.out.changed(context, |s| {
            let same = s.title.as_ref() == Some(&title);
            s.title = Some(title.clone());
            !same
        });
        if new {
            let message = PluginMessage::SetTitle {
                context: context.clone(),
                title,
                state: None,
            };
            self.inner.out.send(&message, None);
        }
    }

    /// Moves a key with several states to another state.
    pub fn set_state(&self, state: u8) {
        if !self.live() {
            return;
        }
        let context = &self.inner.context;
        let new = self.inner.out.changed(context, |s| {
            let same = s.state == Some(state);
            s.state = Some(state);
            !same
        });
        if new {
            self.inner.out.send(
                &PluginMessage::SetState {
                    context: context.clone(),
                    state,
                },
                None,
            );
        }
    }

    /// Shows a face drawn by Deckly with the user's fonts and key style.
    pub fn set_face(&self, face: Face) {
        self.send_face(Some(face));
    }

    /// Goes back to the plain key.
    pub fn clear_face(&self) {
        self.send_face(None);
    }

    fn send_face(&self, face: Option<Face>) {
        if !self.live() {
            return;
        }
        let context = &self.inner.context;
        let new = self.inner.out.changed(context, |s| {
            let same = s.face.as_ref() == Some(&face);
            s.face = Some(face.clone());
            !same
        });
        if new {
            let message = PluginMessage::SetFace {
                context: context.clone(),
                face,
            };
            self.inner.out.send(&message, None);
        }
    }

    /// Shows an image on the key in every state.
    pub fn set_image(&self, image: &Image) {
        if image
            .format
            .raw_len()
            .is_some_and(|n| n != image.bytes.len())
        {
            eprintln!("deckly-sdk: raw image size does not match its pixels; not sent");
            return;
        }
        self.send_image(Some(image));
    }

    /// Goes back to the image from the manifest.
    pub fn clear_image(&self) {
        self.send_image(None);
    }

    fn send_image(&self, image: Option<&Image>) {
        if !self.live() {
            return;
        }
        let context = &self.inner.context;
        let id = image.map(Image::id);
        let new = self.inner.out.changed(context, |s| {
            let same = s.image == Some(id);
            s.image = Some(id);
            !same
        });
        if new {
            let message = PluginMessage::SetImage {
                context: context.clone(),
                format: image.map(|i| i.format),
                state: None,
            };
            self.inner
                .out
                .send(&message, image.map(|i| i.bytes.as_ref()));
        }
    }

    /// Shows the short success mark.
    pub fn show_ok(&self) {
        if self.live() {
            let context = self.inner.context.clone();
            self.inner
                .out
                .send(&PluginMessage::ShowOk { context }, None);
        }
    }

    /// Shows the short failure mark.
    pub fn show_alert(&self) {
        if self.live() {
            let context = self.inner.context.clone();
            self.inner
                .out
                .send(&PluginMessage::ShowAlert { context }, None);
        }
    }

    /// Calls [`Action::tick`] every `period` until [`Key::stop_timer`] or
    /// the key disappears. Replaces the key's earlier timer. Periods are at
    /// least 10 ms.
    pub fn every(&self, period: Duration) {
        if !self.live() {
            return;
        }
        let generation = self.inner.timer.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.inner.timers.send(TimerCtl::Set {
            context: self.inner.context.clone(),
            generation,
            period: period.max(Duration::from_millis(10)),
        });
    }

    pub fn stop_timer(&self) {
        self.inner.timer.fetch_add(1, Ordering::Relaxed);
        let _ = self.inner.timers.send(TimerCtl::Cancel {
            context: self.inner.context.clone(),
        });
    }
}
