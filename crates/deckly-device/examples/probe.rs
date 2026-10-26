//! Lists Elgato HID interfaces, dumps their feature reports, then prints raw
//! input reports for 15 seconds so key presses can be observed.
//!
//! cargo run -p deckly-device --example probe

use async_hid::{AsyncHidFeatureHandle, AsyncHidRead, HidBackend};
use deckly_core::ELGATO_VENDOR_ID;
use futures_lite::StreamExt;
use futures_lite::future::block_on;
use std::time::Duration;

fn main() {
    block_on(async {
        let backend = HidBackend::default();
        let devices: Vec<_> = match backend.enumerate().await {
            Ok(s) => {
                s.filter(|d| d.vendor_id == ELGATO_VENDOR_ID)
                    .collect()
                    .await
            }
            Err(e) => {
                eprintln!("enumerate failed: {e}");
                return;
            }
        };
        for d in &devices {
            println!(
                "{:04x}:{:04x} usage {:04x}/{:04x} name={:?} serial={:?} id={:?}",
                d.vendor_id, d.product_id, d.usage_page, d.usage_id, d.name, d.serial_number, d.id
            );
            match d.open_feature_handle().await {
                Ok(mut h) => {
                    // Main protocol ids, then the Mini firmware ids.
                    for id in [
                        0x03u8, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0xA0, 0xA1, 0xA2,
                    ] {
                        let mut buf = [0u8; 32];
                        buf[0] = id;
                        match h.read_feature_report(&mut buf).await {
                            Ok(n) => println!("  feature {id:#04x} ({n}): {:02x?}", &buf[..n]),
                            Err(e) => println!("  feature {id:#04x}: {e}"),
                        }
                    }
                }
                Err(e) => println!("  feature handle: {e}"),
            }
        }
        let Some(d) = devices.into_iter().next() else {
            println!("no Elgato device found");
            return;
        };
        let mut reader = match d.open_readable().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("open failed: {e}");
                return;
            }
        };
        println!("reading input reports for 15 s, press some keys...");
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(15));
            std::process::exit(0);
        });
        let mut buf = vec![0u8; 1024];
        loop {
            match reader.read_input_report(&mut buf).await {
                Ok(n) => {
                    let end = n.min(32);
                    println!("input ({n}): {:02x?}", &buf[..end]);
                }
                Err(e) => {
                    eprintln!("read failed: {e}");
                    return;
                }
            }
        }
    });
}
