//! HTTP authentication: Basic (RFC 7617) and Digest (RFC 7616).
//!
//! Many download links sit behind a login, and IDM has supported both schemes
//! for years, so Hydra does too.

use crate::headers::Headers;
use hdm_crypto::{base64_encode, hex, random_bytes, Digest as _, Md5, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// A challenge parsed from a `WWW-Authenticate` or `Proxy-Authenticate` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Challenge {
    Basic { realm: String },
    Digest(DigestChallenge),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestChallenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: Option<String>,
    pub qop: Option<String>,
    pub algorithm: DigestAlgorithm,
    /// Set when the server rejected our nonce but the credentials were right,
    /// meaning we should retry with the fresh nonce rather than prompt again.
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestAlgorithm {
    Md5,
    Md5Sess,
    Sha256,
    Sha256Sess,
}

impl DigestAlgorithm {
    fn parse(name: &str) -> Option<DigestAlgorithm> {
        match name.trim().to_ascii_uppercase().as_str() {
            "MD5" => Some(DigestAlgorithm::Md5),
            "MD5-SESS" => Some(DigestAlgorithm::Md5Sess),
            "SHA-256" | "SHA256" => Some(DigestAlgorithm::Sha256),
            "SHA-256-SESS" | "SHA256-SESS" => Some(DigestAlgorithm::Sha256Sess),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            DigestAlgorithm::Md5 => "MD5",
            DigestAlgorithm::Md5Sess => "MD5-sess",
            DigestAlgorithm::Sha256 => "SHA-256",
            DigestAlgorithm::Sha256Sess => "SHA-256-sess",
        }
    }

    fn hash(self, data: &[u8]) -> String {
        match self {
            DigestAlgorithm::Md5 | DigestAlgorithm::Md5Sess => hex(&Md5::digest(data)),
            DigestAlgorithm::Sha256 | DigestAlgorithm::Sha256Sess => hex(&Sha256::digest(data)),
        }
    }

    fn is_sess(self) -> bool {
        matches!(self, DigestAlgorithm::Md5Sess | DigestAlgorithm::Sha256Sess)
    }
}

/// Picks the strongest challenge the server offered.
///
/// Digest is preferred over Basic because Basic puts the password on the wire
/// in reversible form; over plain HTTP that is the difference between a
/// password an eavesdropper can read and one they cannot.
pub fn select_challenge(headers: &Headers, header_name: &str) -> Option<Challenge> {
    let mut basic = None;
    for value in headers.get_all(header_name) {
        match parse_challenge(value) {
            Some(Challenge::Digest(d)) => return Some(Challenge::Digest(d)),
            Some(b @ Challenge::Basic { .. }) => basic = Some(b),
            None => {}
        }
    }
    basic
}

fn parse_challenge(value: &str) -> Option<Challenge> {
    let value = value.trim();
    let (scheme, rest) = match value.split_once(' ') {
        Some((s, r)) => (s, r),
        None => (value, ""),
    };
    let params = parse_auth_params(rest);

    if scheme.eq_ignore_ascii_case("basic") {
        return Some(Challenge::Basic {
            realm: params.get("realm").cloned().unwrap_or_default(),
        });
    }
    if scheme.eq_ignore_ascii_case("digest") {
        let algorithm = params
            .get("algorithm")
            .and_then(|a| DigestAlgorithm::parse(a))
            .unwrap_or(DigestAlgorithm::Md5);
        return Some(Challenge::Digest(DigestChallenge {
            realm: params.get("realm").cloned().unwrap_or_default(),
            nonce: params.get("nonce").cloned()?,
            opaque: params.get("opaque").cloned(),
            qop: params.get("qop").cloned(),
            algorithm,
            stale: params
                .get("stale")
                .map(|s| s.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        }));
    }
    None
}

/// Splits `k=v, k="v, with comma"` honouring quotes.
fn parse_auth_params(input: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut in_value = false;
    let mut in_quotes = false;
    let mut escaped = false;

    let mut flush = |key: &mut String, value: &mut String, in_value: &mut bool| {
        let k = key.trim().to_ascii_lowercase();
        if !k.is_empty() {
            out.insert(k, value.trim().trim_matches('"').to_string());
        }
        key.clear();
        value.clear();
        *in_value = false;
    };

    for c in input.chars() {
        match c {
            '\\' if in_quotes && !escaped => {
                escaped = true;
            }
            '"' if !escaped => {
                in_quotes = !in_quotes;
                value.push(c);
            }
            '=' if !in_quotes && !in_value => in_value = true,
            ',' if !in_quotes => flush(&mut key, &mut value, &mut in_value),
            c => {
                escaped = false;
                if in_value {
                    value.push(c);
                } else {
                    key.push(c);
                }
            }
        }
    }
    flush(&mut key, &mut value, &mut in_value);
    out
}

/// Builds the `Authorization` value for a Basic challenge.
pub fn basic_header(credentials: &Credentials) -> String {
    let raw = format!("{}:{}", credentials.username, credentials.password);
    format!("Basic {}", base64_encode(raw.as_bytes()))
}

/// Per-connection Digest state. `nc` must increase with every request that
/// reuses a nonce, and the server rejects a repeat.
#[derive(Debug, Clone, Default)]
pub struct DigestState {
    nonce_count: u32,
}

impl DigestState {
    /// Builds the `Authorization` value for a Digest challenge.
    ///
    /// `uri` must be the request target exactly as sent, since it is hashed.
    pub fn header(
        &mut self,
        challenge: &DigestChallenge,
        credentials: &Credentials,
        method: &str,
        uri: &str,
    ) -> String {
        let algo = challenge.algorithm;
        self.nonce_count += 1;
        let nc = format!("{:08x}", self.nonce_count);
        let cnonce = hex(&random_bytes(8).unwrap_or_else(|_| {
            // Falling back to a timestamp keeps auth working if the CSPRNG is
            // unavailable; cnonce only needs to be non-repeating, not secret.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos().to_le_bytes()[..8].to_vec())
                .unwrap_or_else(|_| vec![0; 8])
        }));

        let mut ha1 = algo.hash(
            format!(
                "{}:{}:{}",
                credentials.username, challenge.realm, credentials.password
            )
            .as_bytes(),
        );
        if algo.is_sess() {
            ha1 = algo.hash(format!("{ha1}:{}:{cnonce}", challenge.nonce).as_bytes());
        }
        let ha2 = algo.hash(format!("{method}:{uri}").as_bytes());

        // Servers may offer both "auth" and "auth-int"; we only implement
        // "auth", which is what every real deployment uses.
        let qop = challenge
            .qop
            .as_deref()
            .map(|q| q.split(',').map(str::trim).collect::<Vec<_>>())
            .filter(|opts| opts.contains(&"auth"))
            .map(|_| "auth");

        let response = match qop {
            Some(q) => {
                algo.hash(format!("{ha1}:{}:{nc}:{cnonce}:{q}:{ha2}", challenge.nonce).as_bytes())
            }
            None => algo.hash(format!("{ha1}:{}:{ha2}", challenge.nonce).as_bytes()),
        };

        let mut header = format!(
            "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", \
             algorithm={}",
            escape_quotes(&credentials.username),
            escape_quotes(&challenge.realm),
            escape_quotes(&challenge.nonce),
            escape_quotes(uri),
            response,
            algo.name(),
        );
        if let Some(q) = qop {
            header.push_str(&format!(", qop={q}, nc={nc}, cnonce=\"{cnonce}\""));
        }
        if let Some(opaque) = &challenge.opaque {
            header.push_str(&format!(", opaque=\"{}\"", escape_quotes(opaque)));
        }
        header
    }
}

/// Escapes a value going into a quoted-string, so a crafted username cannot
/// inject extra authentication parameters.
fn escape_quotes(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
