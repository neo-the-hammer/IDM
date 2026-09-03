//! HMAC (RFC 2104), generic over any [`Digest`] in this crate.

use crate::Digest;

/// Computes `HMAC(key, message)` using digest `D`.
pub fn hmac<D: Digest>(key: &[u8], message: &[u8]) -> Vec<u8> {
    // Keys longer than the block size are hashed down first; shorter keys are
    // zero-padded.
    let mut block = vec![0u8; D::BLOCK_SIZE];
    if key.len() > D::BLOCK_SIZE {
        let hashed = D::digest(key);
        block[..hashed.len()].copy_from_slice(&hashed);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner = D::new();
    inner.update(&block.iter().map(|b| b ^ 0x36).collect::<Vec<_>>());
    inner.update(message);
    let inner = inner.finish();

    let mut outer = D::new();
    outer.update(&block.iter().map(|b| b ^ 0x5c).collect::<Vec<_>>());
    outer.update(&inner);
    outer.finish()
}
