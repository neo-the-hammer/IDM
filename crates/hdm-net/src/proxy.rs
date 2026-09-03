//! HTTP CONNECT and SOCKS5 proxies.

use crate::auth::Credentials;
use crate::stream::{Stream, Timeouts};
use crate::url::Url;
use hdm_crypto::base64_encode;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Socks5,
}

#[derive(Debug, Clone)]
pub struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub credentials: Option<Credentials>,
    /// Hosts that bypass the proxy, e.g. `localhost`, `*.internal`, `10.0.0.0/8`.
    pub bypass: Vec<String>,
}

impl Proxy {
    /// Parses `http://user:pass@host:port` or `socks5://host:port`.
    pub fn parse(spec: &str) -> Result<Proxy, String> {
        let spec = spec.trim();
        // Default to HTTP when no scheme is given, matching common convention.
        let with_scheme = if spec.contains("://") {
            spec.to_string()
        } else {
            format!("http://{spec}")
        };
        let url = Url::parse(&with_scheme).map_err(|e| e.to_string())?;
        let kind = match url.scheme.as_str() {
            "http" | "https" => ProxyKind::Http,
            "socks5" | "socks5h" | "socks" => ProxyKind::Socks5,
            other => return Err(format!("unsupported proxy scheme `{other}`")),
        };
        let port = url.port.unwrap_or(match kind {
            ProxyKind::Http => 8080,
            ProxyKind::Socks5 => 1080,
        });
        let credentials = (!url.username.is_empty()).then(|| Credentials {
            username: url.username.clone(),
            password: url.password.clone(),
        });
        Ok(Proxy {
            kind,
            host: url.host,
            port,
            credentials,
            bypass: Vec::new(),
        })
    }

    /// Whether `host` should bypass this proxy.
    pub fn bypasses(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.bypass.iter().any(|rule| {
            let rule = rule.trim().to_ascii_lowercase();
            match rule.as_str() {
                "" => false,
                "<local>" => !host.contains('.'),
                r if r.starts_with("*.") => host.ends_with(&r[1..]) || host == r[2..],
                r => host == r,
            }
        })
    }

    /// Opens a connection to `target` through this proxy.
    ///
    /// For plaintext HTTP over an HTTP proxy no tunnel is needed — the request
    /// simply carries an absolute URL — so this returns the proxy connection
    /// itself and the caller sets `absolute_target`.
    pub fn connect(
        &self,
        target: &Url,
        timeouts: &Timeouts,
        tls: Option<&crate::stream::TlsContextRef>,
    ) -> io::Result<(Stream, bool)> {
        let tcp = Stream::connect_tcp(&self.host, self.port, timeouts)?;
        match self.kind {
            ProxyKind::Http => {
                if target.is_tls() {
                    let tunnel = self.http_connect(tcp, target)?;
                    Ok((Stream::wrap(tunnel, target, tls)?, false))
                } else {
                    // Absolute-form request line; no tunnel.
                    Ok((Stream::Plain(tcp), true))
                }
            }
            ProxyKind::Socks5 => {
                let tunnel = self.socks5_connect(tcp, target)?;
                Ok((Stream::wrap(tunnel, target, tls)?, false))
            }
        }
    }

