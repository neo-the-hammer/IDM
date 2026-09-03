//! An FTP and FTPS client, enough for downloading.
//!
//! FTP is still how a lot of mirrors — distribution archives, scientific data,
//! university servers — publish large files, and IDM has always supported it.
//! Only the download path is implemented: log in, ask for a size, and retrieve
//! a byte range.
//!
//! Resume and segmentation both work through `REST`, which sets the offset of
//! the next transfer. Each segment opens its own control and data connection,
//! which is what the specification intends and what every server supports.

use crate::auth::Credentials;
use crate::stream::{Stream, Timeouts};
use crate::url::Url;
use std::io::{self, BufRead, BufReader, Read, Write};

/// Caps on server-supplied data, to keep a hostile server bounded.
const MAX_REPLY_LINE: usize = 8 * 1024;
const MAX_REPLY_LINES: usize = 200;

pub struct FtpClient {
    control: BufReader<Stream>,
    /// Host of the control connection, reused for data connections.
    host: String,
    timeouts: Timeouts,
    #[cfg(unix)]
    tls: Option<std::sync::Arc<crate::tls::TlsContext>>,
    /// True once the data channel has been switched to TLS.
    protected: bool,
}

/// One reply from the server.
#[derive(Debug, Clone)]
pub struct Reply {
    pub code: u16,
    pub text: String,
}

impl Reply {
    fn is_positive(&self) -> bool {
        (200..400).contains(&self.code)
    }
}

