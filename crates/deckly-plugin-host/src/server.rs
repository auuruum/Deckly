//! The local WebSocket server plugins and property inspectors connect to.
//!
//! Each connection gets a reader thread (blocking reads, no polling) and a
//! writer thread fed by a bounded queue, so a plugin that stops reading can
//! never block Deckly: when its queue is full it is disconnected. Only
//! connections that register with a token handed out by [`Server::allow`]
//! are kept.

use crate::protocol::{MAX_MESSAGE_BYTES, parse_registration};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tungstenite::protocol::{Role, WebSocketConfig};
use tungstenite::{Message, WebSocket};

/// Messages waiting for a slow peer before it is dropped.
const QUEUE: usize = 1024;
#[cfg(not(test))]
const REGISTER_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(test)]
const REGISTER_TIMEOUT: Duration = Duration::from_millis(300);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS: usize = 256;

type Events = Arc<dyn Fn(ServerEvent) + Send + Sync>;

#[derive(Debug, PartialEq)]
pub enum ServerEvent {
    Registered { token: String, event: String },
    Message { token: String, text: String },
    Closed { token: String },
}

struct Peer {
    conn: u64,
    queue: SyncSender<String>,
    socket: TcpStream,
}

#[derive(Default)]
struct Inner {
    allowed: HashSet<String>,
    peers: HashMap<String, Peer>,
    connections: usize,
    next_conn: u64,
}

#[derive(Clone)]
pub struct Server {
    port: u16,
    inner: Arc<Mutex<Inner>>,
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|p| p.into_inner())
}

impl Server {
    /// Listens on a free port on the loopback address, IPv4 and, where the
    /// system has it, IPv6: plugin libraries often connect to `localhost`,
    /// which may resolve to either. `events` is called from connection
    /// threads.
    pub fn start(events: impl Fn(ServerEvent) + Send + Sync + 'static) -> io::Result<Self> {
        let events: Events = Arc::new(events);
        let v4 = TcpListener::bind(("127.0.0.1", 0))?;
        let port = v4.local_addr()?.port();
        let inner = Arc::new(Mutex::new(Inner::default()));
        let server = Self { port, inner };
        server.accept(v4, events.clone())?;
        // Without IPv6 (or with the port taken there) IPv4 alone serves.
        if let Ok(v6) = TcpListener::bind(("::1", port)) {
            server.accept(v6, events)?;
        }
        Ok(server)
    }

