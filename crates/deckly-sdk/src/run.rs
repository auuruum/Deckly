//! The run loop: the handshake, a reader thread for standard input, a
//! timer thread, and the dispatcher that calls the actions.

use crate::key::{KeyInner, Output, TimerCtl};
use crate::{
    Action, ButtonHandler, ChoicesHandler, Error, Globals, GlobalsHandler, Key, Plugin, Result,
    SDK_VERSION,
};
use deckly_plugin_protocol::limits::{MAX_CHOICES, MAX_ERROR_CHARS};
use deckly_plugin_protocol::{
    Choice, HostMessage, KeyInfo, MIN_PROTOCOL, PROTOCOL, PluginMessage, Reader, Received, truncate,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a plugin may take to stop after Deckly closed its input before
/// it exits anyway (the host kills it a little later).
const EXIT_GRACE: Duration = Duration::from_secs(2);

enum Event {
    Host(HostMessage),
    Tick { context: String, generation: u64 },
    End,
}

/// What every new key needs.
pub(crate) struct Shared {
    out: Arc<Output>,
    timers: Sender<TimerCtl>,
    globals: Globals,
}

/// One key with its action value, behind a type-erased face.
pub(crate) trait Slot: Send {
    fn appear(&mut self);
    fn disappear(&mut self);
    fn down(&mut self, info: KeyInfo);
    fn up(&mut self, info: KeyInfo);
    fn settings(&mut self, info: KeyInfo);
    fn tick(&mut self, generation: u64);
    fn button(&mut self, info: KeyInfo, field: &str);
    fn globals_changed(&mut self);
    /// Ends a key made only to answer a button, without `key_disappear`.
    fn discard(&mut self);
}

type ChoicesFn = fn(&str, &Value, &Globals) -> std::result::Result<Vec<Choice>, String>;

/// What the SDK knows of one action type.
#[derive(Clone, Copy)]
pub(crate) struct Maker {
    id: &'static str,
    make: fn(KeyInfo, &Shared) -> Box<dyn Slot>,
    choices: ChoicesFn,
}

pub(crate) fn maker<A: Action>() -> Maker {
    Maker {
        id: A::ID,
        make: make::<A>,
        choices: choices_of::<A>,
    }
}

fn choices_of<A: Action>(
    field: &str,
    settings: &Value,
    globals: &Globals,
) -> std::result::Result<Vec<Choice>, String> {
    let settings = serde_json::from_value(settings.clone()).unwrap_or_default();
    A::choices(field, &settings, globals)
}

fn make<A: Action>(info: KeyInfo, shared: &Shared) -> Box<dyn Slot> {
    let settings = read_settings::<A>(&info);
    let key = Key::<A>::new(KeyInner {
        context: info.context.clone(),
        info: Mutex::new(info),
        settings: Mutex::new(settings),
        alive: AtomicBool::new(true),
        timer: AtomicU64::new(0),
        out: shared.out.clone(),
        timers: shared.timers.clone(),
        globals: shared.globals.clone(),
    });
    Box::new(Instance {
        action: A::default(),
        key,
    })
}

fn read_settings<A: Action>(info: &KeyInfo) -> A::Settings {
    match serde_json::from_value(info.settings.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "deckly-sdk: settings of key {} do not fit {}: {e}; using defaults",
                info.context,
                A::ID
            );
            A::Settings::default()
        }
    }
}

struct Instance<A: Action> {
    action: A,
    key: Key<A>,
}

impl<A: Action> Instance<A> {
    /// Takes what moved (place, device, state) from a newer description;
    /// settings stay as the plugin last knew them.
    fn update(&self, info: KeyInfo) {
        let mut mine = self
            .key
            .inner
            .info
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let settings = std::mem::take(&mut mine.settings);
        *mine = KeyInfo { settings, ..info };
    }
}

impl<A: Action> Slot for Instance<A> {
    fn appear(&mut self) {
        self.action.key_appear(&self.key);
    }

    fn disappear(&mut self) {
        self.key.stop_timer();
        self.action.key_disappear(&self.key);
        self.key.inner.alive.store(false, Ordering::Relaxed);
        self.key.inner.out.forget(&self.key.inner.context);
    }

    fn down(&mut self, info: KeyInfo) {
        self.update(info);
        self.action.key_down(&self.key);
    }

    fn up(&mut self, info: KeyInfo) {
        self.update(info);
        self.action.key_up(&self.key);
    }

    fn settings(&mut self, info: KeyInfo) {
        let settings = read_settings::<A>(&info);
        *self
            .key
            .inner
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = settings;
        self.update(info);
        self.action.settings_changed(&self.key);
    }

    fn tick(&mut self, generation: u64) {
        if self.key.inner.timer.load(Ordering::Relaxed) == generation && self.key.is_visible() {
            self.action.tick(&self.key);
        }
    }

