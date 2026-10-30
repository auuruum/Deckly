//! One Deckly at a time. A closed window leaves Deckly running in the tray,
//! so starting the program again must bring that window back instead of
//! opening the devices a second time.
//!
//! The first instance holds a lock on a file in the settings folder; the
//! system releases it however the process ends. It also listens on a local
//! port, which a later start connects to. A connection carries no data and
//! only shows the window.

use std::fs::{self, File, TryLockError};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

const LOCK: &str = "instance.lock";
/// The lock is mandatory on Windows, so the port has its own file.
const PORT: &str = "instance.port";

pub struct Instance {
    lock: File,
    listener: TcpListener,
}

/// Returns `None` when another Deckly runs; it was asked to show its window.
pub fn claim(dir: &Path) -> io::Result<Option<Instance>> {
    fs::create_dir_all(dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            show_first(dir);
            return Ok(None);
        }
        Err(TryLockError::Error(e)) => return Err(e),
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    fs::write(dir.join(PORT), listener.local_addr()?.port().to_string())?;
    Ok(Some(Instance { lock, listener }))
}

/// A first instance still starting has no port yet and misses this; the user
/// then sees its window open anyway.
fn show_first(dir: &Path) {
    let port = fs::read_to_string(dir.join(PORT))
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok());
    if let Some(port) = port {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
    }
}

impl Instance {
    /// Calls `show` for each later start, for as long as the process runs.
    pub fn listen(self, show: impl Fn() + Send + 'static) {
        let run = move || {
            let _lock = self.lock;
            for stream in self.listener.incoming() {
                if stream.is_ok() {
                    show();
                }
            }
        };
        if let Err(e) = std::thread::Builder::new()
            .name("deckly-instance".into())
            .spawn(run)
        {
            eprintln!("deckly: cannot watch for later starts: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn folder(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("deckly-{name}-{}", std::process::id()))
    }

    #[test]
    fn a_second_start_shows_the_first() {
        let dir = folder("second-start");
        let first = claim(&dir).unwrap().unwrap();
        let (tx, rx) = channel();
        first.listen(move || {
            let _ = tx.send(());
        });
        assert!(claim(&dir).unwrap().is_none());
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_released_lock_can_be_claimed_again() {
        let dir = folder("released");
        drop(claim(&dir).unwrap().unwrap());
        assert!(claim(&dir).unwrap().is_some());
    }

    #[test]
    fn a_damaged_port_file_does_not_stop_the_second_start() {
        let dir = folder("damaged-port");
        let _first = claim(&dir).unwrap().unwrap();
        fs::write(dir.join(PORT), "not a port").unwrap();
        assert!(claim(&dir).unwrap().is_none());
    }
}