impl FtpClient {
    /// Connects, negotiates TLS for `ftps://`, and logs in.
    ///
    /// Credentials come from the URL, then the explicit argument, and fall back
    /// to anonymous — which is what public mirrors expect.
    pub fn connect(
        url: &Url,
        credentials: Option<&Credentials>,
        timeouts: &Timeouts,
        #[cfg(unix)] tls: Option<std::sync::Arc<crate::tls::TlsContext>>,
        #[cfg(not(unix))] _tls: Option<()>,
    ) -> io::Result<FtpClient> {
        let tcp = Stream::connect_tcp(&url.host, url.effective_port(), timeouts)?;
        let mut control = BufReader::new(Stream::Plain(tcp));

        // Greeting.
        let greeting = read_reply_from(&mut control)?;
        if !greeting.is_positive() {
            return Err(io::Error::other(format!(
                "the FTP server refused the connection: {}",
                greeting.text
            )));
        }

        // The control connection is upgraded here, before the client exists,
        // so the plaintext stream can simply be consumed rather than swapped
        // out from under a half-built object.
        let mut protected = false;
        if url.scheme == "ftps" {
            #[cfg(unix)]
            {
                let ctx = tls
                    .clone()
                    .ok_or_else(|| io::Error::other("FTPS requested without a TLS context"))?;
                control = upgrade_to_tls(control, &url.host, &ctx)?;
                // Protect the data channel too, or the file itself travels in
                // clear even though the login did not (RFC 4217).
                command_on(&mut control, "PBSZ 0")?;
                protected = command_on(&mut control, "PROT P")?.is_positive();
            }
            #[cfg(not(unix))]
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "FTPS needs a TLS backend that this build does not have",
                ));
            }
        }

        let mut client = FtpClient {
            control,
            host: url.host.clone(),
            timeouts: *timeouts,
            #[cfg(unix)]
            tls,
            protected,
        };

        // Log in. Credentials come from the URL, then the explicit argument,
        // and fall back to anonymous, which is what public mirrors expect.
        let (user, pass) = if !url.username.is_empty() {
            (url.username.clone(), url.password.clone())
        } else if let Some(c) = credentials {
            (c.username.clone(), c.password.clone())
        } else {
            ("anonymous".to_string(), "hydra@example.invalid".to_string())
        };

        let reply = client.command(&format!("USER {user}"))?;
        // 331 asks for a password; 230 means we are already in.
        if reply.code == 331 {
            let reply = client.command(&format!("PASS {pass}"))?;
            if !reply.is_positive() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("FTP login failed: {}", reply.text),
                ));
            }
        } else if !reply.is_positive() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("FTP login failed: {}", reply.text),
            ));
        }

        // Binary mode. Anything else corrupts non-text files.
        client.command("TYPE I")?;
        Ok(client)
    }

    /// Sends a command and reads its reply.
    pub fn command(&mut self, line: &str) -> io::Result<Reply> {
        command_on(&mut self.control, line)
    }

    /// The size of `path` in bytes, via `SIZE`.
    pub fn size(&mut self, path: &str) -> io::Result<Option<u64>> {
        let reply = self.command(&format!("SIZE {path}"))?;
        if reply.code != 213 {
            // Plenty of servers do not implement SIZE; that only means we
            // cannot segment, not that the download fails.
            return Ok(None);
        }
        Ok(reply.text.trim().parse().ok())
    }

    /// The modification time of `path`, via `MDTM`, as `YYYYMMDDHHMMSS`.
    /// Used as a resume validator in place of an ETag.
    pub fn modified_time(&mut self, path: &str) -> io::Result<Option<String>> {
        let reply = self.command(&format!("MDTM {path}"))?;
        if reply.code != 213 {
            return Ok(None);
        }
        Ok(Some(reply.text.trim().to_string()))
    }

    /// Opens a data connection and starts retrieving `path` from `offset`.
    ///
    /// The returned reader yields file bytes until the transfer ends.
    pub fn retrieve(&mut self, path: &str, offset: u64) -> io::Result<FtpDownload> {
        let data_port = self.passive_port()?;
        let tcp = Stream::connect_tcp(&self.host, data_port, &self.timeouts)?;

        #[cfg(unix)]
        let data = if self.protected {
            let ctx = self
                .tls
                .clone()
                .ok_or_else(|| io::Error::other("protected data channel without TLS"))?;
            Stream::Tls(ctx.connect(&self.host, tcp)?)
        } else {
            Stream::Plain(tcp)
        };
        #[cfg(not(unix))]
        let data = Stream::Plain(tcp);

        // REST must come immediately before RETR.
        if offset > 0 {
            let reply = self.command(&format!("REST {offset}"))?;
            if reply.code != 350 {
                return Err(io::Error::other(format!(
                    "the server cannot resume from {offset}: {}",
                    reply.text
                )));
            }
        }

        let reply = self.command(&format!("RETR {path}"))?;
        // 125 "transfer starting" or 150 "opening data connection".
        if reply.code != 125 && reply.code != 150 {
            return Err(io::Error::other(format!(
                "the server refused RETR {path}: {}",
                reply.text
            )));
        }
        Ok(FtpDownload { data })
    }

    /// Enters passive mode, returning the port to connect to.
    ///
    /// `EPSV` is tried first because it works over IPv6 and returns only a
    /// port; `PASV` is the fallback for older servers.
    fn passive_port(&mut self) -> io::Result<u16> {
        let epsv = self.command("EPSV")?;
        if epsv.code == 229 {
            if let Some(port) = parse_epsv(&epsv.text) {
                return Ok(port);
            }
        }
        let pasv = self.command("PASV")?;
        if pasv.code != 227 {
            return Err(io::Error::other(format!(
                "the server refused passive mode: {}",
                pasv.text
            )));
        }
        parse_pasv(&pasv.text)
            .ok_or_else(|| io::Error::other(format!("cannot parse PASV reply: {}", pasv.text)))
    }

    /// Ends the session politely.
    pub fn quit(&mut self) {
        let _ = self.command("QUIT");
    }
}

/// An in-progress FTP transfer.
pub struct FtpDownload {
    data: Stream,
}

impl Read for FtpDownload {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.data.read(buf)
    }
}

impl FtpDownload {
    /// Aborts the transfer, unblocking a thread parked in `read`.
    pub fn shutdown(&self) {
        self.data.shutdown();
    }
}

/// Parses `229 Entering Extended Passive Mode (|||51234|)`.
fn parse_epsv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = text[open..].find(')')? + open;
    let inner = &text[open + 1..close];
    // The delimiter is whatever character appears first, repeated three times.
    let delimiter = inner.chars().next()?;
    inner
        .trim_matches(delimiter)
        .split(delimiter)
        .next_back()?
        .trim()
        .parse()
        .ok()
}

