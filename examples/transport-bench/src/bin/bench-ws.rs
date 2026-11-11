//! The benchmark plugin as a native Stream Deck SDK plugin: WebSocket and
//! JSON text, with the same logic and the same threading as the SDK one (a
//! reader thread, and a writer that also runs the once-a-second timer).

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::TcpStream;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};
use transport_bench::now_micros;
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};

const PROBE: &str = "com.deckly.bench.ws.probe";
const TICK: &str = "com.deckly.bench.ws.tick";

enum Event {
    Probe(String),
    Appear(String),
    Disappear(String),
}

fn arg(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    args.get(at + 1).cloned()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("bench-ws: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let port = arg(&args, "-port").ok_or("no -port")?;
    let uuid = arg(&args, "-pluginUUID").ok_or("no -pluginUUID")?;
    let event = arg(&args, "-registerEvent").ok_or("no -registerEvent")?;
    let stream = TcpStream::connect(("127.0.0.1", port.parse::<u16>().map_err(|e| e.to_string())?))
        .map_err(|e| e.to_string())?;
    let writer_stream = stream.try_clone().map_err(|e| e.to_string())?;
    let (mut reader, _) = tungstenite::client::client(format!("ws://127.0.0.1:{port}"), stream)
        .map_err(|e| e.to_string())?;
    // Only this side writes after the handshake; the reader never does.
    let mut writer = WebSocket::from_raw_socket(writer_stream, Role::Client, None);
    let register = json!({"event": event, "uuid": uuid}).to_string();
    writer
        .send(Message::text(register))
        .map_err(|e| e.to_string())?;

    let (tx, rx) = channel();
    std::thread::spawn(move || {
        while let Ok(msg) = reader.read() {
            let Message::Text(text) = msg else { continue };
            let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
            let e = match (field("event").as_str(), field("action").as_str()) {
                ("keyDown", PROBE) => Event::Probe(field("context")),
                ("willAppear", TICK) => Event::Appear(field("context")),
                ("willDisappear", TICK) => Event::Disappear(field("context")),
                _ => continue,
            };
            if tx.send(e).is_err() {
                break;
            }
        }
        // Deckly closed the connection.
        std::process::exit(0);
    });

    let title = |context: &str, title: String| {
        json!({"event": "setTitle", "context": context, "payload": {"title": title}}).to_string()
    };
    // context -> (count, next tick)
    let mut ticks: BTreeMap<String, (u64, Instant)> = BTreeMap::new();
    loop {
        let next = ticks.values().map(|t| t.1).min();
        let event = match next {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(e) => Some(e),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            },
            None => match rx.recv() {
                Ok(e) => Some(e),
                Err(_) => return Ok(()),
            },
        };
        match event {
            Some(Event::Probe(context)) => {
                let text = title(&context, now_micros().to_string());
                writer.send(Message::text(text)).map_err(|e| e.to_string())?;
            }
            Some(Event::Appear(context)) => {
                ticks.insert(context, (0, Instant::now() + Duration::from_secs(1)));
            }
            Some(Event::Disappear(context)) => {
                ticks.remove(&context);
            }
            None => {
                let now = Instant::now();
                for (context, (count, next)) in &mut ticks {
                    if *next > now {
                        continue;
                    }
                    *count += 1;
                    *next = (*next + Duration::from_secs(1)).max(now);
                    let text = title(context, count.to_string());
                    writer.send(Message::text(text)).map_err(|e| e.to_string())?;
                }
            }
        }
    }
}
