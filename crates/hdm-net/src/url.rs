//! URL parsing, percent-coding and relative-reference resolution (RFC 3986).
//!
//! Redirect chains and the site grabber both hand us relative references, so
//! `join` is as important here as parsing.

use crate::punycode;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// Lowercase, e.g. `https`.
    pub scheme: String,
    pub username: String,
    pub password: String,
    /// Lowercase and IDN-encoded. IPv6 literals are stored without brackets.
    pub host: String,
    pub is_ipv6: bool,
    /// `None` means "the scheme's default".
    pub port: Option<u16>,
    /// Always begins with `/`.
    pub path: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlError(pub String);

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid URL: {}", self.0)
    }
}
impl std::error::Error for UrlError {}

impl Url {
    pub fn parse(input: &str) -> Result<Url, UrlError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(UrlError("empty".into()));
        }

        // Scheme must be ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":".
        let colon = input
            .find(':')
            .ok_or_else(|| UrlError("missing scheme".into()))?;
        let scheme = &input[..colon];
        if scheme.is_empty()
            || !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return Err(UrlError(format!("bad scheme `{scheme}`")));
        }
        let scheme = scheme.to_ascii_lowercase();

        let rest = &input[colon + 1..];
        let rest = rest
            .strip_prefix("//")
            .ok_or_else(|| UrlError("expected `//` after the scheme".into()))?;

        // Split off fragment, then query, before touching the authority so that
        // a `?` or `#` inside them cannot be mistaken for a path separator.
        let (rest, fragment) = match rest.find('#') {
            Some(i) => (&rest[..i], Some(rest[i + 1..].to_string())),
            None => (rest, None),
        };
        let (rest, query) = match rest.find('?') {
            Some(i) => (&rest[..i], Some(rest[i + 1..].to_string())),
            None => (rest, None),
        };

        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].to_string()),
            None => (rest, "/".to_string()),
        };

        // userinfo is delimited by the LAST '@', since a password may contain one.
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (&authority[..i], &authority[i + 1..]),
            None => ("", authority),
        };
        let (username, password) = match userinfo.find(':') {
            Some(i) => (
                percent_decode_str(&userinfo[..i]),
                percent_decode_str(&userinfo[i + 1..]),
            ),
            None => (percent_decode_str(userinfo), String::new()),
        };

        let (host_raw, port_str, is_ipv6) = if let Some(stripped) = hostport.strip_prefix('[') {
            // IPv6 literal: [::1]:8080
            let close = stripped
                .find(']')
                .ok_or_else(|| UrlError("unterminated IPv6 literal".into()))?;
            let host = &stripped[..close];
            let after = &stripped[close + 1..];
            let port = after.strip_prefix(':').unwrap_or("");
            if !after.is_empty() && !after.starts_with(':') {
                return Err(UrlError("junk after IPv6 literal".into()));
            }
            (host.to_string(), port.to_string(), true)
        } else {
            match hostport.rfind(':') {
                Some(i) => (
                    hostport[..i].to_string(),
                    hostport[i + 1..].to_string(),
                    false,
                ),
                None => (hostport.to_string(), String::new(), false),
            }
        };

        if host_raw.is_empty() {
            return Err(UrlError("missing host".into()));
        }
        let host = if is_ipv6 {
            host_raw.to_ascii_lowercase()
        } else {
            let decoded = percent_decode_str(&host_raw);
            punycode::host_to_ascii(&decoded)
                .ok_or_else(|| UrlError(format!("cannot encode host `{host_raw}`")))?
        };
        if !is_ipv6 && host.contains(|c: char| c.is_whitespace() || c == '/' || c == '@') {
            return Err(UrlError(format!("bad host `{host}`")));
        }

        let port = if port_str.is_empty() {
            None
        } else {
            Some(
                port_str
                    .parse::<u16>()
                    .map_err(|_| UrlError(format!("bad port `{port_str}`")))?,
            )
        };

        Ok(Url {
            scheme,
            username,
            password,
            host,
            is_ipv6,
            port,
            path: normalize_path(&path),
            query,
            fragment,
        })
    }

    /// The port to actually connect to.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.scheme.as_str() {
            "https" | "ftps" => 443,
            "ftp" => 21,
            _ => 80,
        })
    }

    pub fn is_tls(&self) -> bool {
        matches!(self.scheme.as_str(), "https" | "ftps")
    }

    /// The value for the `Host` header: the port is omitted when it is the
    /// scheme default, and IPv6 literals are re-bracketed.
    pub fn host_header(&self) -> String {
        let host = if self.is_ipv6 {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default = match self.scheme.as_str() {
            "https" => 443,
            _ => 80,
        };
        match self.port {
            Some(p) if p != default => format!("{host}:{p}"),
            _ => host,
        }
    }

    /// Path plus query, which is what goes on the HTTP request line.
    pub fn request_target(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{}", self.path, q),
            None => self.path.clone(),
        }
    }

    /// The last path segment, percent-decoded — the starting guess for a
    /// download's filename before `Content-Disposition` is consulted.
    pub fn filename(&self) -> Option<String> {
        let last = self.path.rsplit('/').next()?;
        if last.is_empty() {
            return None;
        }
        let decoded = percent_decode_str(last);
        if decoded.is_empty() {
            None
        } else {
            Some(decoded)
        }
    }

    /// Resolves a possibly-relative reference against this URL (RFC 3986 §5.3).
    ///
    /// `Location` headers are frequently relative, and the site grabber walks
    /// relative `href`s, so this is on the hot path for both.
    pub fn join(&self, reference: &str) -> Result<Url, UrlError> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Ok(self.clone());
        }
        // An absolute URL replaces everything.
        if let Ok(abs) = Url::parse(reference) {
            return Ok(abs);
        }
        // Protocol-relative: //host/path
        if let Some(rest) = reference.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, rest));
        }

        let mut out = self.clone();
        out.fragment = None;

        let (reference, fragment) = match reference.find('#') {
            Some(i) => (&reference[..i], Some(reference[i + 1..].to_string())),
            None => (reference, None),
        };
        out.fragment = fragment;

        if reference.is_empty() {
            return Ok(out);
        }
        if let Some(q) = reference.strip_prefix('?') {
            out.query = Some(q.to_string());
            return Ok(out);
        }

        let (path_part, query) = match reference.find('?') {
            Some(i) => (&reference[..i], Some(reference[i + 1..].to_string())),
            None => (reference, None),
        };
        out.query = query;

        let merged = if path_part.starts_with('/') {
            path_part.to_string()
        } else {
            // Replace the last segment of the base path.
            let base_dir = match self.path.rfind('/') {
                Some(i) => &self.path[..=i],
                None => "/",
            };
            format!("{base_dir}{path_part}")
        };
        out.path = normalize_path(&merged);
        Ok(out)
    }

    /// Renders the URL, deliberately omitting any userinfo.
    ///
    /// Credentials in a URL must never reach a log file, the UI, or a state
    /// file on disk; [`Url::to_string_with_credentials`] is the explicit
    /// opt-in for the rare case that needs them.
    pub fn to_string_safe(&self) -> String {
        self.render(false)
    }

    pub fn to_string_with_credentials(&self) -> String {
        self.render(true)
    }

    fn render(&self, credentials: bool) -> String {
        let mut s = format!("{}://", self.scheme);
        if credentials && !self.username.is_empty() {
            s.push_str(&percent_encode(&self.username, USERINFO));
            if !self.password.is_empty() {
                s.push(':');
                s.push_str(&percent_encode(&self.password, USERINFO));
            }
            s.push('@');
        }
        if self.is_ipv6 {
            s.push('[');
            s.push_str(&self.host);
            s.push(']');
        } else {
            s.push_str(&self.host);
        }
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        s.push_str(&self.path);
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        if let Some(f) = &self.fragment {
            s.push('#');
            s.push_str(f);
        }
        s
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_safe())
    }
}