    /// Establishes an HTTP `CONNECT` tunnel.
    fn http_connect(&self, mut tcp: TcpStream, target: &Url) -> io::Result<TcpStream> {
        let authority = if target.is_ipv6 {
            format!("[{}]:{}", target.host, target.effective_port())
        } else {
            format!("{}:{}", target.host, target.effective_port())
        };
        let mut request = format!(
            "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\
             User-Agent: {}\r\nProxy-Connection: keep-alive\r\n",
            crate::http::USER_AGENT
        );
        if let Some(c) = &self.credentials {
            let raw = format!("{}:{}", c.username, c.password);
            request.push_str(&format!(
                "Proxy-Authorization: Basic {}\r\n",
                base64_encode(raw.as_bytes())
            ));
        }
        request.push_str("\r\n");
        tcp.write_all(request.as_bytes())?;
        tcp.flush()?;

        // Read the tunnel response. Borrow the socket so the BufReader cannot
        // swallow bytes belonging to the TLS handshake that follows.
        let mut reader = BufReader::new(&mut tcp);
        let mut status_line = String::new();
        reader.read_line(&mut status_line)?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                io::Error::other(format!("bad proxy response: {}", status_line.trim()))
            })?;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
                break;
            }
        }
        if status == 407 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the proxy requires authentication",
            ));
        }
        if !(200..300).contains(&status) {
            return Err(io::Error::other(format!(
                "the proxy refused CONNECT with status {status}"
            )));
        }
        Ok(tcp)
    }

    /// Performs the SOCKS5 handshake (RFC 1928) and connect request.
    fn socks5_connect(&self, mut tcp: TcpStream, target: &Url) -> io::Result<TcpStream> {
        // Greeting: offer "no auth" and, when we have credentials, username/password.
        let methods: &[u8] = if self.credentials.is_some() {
            &[0x00, 0x02]
        } else {
            &[0x00]
        };
        let mut greeting = vec![0x05, methods.len() as u8];
        greeting.extend_from_slice(methods);
        tcp.write_all(&greeting)?;
        tcp.flush()?;

        let mut reply = [0u8; 2];
        tcp.read_exact(&mut reply)?;
        if reply[0] != 0x05 {
            return Err(io::Error::other("the proxy is not SOCKS5"));
        }
        match reply[1] {
            0x00 => {}
            0x02 => {
                let c = self.credentials.as_ref().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "the SOCKS5 proxy requires a username and password",
                    )
                })?;
                // RFC 1929 sub-negotiation.
                if c.username.len() > 255 || c.password.len() > 255 {
                    return Err(io::Error::other("SOCKS5 credentials are too long"));
                }
                let mut auth = vec![0x01, c.username.len() as u8];
                auth.extend_from_slice(c.username.as_bytes());
                auth.push(c.password.len() as u8);
                auth.extend_from_slice(c.password.as_bytes());
                tcp.write_all(&auth)?;
                tcp.flush()?;
                let mut ok = [0u8; 2];
                tcp.read_exact(&mut ok)?;
                if ok[1] != 0x00 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "the SOCKS5 proxy rejected the credentials",
                    ));
                }
            }
            0xff => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the SOCKS5 proxy offered no acceptable authentication method",
                ))
            }
            other => {
                return Err(io::Error::other(format!(
                    "the SOCKS5 proxy chose unsupported method {other:#04x}"
                )))
            }
        }

        // CONNECT request. The hostname is sent as-is so the proxy resolves it,
        // which keeps DNS off the local network — the point of socks5h.
        let host = target.host.as_bytes();
        if host.len() > 255 {
            return Err(io::Error::other("hostname too long for SOCKS5"));
        }
        let mut request = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
        request.extend_from_slice(host);
        request.extend_from_slice(&target.effective_port().to_be_bytes());
        tcp.write_all(&request)?;
        tcp.flush()?;

        let mut head = [0u8; 4];
        tcp.read_exact(&mut head)?;
        if head[1] != 0x00 {
            let reason = match head[1] {
                0x01 => "general SOCKS server failure",
                0x02 => "connection not allowed by ruleset",
                0x03 => "network unreachable",
                0x04 => "host unreachable",
                0x05 => "connection refused",
                0x06 => "TTL expired",
                0x07 => "command not supported",
                0x08 => "address type not supported",
                _ => "unknown SOCKS5 error",
            };
            return Err(io::Error::other(format!("SOCKS5 connect failed: {reason}")));
        }
        // Consume the bound address, whose length depends on its type.
        match head[3] {
            0x01 => {
                let mut skip = [0u8; 4];
                tcp.read_exact(&mut skip)?;
            }
            0x03 => {
                let mut len = [0u8; 1];
                tcp.read_exact(&mut len)?;
                let mut skip = vec![0u8; len[0] as usize];
                tcp.read_exact(&mut skip)?;
            }
            0x04 => {
                let mut skip = [0u8; 16];
                tcp.read_exact(&mut skip)?;
            }
            other => {
                return Err(io::Error::other(format!(
                    "SOCKS5 returned unknown address type {other:#04x}"
                )))
            }
        }
        let mut port = [0u8; 2];
        tcp.read_exact(&mut port)?;
        Ok(tcp)
    }
}
