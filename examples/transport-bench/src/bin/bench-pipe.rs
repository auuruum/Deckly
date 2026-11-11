//! The benchmark plugin written with the Deckly SDK (pipes).

use deckly_sdk::prelude::*;
use std::time::Duration;
use transport_bench::{PIPE_PLUGIN, now_micros};

#[derive(Default)]
struct Probe;

impl Action for Probe {
    const ID: &'static str = "com.deckly.bench.pipe.probe";
    const NAME: &'static str = "Probe";
    type Settings = serde_json::Value;

    fn key_down(&mut self, key: &Key<Self>) {
        key.set_title(now_micros().to_string());
    }
}

#[derive(Default)]
struct Tick {
    count: u64,
}

impl Action for Tick {
    const ID: &'static str = "com.deckly.bench.pipe.tick";
    const NAME: &'static str = "Tick";
    type Settings = serde_json::Value;

    fn key_appear(&mut self, key: &Key<Self>) {
        key.every(Duration::from_secs(1));
    }

    fn tick(&mut self, key: &Key<Self>) {
        self.count += 1;
        key.set_title(self.count.to_string());
    }
}

fn main() {
    let plugin = Plugin::new(PIPE_PLUGIN, "Bench pipe")
        .action::<Probe>()
        .action::<Tick>();
    if let Err(e) = plugin.run() {
        eprintln!("bench-pipe: {e}");
        std::process::exit(1);
    }
}