/// Removes `.` and `..` segments (RFC 3986 §5.2.4).
///
/// This is a security control as much as a correctness one: without it a
/// redirect to `/../../etc/passwd` would be sent to the server verbatim.
fn normalize_path(path: &str) -> String {
    let path = if path.is_empty() { "/" } else { path };
    let mut out: Vec<&str> = Vec::new();
    let trailing_slash = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");

    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut result = String::from("/");
    result.push_str(&out.join("/"));
    if trailing_slash && !result.ends_with('/') {
        result.push('/');
    }
    result
}

// ------------------------------------------------------------ percent-coding

/// Characters that never need escaping: ALPHA / DIGIT / "-" / "." / "_" / "~".
const UNRESERVED: &str = "-._~";
/// Additionally allowed inside a path.
pub const PATH: &str = "-._~!$&'()*+,;=:@/";
/// Additionally allowed inside a query string.
pub const QUERY: &str = "-._~!$&'()*+,;=:@/?";
/// Allowed inside userinfo.
pub const USERINFO: &str = "-._~!$&'()*+,;=";
/// Nothing extra: for a single path or query component.
pub const COMPONENT: &str = "-._~";

/// Percent-encodes everything outside the unreserved set plus `extra`.
pub fn percent_encode(input: &str, extra: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || UNRESERVED.contains(c) || extra.contains(c) {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes `%XX` sequences. Invalid escapes are left as literal text rather
/// than dropped, which keeps filenames containing a bare `%` intact.
pub fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Decodes to a `String`, replacing invalid UTF-8 rather than failing —
/// servers do serve filenames in legacy encodings.
pub fn percent_decode_str(input: &str) -> String {
    String::from_utf8_lossy(&percent_decode(input)).into_owned()
}