/// Parses `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)`.
///
/// Only the port is taken. The host in the reply is deliberately ignored in
/// favour of the control connection's host: honouring it would let a malicious
/// server redirect the data connection to a third party, which is the classic
/// FTP bounce attack.
fn parse_pasv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = text[open..].find(')')? + open;
    let numbers: Vec<u16> = text[open + 1..close]
        .split(',')
        .map(|n| n.trim().parse().ok())
        .collect::<Option<Vec<u16>>>()?;
    if numbers.len() != 6 {
        return None;
    }
    numbers[4].checked_mul(256)?.checked_add(numbers[5])
}

// --- control-channel primitives, usable before an FtpClient exists ---------

/// Sends a command and reads its reply.
fn command_on(control: &mut BufReader<Stream>, line: &str) -> io::Result<Reply> {
    if line.contains(['\r', '\n']) {
        // A newline in a path would let a crafted URL inject extra commands
        // into the control channel.
        return Err(io::Error::other("newline in an FTP command"));
    }
    let wire = format!("{line}\r\n");
    control.get_mut().write_all(wire.as_bytes())?;
    control.get_mut().flush()?;
    read_reply_from(control)
}

/// Reads a reply, joining a multi-line block into one `Reply`.
fn read_reply_from(control: &mut BufReader<Stream>) -> io::Result<Reply> {
    let first = read_line_from(control)?;
    if first.len() < 4 {
        return Err(io::Error::other(format!("malformed FTP reply: {first:?}")));
    }
    let code: u16 = first[..3]
        .parse()
        .map_err(|_| io::Error::other(format!("malformed FTP reply: {first:?}")))?;

    let mut text = first[4..].to_string();
    // "NNN-" opens a block that ends with a line starting "NNN ".
    if first.as_bytes()[3] == b'-' {
        let terminator = format!("{code} ");
        for _ in 0..MAX_REPLY_LINES {
            let line = read_line_from(control)?;
            if line.starts_with(&terminator) {
                text.push('\n');
                text.push_str(&line[4..]);
                return Ok(Reply { code, text });
            }
            text.push('\n');
            text.push_str(&line);
        }
        return Err(io::Error::other("FTP reply had too many lines"));
    }
    Ok(Reply { code, text })
}

fn read_line_from(control: &mut BufReader<Stream>) -> io::Result<String> {
    let mut raw = Vec::new();
    let mut limited = control.take(MAX_REPLY_LINE as u64);
    if limited.read_until(b'\n', &mut raw)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the FTP control connection closed",
        ));
    }
    while raw.last() == Some(&b'\n') || raw.last() == Some(&b'\r') {
        raw.pop();
    }
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// Replaces a plaintext control connection with a TLS one after `AUTH TLS`.
#[cfg(unix)]
fn upgrade_to_tls(
    mut control: BufReader<Stream>,
    host: &str,
    ctx: &crate::tls::TlsContext,
) -> io::Result<BufReader<Stream>> {
    let reply = command_on(&mut control, "AUTH TLS")?;
    if !reply.is_positive() {
        return Err(io::Error::other(format!(
            "the server refused AUTH TLS: {}",
            reply.text
        )));
    }
    // Unwrapping the BufReader discards anything it has buffered. The server
    // must not speak between the AUTH reply and the handshake, so buffered
    // bytes here mean either a broken server or an attempted injection.
    if !control.buffer().is_empty() {
        return Err(io::Error::other(
            "the server sent data between AUTH TLS and the handshake",
        ));
    }
    let Stream::Plain(tcp) = control.into_inner() else {
        return Err(io::Error::other("the control connection is already TLS"));
    };
    Ok(BufReader::new(Stream::Tls(ctx.connect(host, tcp)?)))
}

/// Exposes [`parse_epsv`] for tests without widening the public surface.
#[doc(hidden)]
pub fn parse_epsv_for_test(text: &str) -> Option<u16> {
    parse_epsv(text)
}

/// Exposes [`parse_pasv`] for tests without widening the public surface.
#[doc(hidden)]
pub fn parse_pasv_for_test(text: &str) -> Option<u16> {
    parse_pasv(text)
}
