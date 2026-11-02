//! Decode only visible rows on one worker; keep textures and misses in a
//! bounded cache for the lifetime of this picker. No disk I/O in a UI frame.

use crate::os::applications::icons::Loader;
use crate::prefs::shrink_to;
use eframe::egui::{self, ColorImage, TextureHandle};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, SyncSender};

const CAPACITY: usize = 256;
const QUEUE: usize = 32;
type IconResult = (String, Option<ColorImage>);

#[derive(Default)]
pub struct Cache {
    requests: Option<SyncSender<String>>,
    results: Option<Receiver<IconResult>>,
    pending: HashSet<String>,
    textures: HashMap<String, Option<TextureHandle>>,
    order: VecDeque<String>,
}

impl Cache {
    /// For icons drawn `size` points wide.
    pub fn new(ctx: &egui::Context, size: f32) -> Self {
        let (requests, rx) = mpsc::sync_channel::<String>(QUEUE);
        let (tx, results) = mpsc::sync_channel(QUEUE);
        let target = (size * ctx.pixels_per_point()).round().max(1.0) as u32;
        let ctx = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("deckly-app-icons".into())
            .spawn(move || {
                let mut loader = Loader::new();
                while let Ok(key) = rx.recv() {
                    let image = loader.load(&key).map(|image| shrink_to(image, target));
                    if tx.send((key, image)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        if worker.is_err() {
            return Self::default();
        }
        Self {
            requests: Some(requests),
            results: Some(results),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn receive(&mut self, ctx: &egui::Context) {
        let Some(results) = &self.results else { return };
        for (key, image) in results.try_iter().take(QUEUE) {
            self.pending.remove(&key);
            let texture = image.map(|image| {
                ctx.load_texture(
                    format!("app-icon:{key}"),
                    image,
                    egui::TextureOptions::LINEAR,
                )
            });
            if self.textures.len() >= CAPACITY
                && let Some(oldest) = self.order.pop_front()
            {
                self.textures.remove(&oldest);
            }
            self.order.push_back(key.clone());
            self.textures.insert(key, texture);
        }
    }

    pub fn texture(&mut self, icon: &str) -> Option<&TextureHandle> {
        if icon.is_empty() {
            return None;
        }
        if self.textures.contains_key(icon) {
            return self.textures.get(icon)?.as_ref();
        }
        if !self.pending.contains(icon)
            && let Some(requests) = &self.requests
            && requests.try_send(icon.to_owned()).is_ok()
        {
            self.pending.insert(icon.to_owned());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_failures_are_not_requested_again_and_cache_is_bounded() {
        let ctx = egui::Context::default();
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let (requests, requested) = mpsc::sync_channel(QUEUE);
        let mut cache = Cache {
            requests: Some(requests),
            results: Some(rx),
            ..Cache::default()
        };
        for i in 0..=CAPACITY {
            tx.send((format!("icon-{i}"), None)).unwrap();
            cache.receive(&ctx);
        }
        assert_eq!(cache.textures.len(), CAPACITY);
        assert!(!cache.textures.contains_key("icon-0"));
        assert!(cache.texture("icon-1").is_none());
        assert!(requested.try_recv().is_err());
    }
}
