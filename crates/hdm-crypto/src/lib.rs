//! Dependency-free cryptographic primitives for Hydra Download Manager.
//!
//! Deliberately minimal: hashes for checksum verification and HTTP Digest
//! authentication, base64 for Basic auth and the WebSocket handshake, and a
//! CSPRNG for API tokens. Nothing here invents a new construction — every
//! algorithm is a straight implementation of its specification, checked
//! against the published test vectors in `tests/`.
//!
//! ```
//! use hdm_crypto::{hex, Digest, Sha256};
//!
//! assert_eq!(
//!     hex(&Sha256::digest(b"abc")),
//!     "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
//! );
//! ```

mod base64;
mod block;
mod hmac;
mod md5;
mod rand;
mod sha1;
mod sha256;

pub use base64::{decode as base64_decode, encode as base64_encode, encode_url_safe};
pub use hmac::hmac;
pub use md5::Md5;
pub use rand::{bytes as random_bytes, fill as random_fill, token as random_token};
pub use sha1::Sha1;
pub use sha256::Sha256;

/// A streaming hash function.
pub trait Digest: Clone {
    /// Internal compression block size, in bytes. HMAC needs this.
    const BLOCK_SIZE: usize;
    /// Digest length in bytes.
    const OUTPUT_SIZE: usize;

    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    fn finish(self) -> Vec<u8>;

    /// Hashes a single buffer in one call.
    fn digest(data: &[u8]) -> Vec<u8> {
        let mut d = Self::new();
        d.update(data);
        d.finish()
    }

    /// Hashes a buffer and returns lowercase hex, the form checksums are
    /// published and compared in.
    fn hex_digest(data: &[u8]) -> String {
        hex(&Self::digest(data))
    }
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0xf) as usize] as char);
    }
    s
}

/// Decodes lowercase or uppercase hex. Returns `None` on odd length or a
/// non-hex character.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    let t = text.as_bytes();
    if t.len() % 2 != 0 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    t.chunks(2)
        .map(|p| Some((nibble(p[0])? << 4) | nibble(p[1])?))
        .collect()
}

/// Compares two byte strings in time that does not depend on where they first
/// differ.
///
/// API tokens are compared with this so that a local attacker cannot recover a
/// token byte-by-byte by timing rejected requests. The length check leaks only
/// the length, which is fixed and public.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Identifies which checksum algorithm a user-supplied digest refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
}

impl HashAlgo {
    /// Parses a name as written in a UI field or a `.checksum` file.
    pub fn parse(name: &str) -> Option<HashAlgo> {
        match name.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "md5" => Some(HashAlgo::Md5),
            "sha1" => Some(HashAlgo::Sha1),
            "sha256" => Some(HashAlgo::Sha256),
            _ => None,
        }
    }

    /// Guesses the algorithm from a bare hex digest's length, which is how
    /// most download pages present one.
    pub fn from_hex_len(digest: &str) -> Option<HashAlgo> {
        match digest.trim().len() {
            32 => Some(HashAlgo::Md5),
            40 => Some(HashAlgo::Sha1),
            64 => Some(HashAlgo::Sha256),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            HashAlgo::Md5 => "md5",
            HashAlgo::Sha1 => "sha1",
            HashAlgo::Sha256 => "sha256",
        }
    }
}

/// A hasher chosen at runtime, so the engine can checksum a download with
/// whichever algorithm the user supplied.
#[derive(Clone)]
pub enum AnyHasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
}

impl AnyHasher {
    pub fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Md5 => AnyHasher::Md5(Md5::new()),
            HashAlgo::Sha1 => AnyHasher::Sha1(Sha1::new()),
            HashAlgo::Sha256 => AnyHasher::Sha256(Sha256::new()),
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        match self {
            AnyHasher::Md5(h) => h.update(data),
            AnyHasher::Sha1(h) => h.update(data),
            AnyHasher::Sha256(h) => h.update(data),
        }
    }

    pub fn hex(self) -> String {
        match self {
            AnyHasher::Md5(h) => hex(&h.finish()),
            AnyHasher::Sha1(h) => hex(&h.finish()),
            AnyHasher::Sha256(h) => hex(&h.finish()),
        }
    }
}
