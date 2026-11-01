//! Decode only visible rows on one worker; keep textures and misses in a
//! bounded cache for the lifetime of this picker. No disk I/O in a UI frame.

use crate::os::applications::{Application, icons::Loader};
use eframe::egui::{self, TextureHandle, TextureId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, SyncSender};

const CAPACITY: usize = 256;
const QUEUE: usize = 32;
type IconResult = (String, Option<image::RgbaImage>);

#[derive(Default)]
pub(super) struct Cache {
    requests: Option<SyncSender<String>>,
    results: Option<Receiver<IconResult>>,
    pending: HashSet<String>,
    textures: HashMap<String, Option<TextureHandle>>,
    order: VecDeque<String>,
}

impl Cache {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, rx) = mpsc::sync_channel::<String>(QUEUE);
        let (tx, results) = mpsc::sync_channel(QUEUE);
        let ctx = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("deckly-app-icons".into())
            .spawn(move || {
                let mut loader = Loader::new();
                while let Ok(key) = rx.recv() {
                    let image = loader.load(&key);
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
                    egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    ),
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

    pub fn texture(&mut self, app: &Application) -> Option<TextureId> {
        if app.icon.is_empty() {
            return None;
        }
        if let Some(texture) = self.textures.get(&app.icon) {
            return texture.as_ref().map(TextureHandle::id);
        }
        if !self.pending.contains(&app.icon)
            && let Some(requests) = &self.requests
            && requests.try_send(app.icon.clone()).is_ok()
        {
            self.pending.insert(app.icon.clone());
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
        let app = Application {
            name: "App".into(),
            target: "app".into(),
            detail: String::new(),
            keywords: String::new(),
            icon: "icon-1".into(),
        };
        assert_eq!(cache.texture(&app), None);
        assert!(requested.try_recv().is_err());
    }
}
