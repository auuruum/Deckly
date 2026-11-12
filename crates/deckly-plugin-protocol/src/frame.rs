//! Framing over a byte stream (the plugin's standard input and output).
//!
//! A frame is a 4-byte little-endian body length, a 1-byte kind and the
//! body. The length counts the body only. Kind 0 is a JSON message; kind 1 is
//! a binary blob that the next message refers to. Frames of other kinds are
//! skipped, so a later protocol can add kinds.

use crate::Validate;
use crate::limits::{MAX_BLOB_BYTES, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::io::{self, Read, Write};

/// A JSON message.
pub const KIND_MESSAGE: u8 = 0;
/// Binary data (image bytes) for the message that follows it.
pub const KIND_BLOB: u8 = 1;

const HEADER_BYTES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub body: Vec<u8>,
}

/// What can go wrong on the stream. Anything but `Io` means the other side
/// broke the protocol.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The stream ended inside a frame.
    Truncated,
    /// A frame longer than the limit for its kind.
    TooLarge {
        kind: u8,
        len: usize,
    },
    /// Bytes that look like text printed to standard output instead of a
    /// frame: a stray `println!`. Holds the start of that text.
    Text(String),
    /// A message that is not valid JSON or does not fit its type.
    Json(String),
    /// A message that breaks a limit or a rule of the protocol.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Truncated => write!(f, "the stream ended in the middle of a frame"),
            Error::TooLarge { kind, len } => {
                write!(f, "a frame of kind {kind} is too large ({len} bytes)")
            }
            Error::Text(text) => write!(
                f,
                "text was written to standard output, which belongs to the protocol \
                 (\"{text}\"); a plugin must write its log to standard error"
            ),
            Error::Json(e) => write!(f, "a message is not valid: {e}"),
            Error::Invalid(e) => write!(f, "a message breaks the protocol: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// A message with the blob sent right before it, if any.
#[derive(Debug, PartialEq)]
pub struct Received<T> {
    pub message: T,
    pub blob: Option<Vec<u8>>,
}

/// Reads frames from a byte stream.
pub struct Reader<R> {
    inner: R,
    blob: Option<Vec<u8>>,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, blob: None }
    }

    /// The next frame; `None` when the stream ended cleanly between frames.
    pub fn read_frame(&mut self) -> Result<Option<Frame>, Error> {
        let mut header = [0u8; HEADER_BYTES];
        match read_full(&mut self.inner, &mut header)? {
            0 => return Ok(None),
            HEADER_BYTES => {}
            n => {
                if let Some(text) = as_text(&header[..n]) {
                    return Err(Error::Text(text));
                }
                return Err(Error::Truncated);
            }
        }
        let [a, b, c, d, kind] = header;
        let len = u32::from_le_bytes([a, b, c, d]) as usize;
        let limit = match kind {
            KIND_MESSAGE => MAX_MESSAGE_BYTES,
            KIND_BLOB => MAX_BLOB_BYTES,
            _ => MAX_FRAME_BYTES,
        };
        if len > limit {
            // Printable text read as a length is always far above the limit.
            if as_text(&header).is_some() {
                let mut more = [0u8; 40];
                let n = read_full(&mut self.inner, &mut more).unwrap_or(0);
                let mut all = header.to_vec();
                all.extend_from_slice(&more[..n]);
                return Err(Error::Text(printable_prefix(&all)));
            }
            return Err(Error::TooLarge { kind, len });
        }
        let mut body = Vec::with_capacity(len);
        let got = (&mut self.inner).take(len as u64).read_to_end(&mut body)?;
        if got != len {
            return Err(Error::Truncated);
        }
        Ok(Some(Frame { kind, body }))
    }

    /// The next message, with the blob that came right before it. Frames of
    /// unknown kinds are skipped. Two blobs in a row are a protocol error;
    /// a blob followed by a message that does not use it is dropped by the
    /// caller.
    pub fn read<T: DeserializeOwned + Validate>(&mut self) -> Result<Option<Received<T>>, Error> {
        loop {
            let Some(frame) = self.read_frame()? else {
                if self.blob.is_some() {
                    return Err(Error::Truncated);
                }
                return Ok(None);
            };
            match frame.kind {
                KIND_MESSAGE => {
                    let message = decode::<T>(&frame.body)?;
                    return Ok(Some(Received {
                        message,
                        blob: self.blob.take(),
                    }));
                }
                KIND_BLOB if self.blob.is_some() => {
                    return Err(Error::Invalid("two blobs without a message".into()));
                }
                KIND_BLOB => self.blob = Some(frame.body),
                _ => {}
            }
        }
    }
}

/// Decodes and checks one message body.
pub fn decode<T: DeserializeOwned + Validate>(body: &[u8]) -> Result<T, Error> {
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(Error::TooLarge {
            kind: KIND_MESSAGE,
            len: body.len(),
        });
    }
    let message: T = serde_json::from_slice(body).map_err(|e| Error::Json(e.to_string()))?;
    message.validate().map_err(Error::Invalid)?;
    Ok(message)
}

/// Reads until `buf` is full or the stream ends; returns the bytes read.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn is_text_byte(b: u8) -> bool {
    b.is_ascii_graphic() || matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

fn as_text(bytes: &[u8]) -> Option<String> {
    (!bytes.is_empty() && bytes.iter().all(|&b| is_text_byte(b))).then(|| printable_prefix(bytes))
}

fn printable_prefix(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|&&b| is_text_byte(b))
        .take(40)
        .map(|&b| if b.is_ascii_graphic() { b as char } else { ' ' })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A whole message frame, ready to be written in one piece.
pub fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>, Error> {
    let mut out = vec![0u8; HEADER_BYTES];
    serde_json::to_writer(&mut out, message).map_err(|e| Error::Json(e.to_string()))?;
    let len = out.len() - HEADER_BYTES;
    if len > MAX_MESSAGE_BYTES {
        return Err(Error::TooLarge {
            kind: KIND_MESSAGE,
            len,
        });
    }
    set_header(&mut out, len, KIND_MESSAGE);
    Ok(out)
}

/// A whole blob frame.
pub fn encode_blob(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(Error::TooLarge {
            kind: KIND_BLOB,
            len: bytes.len(),
        });
    }
    let mut out = Vec::with_capacity(HEADER_BYTES + bytes.len());
    out.resize(HEADER_BYTES, 0);
    out.extend_from_slice(bytes);
    set_header(&mut out, bytes.len(), KIND_BLOB);
    Ok(out)
}

fn set_header(out: &mut [u8], len: usize, kind: u8) {
    // `len` is at most MAX_FRAME_BYTES, far below u32::MAX.
    let len = u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes();
    out[..4].copy_from_slice(&len);
    out[4] = kind;
}

/// Writes frames to a byte stream. Each frame goes out in one `write_all`
/// followed by a flush.
pub struct Writer<W> {
    inner: W,
}

impl<W: Write> Writer<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    pub fn write_message<T: Serialize>(&mut self, message: &T) -> Result<(), Error> {
        let frame = encode_message(message)?;
        self.write_encoded(&frame)
    }

    pub fn write_blob(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let frame = encode_blob(bytes)?;
        self.write_encoded(&frame)
    }

    /// Writes frames made by [`encode_message`] or [`encode_blob`].
    pub fn write_encoded(&mut self, frame: &[u8]) -> Result<(), Error> {
        self.inner.write_all(frame)?;
        self.inner.flush()?;
        Ok(())
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}
