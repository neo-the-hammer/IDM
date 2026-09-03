//! Socket setup and a single `Read + Write` type over plain and TLS connections.

use crate::url::Url;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Timeouts applied to every connection.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    pub read: Duration,
    pub write: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(30),
            // Generous, because a heavily throttled or congested transfer can
            // legitimately stall for a while before the next TCP segment lands.
            read: Duration::from_secs(60),
            write: Duration::from_secs(60),
        }
    }
}

/// Either a plain TCP connection or a TLS session over one.
pub enum Stream {
    Plain(TcpStream),
    #[cfg(unix)]
    Tls(crate::tls::TlsStream),
}

impl Stream {
    /// Opens a TCP connection, trying every resolved address in turn.
    ///
    /// Hosts routinely resolve to both an AAAA and an A record, and on a
    /// network with broken IPv6 the first will hang; falling through to the
    /// next address is what makes those networks usable.
    pub fn connect_tcp(host: &str, port: u16, timeouts: &Timeouts) -> io::Result<TcpStream> {
        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("cannot resolve `{host}`: {e}"),
                )
            })?
            .collect();
        if addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("`{host}` resolved to no addresses"),
            ));
        }

        let mut last: Option<io::Error> = None;
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, timeouts.connect) {
                Ok(tcp) => {
                    tcp.set_read_timeout(Some(timeouts.read))?;
                    tcp.set_write_timeout(Some(timeouts.write))?;
                    // Downloads are throughput-bound and we write whole
                    // requests at once, so Nagle only adds latency.
                    let _ = tcp.set_nodelay(true);
                    return Ok(tcp);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other(format!("cannot connect to {host}:{port}"))))
    }

    /// Connects and, for `https`/`ftps`, completes the TLS handshake.
    pub fn connect(
        url: &Url,
        timeouts: &Timeouts,
        tls: Option<&TlsContextRef>,
    ) -> io::Result<Stream> {
        let tcp = Stream::connect_tcp(&url.host, url.effective_port(), timeouts)?;
        Stream::wrap(tcp, url, tls)
    }

    /// Wraps an already-connected socket, negotiating TLS if the URL needs it.
    /// Used after a proxy `CONNECT` has been established.
    pub fn wrap(tcp: TcpStream, url: &Url, tls: Option<&TlsContextRef>) -> io::Result<Stream> {
        if !url.is_tls() {
            return Ok(Stream::Plain(tcp));
        }
        #[cfg(unix)]
        {
            let ctx = tls
                .ok_or_else(|| io::Error::other("a TLS URL was requested without a TLS context"))?;
            Ok(Stream::Tls(ctx.connect(&url.host, tcp)?))
        }
        #[cfg(not(unix))]
        {
            let _ = tls;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this build has no TLS backend; use the WinHTTP transport on Windows",
            ))
        }
    }

    /// A short description of the security of this connection, for the UI.
    pub fn security(&self) -> String {
        match self {
            Stream::Plain(_) => "plaintext".into(),
            #[cfg(unix)]
            Stream::Tls(s) => s.protocol_version(),
        }
    }

    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(t) => t,
            #[cfg(unix)]
            Stream::Tls(s) => s.tcp(),
        }
    }

    /// Adjusts the read timeout on an established connection. The engine
    /// shortens this while probing and lengthens it during a slow transfer.
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.tcp().set_read_timeout(d)
    }

    pub fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.tcp().peer_addr()
    }

    /// Stops the connection immediately, unblocking a thread parked in `read`.
    /// This is how a paused or cancelled download is torn down promptly.
    pub fn shutdown(&self) {
        let _ = self.tcp().shutdown(std::net::Shutdown::Both);
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(t) => t.read(buf),
            #[cfg(unix)]
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(t) => t.write(buf),
            #[cfg(unix)]
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(t) => t.flush(),
            #[cfg(unix)]
            Stream::Tls(s) => s.flush(),
        }
    }
}

#[cfg(unix)]
pub type TlsContextRef = crate::tls::TlsContext;

/// On platforms with no OpenSSL backend this is an uninhabited placeholder, so
/// the signatures above stay identical across targets.
#[cfg(not(unix))]
pub enum TlsContextRef {}

#[cfg(not(unix))]
impl TlsContextRef {
    pub fn connect(&self, _host: &str, _tcp: TcpStream) -> io::Result<Stream> {
        match *self {}
    }
}
