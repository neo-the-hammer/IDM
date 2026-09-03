//! SHA-1 (RFC 3174).
//!
//! Like MD5, SHA-1 is not collision resistant and is here only for protocol
//! compatibility: the WebSocket opening handshake (RFC 6455) specifies it, and
//! some download mirrors still publish SHA-1 checksums.

use crate::block::{words_be, Block64};
use crate::Digest;

#[derive(Clone)]
pub struct Sha1 {
    state: [u32; 5],
    block: Block64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    fn compress(state: &mut [u32; 5], block: &[u8; 64]) {
        let mut w = [0u32; 80];
        w[..16].copy_from_slice(&words_be(block));
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = *state;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5a827999),
                1 => (b ^ c ^ d, 0x6ed9eba1),
                2 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }

        for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }
}

impl Digest for Sha1 {
    const BLOCK_SIZE: usize = 64;
    const OUTPUT_SIZE: usize = 20;

    fn new() -> Self {
        Sha1 {
            state: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            block: Block64::new(),
        }
    }

    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        self.block.update(data, |b| Sha1::compress(state, b));
    }

    fn finish(mut self) -> Vec<u8> {
        let state = &mut self.state;
        self.block.finish(false, |b| Sha1::compress(state, b));
        self.state.iter().flat_map(|w| w.to_be_bytes()).collect()
    }
}
