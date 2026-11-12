use deckly_sdk::prelude::*;
use deckly_sdk::protocol::{ImageFormat, Manifest, PluginMessage};
use deckly_sdk::testing::Harness;
use serde_json::json;
use std::sync::Mutex;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);

/// Keys handed out to other threads by the test actions.
static KEYS: Mutex<Vec<Key<Repeater>>> = Mutex::new(Vec::new());

#[derive(Clone, Default, Serialize, Deserialize)]
struct Text {
    text: String,
}

/// Sets the same title and face on every press.
#[derive(Default)]
struct Repeater;

impl Action for Repeater {
    const ID: &'static str = "com.example.test.repeat";
    const NAME: &'static str = "Repeat";
    type Settings = Text;

    fn states() -> Vec<State> {
        vec![
            State {
                title: "Off".into(),
                image: String::new(),
            },
            State {
                title: "On".into(),
                image: String::new(),
            },
        ]
    }

    fn key_appear(&mut self, key: &Key<Self>) {
        KEYS.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(key.clone());
    }

    fn key_down(&mut self, key: &Key<Self>) {
        let text = key.settings().text;
        key.set_title(&text);
        key.set_face(Face::new().value(&text));
        key.set_state(1);
        key.set_image(&Image::png(vec![1, 2, 3]));
    }

    fn key_up(&mut self, key: &Key<Self>) {
        key.show_ok();
    }
}

fn plugin() -> Plugin {
    Plugin::new("com.example.test", "Test")
        .version("1.2.3")
        .action::<Repeater>()
}

fn kind(m: &PluginMessage) -> &'static str {
    match m {
        PluginMessage::SetTitle { .. } => "title",
        PluginMessage::SetFace { .. } => "face",
        PluginMessage::SetState { .. } => "state",
        PluginMessage::SetImage { .. } => "image",
        PluginMessage::ShowOk { .. } => "ok",
        _ => "other",
    }
}

#[test]
fn the_handshake_answers_ready() {
    let deck = Harness::start(plugin());
    assert!(matches!(
        deck.ready,
        Some(PluginMessage::Ready { protocol: 1, .. })
    ));
    deck.finish().unwrap();
}

#[test]
fn updates_equal_to_the_last_one_are_not_sent_again() {
    let mut deck = Harness::start(plugin());
    deck.appear("k", Repeater::ID, json!({"text": "hi"}));
    deck.press("k");
    let mut first = Vec::new();
    while let Some(m) = deck.recv(QUIET) {
        if let PluginMessage::SetImage { format, .. } = &m.message {
            assert_eq!(*format, Some(ImageFormat::Png));
            assert_eq!(m.blob.as_deref(), Some(&[1u8, 2, 3][..]));
        }
        first.push(kind(&m.message));
    }
    assert_eq!(first, ["title", "face", "state", "image", "ok"]);
    // The same again: only the mark, which is an event, goes out.
    deck.press("k");
    let mut second = Vec::new();
    while let Some(m) = deck.recv(QUIET) {
        second.push(kind(&m.message));
    }
    assert_eq!(second, ["ok"]);
    // A key that disappears and comes back starts over.
    deck.disappear("k");
    deck.appear("k", Repeater::ID, json!({"text": "hi"}));
    deck.key_down("k");
    assert_eq!(deck.title("k", WAIT).as_deref(), Some("hi"));
    deck.finish().unwrap();
}

#[test]
fn keys_work_from_other_threads_until_they_disappear() {
    let mut deck = Harness::start(plugin());
    deck.appear("t1", Repeater::ID, json!({}));
    let key = loop {
        if let Some(k) = KEYS.lock().unwrap().iter().find(|k| k.context() == "t1") {
            break k.clone();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let worker = key.clone();
    std::thread::spawn(move || worker.set_title("from a thread"))
        .join()
        .unwrap();
    assert_eq!(deck.title("t1", WAIT).as_deref(), Some("from a thread"));
    deck.disappear("t1");
    while key.is_visible() {
        std::thread::sleep(Duration::from_millis(10));
    }
    key.set_title("too late");
    assert!(deck.recv(QUIET).is_none());
    deck.finish().unwrap();
}

#[test]
fn unknown_actions_and_messages_are_ignored() {
    let mut deck = Harness::start(plugin());
    deck.appear("x", "com.example.test.missing", json!({}));
    deck.press("x");
    deck.send(&deckly_sdk::protocol::HostMessage::Unknown);
    assert!(deck.recv(QUIET).is_none());
    deck.finish().unwrap();
}

#[test]
fn closing_input_ends_the_plugin() {
    let mut deck = Harness::start(plugin());
    deck.appear("k", Repeater::ID, json!({}));
    // finish() returns only once run_with() has returned.
    deck.finish().unwrap();
}

#[test]
fn input_that_is_not_deckly_is_refused() {
    let r = plugin().run_with(std::io::empty(), std::io::sink());
    assert!(r.is_err_and(|e| e.to_string().contains("--describe")));
    let garbage = std::io::Cursor::new(b"hello there\n".to_vec());
    assert!(plugin().run_with(garbage, std::io::sink()).is_err());
}

#[test]
fn the_manifest_describes_the_plugin() {
    let m = plugin().manifest();
    let text = serde_json::to_string(&m).unwrap();
    let back = Manifest::parse(text.as_bytes()).unwrap();
    assert_eq!(back.id, "com.example.test");
    assert_eq!(back.version, "1.2.3");
    assert_eq!(back.protocol, 1);
    assert_eq!(back.actions[0].id, Repeater::ID);
    assert_eq!(back.actions[0].states[1].title, "On");
    let target = deckly_sdk::protocol::current_target();
    assert!(back.program_for(&target).is_some(), "{:?}", back.programs);
}
