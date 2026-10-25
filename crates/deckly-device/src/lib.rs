//! Stream Deck hardware access.
//!
//! [`DeviceManager`] finds supported devices, follows hot-plug events and
//! runs one reader and one writer thread per connected device. Key changes
//! arrive as [`Event`]s through the sink passed to [`DeviceManager::start`];
//! images and brightness are sent through the manager. A disconnect is an
//! ordinary event, never an error that stops the manager.

pub mod protocol;

use async_hid::{AsyncHidFeatureHandle, AsyncHidRead, AsyncHidWrite, Device, DeviceId, HidBackend};
use deckly_core::{DeviceModel, ELGATO_VENDOR_ID};
use futures_lite::StreamExt;
use futures_lite::future::block_on;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Receives device events; called from the device threads.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Identifies one connection of one device for as long as it stays plugged in.
pub type Handle = u64;

#[derive(Clone, Debug)]
pub struct ConnectedDevice {
    pub handle: Handle,
    pub model: &'static DeviceModel,
    pub serial: String,
    pub firmware: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Event {
    Connected(ConnectedDevice),
    Disconnected(Handle),
    Key {
        handle: Handle,
        key: u8,
        pressed: bool,
    },
}

enum Command {
    Image(u8, Vec<u8>),
    Brightness(u8),
    Reset,
}

type Writers = Arc<Mutex<HashMap<Handle, Sender<Command>>>>;

#[derive(Clone)]
pub struct DeviceManager {
    writers: Writers,
}

impl DeviceManager {
    pub fn start(events: EventSink) -> Self {
        let writers: Writers = Arc::default();
        let manager = Self {
            writers: writers.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("deckly-devices".into())
            .spawn(move || watch_devices(events, writers));
        if let Err(e) = spawned {
            eprintln!("deckly: cannot start device thread: {e}");
        }
        manager
    }

    /// Queues a JPEG image for a key. Images still queued for the same key
    /// are replaced, so a burst of updates sends only the latest one.
    pub fn set_image(&self, handle: Handle, key: u8, jpeg: Vec<u8>) {
        self.send(handle, Command::Image(key, jpeg));
    }

    pub fn set_brightness(&self, handle: Handle, percent: u8) {
        self.send(handle, Command::Brightness(percent));
    }

    /// Returns the device to its power-on state (its own logo screen).
    pub fn reset(&self, handle: Handle) {
        self.send(handle, Command::Reset);
    }

    fn send(&self, handle: Handle, command: Command) {
        if let Some(tx) = lock(&self.writers).get(&handle) {
            let _ = tx.send(command);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn watch_devices(events: EventSink, writers: Writers) {
    let backend = HidBackend::default();
    let open: Arc<Mutex<HashMap<DeviceId, Handle>>> = Arc::default();
    let next_handle = AtomicU64::new(1);
    let scan = || {
        block_on(scan_devices(
            &backend,
            &events,
            &writers,
            &open,
            &next_handle,
        ));
    };
    scan();
    match backend.watch() {
        Ok(mut changes) => {
            while block_on(changes.next()).is_some() {
                scan();
            }
        }
        // Without hot-plug notifications fall back to a slow rescan.
        Err(e) => {
            eprintln!("deckly: hot-plug watch unavailable ({e}), rescanning every 2 s");
            loop {
                std::thread::sleep(Duration::from_secs(2));
                scan();
            }
        }
    }
}

async fn scan_devices(
    backend: &HidBackend,
    events: &EventSink,
    writers: &Writers,
    open: &Arc<Mutex<HashMap<DeviceId, Handle>>>,
    next_handle: &AtomicU64,
) {
    let devices: Vec<Device> = match backend.enumerate().await {
        Ok(stream) => {
            stream
                .filter(|d| d.vendor_id == ELGATO_VENDOR_ID)
                .collect()
                .await
        }
        Err(e) => {
            eprintln!("deckly: HID enumeration failed: {e}");
            return;
        }
    };
    for device in devices {
        let Some(model) = DeviceModel::from_product_id(device.product_id) else {
            continue;
        };
        if lock(open).contains_key(&device.id) {
            continue;
        }
        match open_device(&device, model, next_handle).await {
            Ok((info, reader, writer, feature)) => {
                let handle = info.handle;
                lock(open).insert(device.id.clone(), handle);
                let (tx, rx) = channel();
                lock(writers).insert(handle, tx);
                spawn_writer(handle, writer, feature, rx);
                events(Event::Connected(info));
                spawn_reader(
                    handle,
                    model.key_count(),
                    reader,
                    events.clone(),
                    writers.clone(),
                    open.clone(),
                    device.id.clone(),
                );
            }
            Err(e) => eprintln!("deckly: cannot open {}: {e}", model.name),
        }
    }
}

async fn open_device(
    device: &Device,
    model: &'static DeviceModel,
    next_handle: &AtomicU64,
) -> async_hid::HidResult<(
    ConnectedDevice,
    async_hid::DeviceReader,
    async_hid::DeviceWriter,
    async_hid::DeviceFeatureHandle,
)> {
    let mut feature = device.open_feature_handle().await?;
    let serial = read_feature(&mut feature, protocol::REPORT_SERIAL)
        .await
        .and_then(|r| protocol::parse_serial(&r))
        .or_else(|| device.serial_number.clone())
        .unwrap_or_default();
    let firmware = read_feature(&mut feature, protocol::REPORT_FIRMWARE)
        .await
        .and_then(|r| protocol::parse_firmware(&r));
    let (reader, writer) = device.open().await?;
    let info = ConnectedDevice {
        handle: next_handle.fetch_add(1, Ordering::Relaxed),
        model,
        serial,
        firmware,
    };
    Ok((info, reader, writer, feature))
}

async fn read_feature(
    feature: &mut async_hid::DeviceFeatureHandle,
    id: u8,
) -> Option<[u8; protocol::FEATURE_REPORT_LEN]> {
    let mut buf = [0u8; protocol::FEATURE_REPORT_LEN];
    buf[0] = id;
    feature
        .read_feature_report(&mut buf)
        .await
        .ok()
        .map(|_| buf)
}

fn spawn_reader(
    handle: Handle,
    key_count: u8,
    mut reader: async_hid::DeviceReader,
    events: EventSink,
    writers: Writers,
    open: Arc<Mutex<HashMap<DeviceId, Handle>>>,
    id: DeviceId,
) {
    let run = move || {
        let mut buf = [0u8; protocol::INPUT_REPORT_LEN];
        let mut pressed = vec![false; usize::from(key_count)];
        loop {
            let n = match block_on(reader.read_input_report(&mut buf)) {
                Ok(n) => n,
                Err(_) => break,
            };
            let Some(states) = buf
                .get(..n)
                .and_then(|r| protocol::parse_keys(r, key_count))
            else {
                continue;
            };
            for (key, (&now, was)) in states.iter().zip(pressed.iter_mut()).enumerate() {
                if now != *was {
                    *was = now;
                    // key < key_count, which is a u8.
                    let key = key as u8;
                    events(Event::Key {
                        handle,
                        key,
                        pressed: now,
                    });
                }
            }
        }
        // Dropping the sender ends the writer thread.
        lock(&writers).remove(&handle);
        lock(&open).remove(&id);
        events(Event::Disconnected(handle));
    };
    if let Err(e) = std::thread::Builder::new()
        .name(format!("deckly-read-{handle}"))
        .spawn(run)
    {
        eprintln!("deckly: cannot start reader thread: {e}");
    }
}

fn spawn_writer(
    handle: Handle,
    mut writer: async_hid::DeviceWriter,
    mut feature: async_hid::DeviceFeatureHandle,
    commands: Receiver<Command>,
) {
    let run = move || {
        while let Ok(first) = commands.recv() {
            // Coalesce everything already queued: keep the newest image per key.
            let mut images: Vec<(u8, Vec<u8>)> = Vec::new();
            let mut others = Vec::new();
            for command in std::iter::once(first).chain(commands.try_iter()) {
                match command {
                    Command::Image(key, jpeg) => {
                        images.retain(|(k, _)| *k != key);
                        images.push((key, jpeg));
                    }
                    Command::Reset => {
                        images.clear();
                        others.push(Command::Reset);
                    }
                    other => others.push(other),
                }
            }
            for command in others {
                let report = match command {
                    Command::Brightness(p) => protocol::brightness_report(p),
                    Command::Reset => protocol::reset_report(),
                    Command::Image(..) => continue,
                };
                if let Err(e) = block_on(feature.write_feature_report(&report)) {
                    eprintln!("deckly: feature write failed: {e}");
                }
            }
            for (key, jpeg) in images {
                let reports = match protocol::image_reports(key, &jpeg) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("deckly: {e}");
                        continue;
                    }
                };
                for report in reports {
                    if block_on(writer.write_output_report(&report)).is_err() {
                        // The reader notices the disconnect and reports it.
                        return;
                    }
                }
            }
        }
    };
    if let Err(e) = std::thread::Builder::new()
        .name(format!("deckly-write-{handle}"))
        .spawn(run)
    {
        eprintln!("deckly: cannot start writer thread: {e}");
    }
}
