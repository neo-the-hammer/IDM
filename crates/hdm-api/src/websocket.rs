//! Just enough of RFC 6455 to push live progress to the UI.
//!
//! Only what a server needs: accept a handshake, send text frames, read close
//! and ping. Hydra never sends binary or fragmented frames, and the client
//! never sends anything but control frames, so the rest of the specification is
//! deliberately absent.

use hdm_crypto::{base64_encode, Digest, Sha1};
use std::io::{self, Read, Write};

/// The fixed GUID from RFC 6455 section 1.3.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A frame arriving from the client.
///
/// Anything a browser sends us is bounded by this, so a hostile client cannot
/// make the server allocate without limit.
const MAX_INCOMING_FRAME: usize = 64 * 1024;

const OP_TEXT: u8 = 0x1;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// Computes the `Sec-WebSocket-Accept` value for a client key.
pub fn accept_key(client_key: &str) -> String {
    base64_encode(&Sha1::digest(format!("{client_key}{GUID}").as_bytes()))
}

/// Writes a text frame.
///
/// Server frames are never masked, per the specification.
pub fn write_text(out: &mut impl Write, payload: &str) -> io::Result<()> {
    write_frame(out, OP_TEXT, payload.as_bytes())
}

pub fn write_close(out: &mut impl Write) -> io::Result<()> {
    // 1000 = normal closure.
    write_frame(out, OP_CLOSE, &1000u16.to_be_bytes())
}

fn write_pong(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    write_frame(out, OP_PONG, payload)
}

fn write_frame(out: &mut impl Write, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut header = Vec::with_capacity(10);
    // FIN set, no reserved bits, no fragmentation.
    header.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => header.push(n as u8),
        n if n <= u16::MAX as usize => {
            header.push(126);
            header.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            header.push(127);
            header.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.write_all(&header)?;
    out.write_all(payload)?;
    out.flush()
}

/// What a read produced.
pub enum Incoming {
    /// A text message from the client.
    Text(String),
    /// The client asked to close, or the connection ended.
    Closed,
    /// A control frame that was handled internally; keep reading.
    Handled,
}

/// Reads one frame, answering pings automatically.
pub fn read_frame(input: &mut impl Read, out: &mut impl Write) -> io::Result<Incoming> {
    let mut head = [0u8; 2];
    if let Err(e) = input.read_exact(&mut head) {
        return match e.kind() {
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset => Ok(Incoming::Closed),
            _ => Err(e),
        };
    }

    let opcode = head[0] & 0x0F;
    let masked = head[1] & 0x80 != 0;
    let length = match head[1] & 0x7F {
        126 => {
            let mut extended = [0u8; 2];
            input.read_exact(&mut extended)?;
            u16::from_be_bytes(extended) as usize
        }
        127 => {
            let mut extended = [0u8; 8];
            input.read_exact(&mut extended)?;
            let n = u64::from_be_bytes(extended);
            if n > MAX_INCOMING_FRAME as u64 {
                return Err(io::Error::other("websocket frame too large"));
            }
            n as usize
        }
        n => n as usize,
    };
    if length > MAX_INCOMING_FRAME {
        return Err(io::Error::other("websocket frame too large"));
    }
    // Every frame from a client must be masked (RFC 6455 section 5.1).
    if !masked {
        return Err(io::Error::other("unmasked frame from a client"));
    }

    let mut mask = [0u8; 4];
    input.read_exact(&mut mask)?;
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }

    match opcode {
        OP_TEXT => Ok(Incoming::Text(
            String::from_utf8_lossy(&payload).into_owned(),
        )),
        OP_CLOSE => Ok(Incoming::Closed),
        OP_PING => {
            write_pong(out, &payload)?;
            Ok(Incoming::Handled)
        }
        OP_PONG => Ok(Incoming::Handled),
        // Binary and continuation frames are not part of this protocol.
        _ => Ok(Incoming::Handled),
    }
}