    fn button(&mut self, info: KeyInfo, field: &str) {
        // The form edits settings the plugin may not have heard of yet.
        *self
            .key
            .inner
            .settings
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = read_settings::<A>(&info);
        self.update(info);
        self.action.button(&self.key, field);
    }

    fn globals_changed(&mut self) {
        self.action.global_settings_changed(&self.key);
    }

    fn discard(&mut self) {
        self.key.stop_timer();
        self.key.inner.alive.store(false, Ordering::Relaxed);
        self.key.inner.out.forget(&self.key.inner.context);
    }
}

pub(crate) fn run<R, W>(plugin: Plugin, input: R, output: W, real: bool) -> Result<()>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let mut reader = Reader::new(input);
    let out = Arc::new(Output::new(Box::new(output)));
    // The handshake: hello, then ready.
    let hello = match reader.read::<HostMessage>()? {
        Some(Received {
            message: HostMessage::Hello(h),
            ..
        }) => h,
        Some(_) => return Err(Error::Protocol("Deckly did not start with hello".into())),
        None => {
            return Err(Error::Protocol(
                "standard input closed before hello; this program is a Deckly plugin, \
                 started by Deckly (run it with --describe to see its manifest)"
                    .into(),
            ));
        }
    };
    if hello.protocol < MIN_PROTOCOL || hello.protocol < plugin.manifest.protocol {
        return Err(Error::Protocol(format!(
            "this Deckly speaks protocol {}, the plugin needs {}",
            hello.protocol, plugin.manifest.protocol
        )));
    }
    out.send(
        &PluginMessage::Ready {
            protocol: PROTOCOL.min(hello.protocol),
            sdk: SDK_VERSION.to_owned(),
        },
        None,
    );
    out.send(&PluginMessage::GetGlobalSettings, None);
    let globals = Globals::new(out.clone());

    let (events, rx) = channel();
    let (timers, timer_rx) = channel();
    let (asks, ask_rx) = channel();
    spawn("deckly-sdk-forms", {
        let answerer = Answerer {
            makers: plugin.makers.clone(),
            plugin: plugin.on_choices.clone(),
            globals: globals.clone(),
            out: out.clone(),
        };
        move || answerer.run(ask_rx)
    })?;
    spawn("deckly-sdk-input", {
        let events = events.clone();
        move || read_input(reader, events, real)
    })?;
    spawn("deckly-sdk-timers", {
        let events = events.clone();
        move || run_timers(timer_rx, events)
    })?;
    drop(events);
    let shared = Shared {
        out,
        timers,
        globals,
    };
    let handlers = Handlers {
        button: plugin.on_button.clone(),
        globals: plugin.on_globals.clone(),
        asks,
    };
    dispatch(&plugin.makers, &shared, &handlers, rx);
    Ok(())
}

/// A request for the options of a dynamic choice.
struct Ask {
    request: u64,
    field: String,
    key: Option<KeyInfo>,
}

/// Answers choice requests on a thread of its own, so a slow handler (a
/// network lookup) does not hold up key events.
struct Answerer {
    makers: Vec<Maker>,
    plugin: Option<ChoicesHandler>,
    globals: Globals,
    out: Arc<Output>,
}

impl Answerer {
    fn run(self, rx: Receiver<Ask>) {
        while let Ok(ask) = rx.recv() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.list(&ask)))
                .unwrap_or_else(|_| Err("the plugin failed while listing the options".into()));
            self.out.send(&choices_answer(ask.request, result), None);
        }
    }

    fn list(&self, ask: &Ask) -> std::result::Result<Vec<Choice>, String> {
        match &ask.key {
            Some(key) => {
                let maker = self
                    .makers
                    .iter()
                    .find(|m| m.id == key.action)
                    .ok_or("the plugin has no such action")?;
                (maker.choices)(&ask.field, &key.settings, &self.globals)
            }
            None => match &self.plugin {
                Some(handler) => handler(&ask.field, &self.globals),
                None => Err(format!("no options for {}", ask.field)),
            },
        }
    }
}

/// The `choices` message for a handler's result, cut to the limits.
pub(crate) fn choices_answer(
    request: u64,
    result: std::result::Result<Vec<Choice>, String>,
) -> PluginMessage {
    match result {
        Ok(options) => {
            if options.len() > MAX_CHOICES {
                eprintln!("deckly-sdk: only the first {MAX_CHOICES} options are sent");
            }
            PluginMessage::Choices {
                request,
                options: options
                    .into_iter()
                    .take(MAX_CHOICES)
                    .map(|c| Choice::new(&c.value, &c.label))
                    .collect(),
                error: None,
            }
        }
        Err(e) => PluginMessage::Choices {
            request,
            options: Vec::new(),
            error: Some(truncate(&e, MAX_ERROR_CHARS).to_owned()),
        },
    }
}