    fn accept(&self, listener: TcpListener, events: Events) -> io::Result<()> {
        let accept = self.clone();
        std::thread::Builder::new()
            .name("deckly-plugin-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let conn = {
                        let mut s = lock(&accept.inner);
                        if s.connections >= MAX_CONNECTIONS {
                            continue;
                        }
                        s.connections += 1;
                        s.next_conn += 1;
                        s.next_conn
                    };
                    let server = accept.clone();
                    let events = events.clone();
                    let spawned = std::thread::Builder::new()
                        .name("deckly-plugin-conn".into())
                        .spawn(move || {
                            server.serve(stream, conn, &*events);
                            lock(&server.inner).connections -= 1;
                        });
                    if spawned.is_err() {
                        lock(&accept.inner).connections -= 1;
                    }
                }
            })?;
        Ok(())
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Lets one connection register with this token.
    pub fn allow(&self, token: &str) {
        lock(&self.inner).allowed.insert(token.to_owned());
    }

    /// Forgets a token and closes its connection without a `Closed` event.
    pub fn forbid(&self, token: &str) {
        let mut s = lock(&self.inner);
        s.allowed.remove(token);
        if let Some(peer) = s.peers.remove(token) {
            let _ = peer.socket.shutdown(Shutdown::Both);
        }
    }

    /// Queues a message; returns false when the peer is not connected. A
    /// peer whose queue is full is disconnected.
    pub fn send(&self, token: &str, text: String) -> bool {
        let mut s = lock(&self.inner);
        let Some(peer) = s.peers.get(token) else {
            return false;
        };
        match peer.queue.try_send(text) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                if let Some(peer) = s.peers.remove(token) {
                    let _ = peer.socket.shutdown(Shutdown::Both);
                }
                false
            }
        }
    }

    fn serve(&self, stream: TcpStream, conn: u64, events: &(dyn Fn(ServerEvent) + Send + Sync)) {
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(REGISTER_TIMEOUT));
        let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
        let (Ok(read), Ok(write)) = (stream.try_clone(), stream.try_clone()) else {
            return;
        };
        let write = Arc::new(Mutex::new(write));
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let duplex = Duplex {
            read,
            write: write.clone(),
        };
        let Ok(mut ws) = tungstenite::accept_with_config(duplex, Some(config)) else {
            return;
        };
        // The first text message must register with a known token.
        let token = loop {
            match ws.read() {
                Ok(Message::Text(text)) => match parse_registration(text.as_str()) {
                    Some((event, token)) if lock(&self.inner).allowed.contains(&token) => {
                        break (event, token);
                    }
                    _ => return,
                },
                Ok(Message::Close(_)) | Err(_) => return,
                Ok(_) => {}
            }
        };
        let (event, token) = token;
        // Socket options belong to each handle on Windows: clear the
        // registration timeout on the handle the reader uses, or a plugin
        // that stays quiet that long is cut off.
        let _ = ws.get_ref().read.set_read_timeout(None);
        let Ok(reader_side) = stream.try_clone() else {
            return;
        };
        let (queue, rx) = sync_channel::<String>(QUEUE);
        let mut out = WebSocket::from_raw_socket(
            Duplex {
                read: reader_side,
                write,
            },
            Role::Server,
            Some(config),
        );
        let writer = std::thread::Builder::new()
            .name("deckly-plugin-write".into())
            .spawn(move || {
                while let Ok(text) = rx.recv() {
                    if out.send(Message::text(text)).is_err() {
                        break;
                    }
                }
                let _ = out.close(None);
                let _ = out.flush();
            });
        if writer.is_err() {
            return;
        }
        {
            let mut s = lock(&self.inner);
            let Ok(socket) = stream.try_clone() else {
                return;
            };
            if let Some(old) = s.peers.insert(
                token.clone(),
                Peer {
                    conn,
                    queue,
                    socket,
                },
            ) {
                let _ = old.socket.shutdown(Shutdown::Both);
            }
        }
        events(ServerEvent::Registered {
            token: token.clone(),
            event,
        });
        loop {
            match ws.read() {
                Ok(Message::Text(text)) => {
                    events(ServerEvent::Message {
                        token: token.clone(),
                        text: text.as_str().to_owned(),
                    });
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
        let mut s = lock(&self.inner);
        // A newer connection may have taken the token over.
        if s.peers.get(&token).is_some_and(|p| p.conn == conn) {
            s.peers.remove(&token);
            drop(s);
            events(ServerEvent::Closed { token });
        }
    }
}

/// One TCP connection seen by two WebSocket halves. Reads go to the reader's
/// own handle; every write call sends its whole buffer under one lock, so
/// frames from the reader (pongs) and the writer never interleave.
struct Duplex {
    read: TcpStream,
    write: Arc<Mutex<TcpStream>>,
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read.read(buf)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut w = self.write.lock().unwrap_or_else(|p| p.into_inner());
        w.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.write.lock().unwrap_or_else(|p| p.into_inner()).flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn client(port: u16) -> WebSocket<TcpStream> {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        tungstenite::client(format!("ws://127.0.0.1:{port}"), stream)
            .unwrap()
            .0
    }

    fn lock_tx<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap()
    }

    fn next(rx: &std::sync::mpsc::Receiver<ServerEvent>) -> ServerEvent {
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn registered_peers_exchange_messages() {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let server = Server::start(move |e| drop(lock_tx(&tx).send(e))).unwrap();
        server.allow("tok");
        let mut ws = client(server.port());
        ws.send(Message::text(r#"{"event":"registerPlugin","uuid":"tok"}"#))
            .unwrap();
        assert_eq!(
            next(&rx),
            ServerEvent::Registered {
                token: "tok".into(),
                event: "registerPlugin".into()
            }
        );
        ws.send(Message::text(r#"{"event":"logMessage"}"#)).unwrap();
        assert_eq!(
            next(&rx),
            ServerEvent::Message {
                token: "tok".into(),
                text: r#"{"event":"logMessage"}"#.into()
            }
        );
        assert!(server.send("tok", "hello".into()));
        match ws.read().unwrap() {
            Message::Text(t) => assert_eq!(t.as_str(), "hello"),
            other => panic!("{other:?}"),
        }
        assert!(!server.send("nobody", "x".into()));
        // A peer the host drops on purpose is not reported as closed.
        server.forbid("tok");
        assert!(!server.send("tok", "x".into()));
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        assert!(ws.read().is_err());
    }

    #[test]
    fn quiet_peers_stay_connected_after_registering() {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let server = Server::start(move |e| drop(lock_tx(&tx).send(e))).unwrap();
        server.allow("tok");
        let mut ws = client(server.port());
        ws.send(Message::text(r#"{"event":"registerPlugin","uuid":"tok"}"#))
            .unwrap();
        assert!(matches!(next(&rx), ServerEvent::Registered { .. }));
        // Longer than the registration timeout.
        assert!(rx.recv_timeout(REGISTER_TIMEOUT * 3).is_err());
        ws.send(Message::text("still here")).unwrap();
        assert_eq!(
            next(&rx),
            ServerEvent::Message {
                token: "tok".into(),
                text: "still here".into()
            }
        );
    }

    #[test]
    fn localhost_works_over_ipv6_too() {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let server = Server::start(move |e| drop(lock_tx(&tx).send(e))).unwrap();
        server.allow("tok6");
        let Ok(stream) = TcpStream::connect(("::1", server.port())) else {
            return; // no IPv6 on this machine
        };
        let (mut ws, _) =
            tungstenite::client(format!("ws://[::1]:{}", server.port()), stream).unwrap();
        ws.send(Message::text(r#"{"event":"registerPlugin","uuid":"tok6"}"#))
            .unwrap();
        assert!(matches!(next(&rx), ServerEvent::Registered { .. }));
    }

    #[test]
    fn unknown_tokens_are_dropped() {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let server = Server::start(move |e| drop(lock_tx(&tx).send(e))).unwrap();
        let mut ws = client(server.port());
        ws.send(Message::text(
            r#"{"event":"registerPlugin","uuid":"guess"}"#,
        ))
        .unwrap();
        // The server closes the connection without registering it.
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        let closed = (0..5).any(|_| ws.read().is_err());
        assert!(closed);
    }
}