/// The plugin's own handlers, called by the dispatcher.
struct Handlers {
    button: Option<ButtonHandler>,
    globals: Option<GlobalsHandler>,
    asks: Sender<Ask>,
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(drop)
        .map_err(Error::Io)
}

fn read_input<R: Read>(mut reader: Reader<R>, events: Sender<Event>, real: bool) {
    loop {
        match reader.read::<HostMessage>() {
            Ok(Some(r)) => {
                if events.send(Event::Host(r.message)).is_err() {
                    return;
                }
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("deckly-sdk: reading from Deckly: {e}");
                break;
            }
        }
    }
    let _ = events.send(Event::End);
    if real {
        // A handler that never returns must not keep the plugin alive.
        std::thread::sleep(EXIT_GRACE);
        std::process::exit(0);
    }
}

fn run_timers(rx: Receiver<TimerCtl>, events: Sender<Event>) {
    // context -> (generation, next, period)
    let mut timers: HashMap<String, (u64, Instant, Duration)> = HashMap::new();
    loop {
        let next = timers.values().map(|t| t.1).min();
        let ctl = match next {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            },
        };
        match ctl {
            Some(TimerCtl::Set {
                context,
                generation,
                period,
            }) => {
                timers.insert(context, (generation, Instant::now() + period, period));
            }
            Some(TimerCtl::Cancel { context }) => {
                timers.remove(&context);
            }
            None => {
                let now = Instant::now();
                for (context, (generation, next, period)) in &mut timers {
                    if *next > now {
                        continue;
                    }
                    // A late tick is not repeated to catch up.
                    *next = (*next + *period).max(now);
                    let tick = Event::Tick {
                        context: context.clone(),
                        generation: *generation,
                    };
                    if events.send(tick).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

fn dispatch(makers: &[Maker], shared: &Shared, handlers: &Handlers, rx: Receiver<Event>) {
    let mut keys: HashMap<String, Box<dyn Slot>> = HashMap::new();
    let create = |info: &KeyInfo| {
        makers
            .iter()
            .find(|m| m.id == info.action)
            .map(|m| (m.make)(info.clone(), shared))
    };
    while let Ok(event) = rx.recv() {
        match event {
            Event::Host(HostMessage::KeyAppear { key }) => {
                if let Some(mut old) = keys.remove(&key.context) {
                    old.disappear();
                }
                if let Some(mut slot) = create(&key) {
                    slot.appear();
                    keys.insert(key.context, slot);
                }
            }
            Event::Host(HostMessage::KeyDisappear { key }) => {
                if let Some(mut slot) = keys.remove(&key.context) {
                    slot.disappear();
                }
            }
            Event::Host(HostMessage::KeyDown { key }) => {
                // A key pressed before it appeared (a Multi Action step).
                if !keys.contains_key(&key.context)
                    && let Some(mut slot) = create(&key)
                {
                    slot.appear();
                    keys.insert(key.context.clone(), slot);
                }
                if let Some(slot) = keys.get_mut(&key.context) {
                    slot.down(key);
                }
            }
            Event::Host(HostMessage::KeyUp { key }) => {
                if let Some(slot) = keys.get_mut(&key.context) {
                    slot.up(key);
                }
            }
            Event::Host(HostMessage::Settings { key }) => {
                if let Some(slot) = keys.get_mut(&key.context) {
                    slot.settings(key);
                }
            }
            Event::Host(HostMessage::GlobalSettings { settings }) => {
                shared.globals.received(settings);
                if let Some(handler) = &handlers.globals {
                    handler(&shared.globals);
                }
                for slot in keys.values_mut() {
                    slot.globals_changed();
                }
            }
            Event::Host(HostMessage::GetChoices {
                request,
                field,
                key,
            }) => {
                let _ = handlers.asks.send(Ask {
                    request,
                    field,
                    key,
                });
            }
            Event::Host(HostMessage::Button {
                field,
                key: Some(key),
            }) => {
                if let Some(slot) = keys.get_mut(&key.context) {
                    slot.button(key, &field);
                } else if let Some(mut slot) = create(&key) {
                    // A key in the editor only: made for the press, then dropped.
                    slot.button(key, &field);
                    slot.discard();
                }
            }
            Event::Host(HostMessage::Button { field, key: None }) => {
                if let Some(handler) = &handlers.button {
                    handler(&field, &shared.globals);
                }
            }
            Event::Host(_) => {}
            Event::Tick {
                context,
                generation,
            } => {
                if let Some(slot) = keys.get_mut(&context) {
                    slot.tick(generation);
                }
            }
            Event::End => break,
        }
    }
    for (_, mut slot) in keys.drain() {
        slot.disappear();
    }
}
