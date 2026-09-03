//! Shared 64-byte block buffering for MD5, SHA-1 and SHA-256.
//!
//! All three have the same shape — absorb a byte stream, compress every full
//! 64-byte block, then pad with `0x80`, zeros, and a 64-bit bit-count — so the
//! bookkeeping lives here once instead of three times.

#[derive(Clone)]
pub(crate) struct Block64 {
    buf: [u8; 64],
    len: usize,
    /// Total bytes absorbed, used to build the length suffix.
    pub(crate) total: u64,
}

impl Block64 {
    pub(crate) fn new() -> Self {
        Block64 {
            buf: [0; 64],
            len: 0,
            total: 0,
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8], mut compress: impl FnMut(&[u8; 64])) {
        self.total = self.total.wrapping_add(data.len() as u64);

        // Top up a partial block first.
        if self.len > 0 {
            let take = (64 - self.len).min(data.len());
            self.buf[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
            if self.len == 64 {
                compress(&self.buf);
                self.len = 0;
            }
        }

        // Then run whole blocks straight from the caller's slice.
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            compress(block.try_into().unwrap());
            data = rest;
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.len = data.len();
        }
    }

    /// Appends `0x80`, zero padding, and the bit length in the given byte
    /// order (little-endian for MD5, big-endian for the SHA family).
    pub(crate) fn finish(
        &mut self,
        little_endian_length: bool,
        mut compress: impl FnMut(&[u8; 64]),
    ) {
        let bits = self.total.wrapping_mul(8);
        let mut tail = [0u8; 128];
        tail[0] = 0x80;
        // Pad so that the total length is 56 mod 64, leaving room for the count.
        let pad_len = if self.len < 56 {
            56 - self.len
        } else {
            120 - self.len
        };
        let count = if little_endian_length {
            bits.to_le_bytes()
        } else {
            bits.to_be_bytes()
        };
        tail[pad_len..pad_len + 8].copy_from_slice(&count);

        let total = pad_len + 8;
        let mut i = 0;
        // Feed the tail through the same buffering path.
        while i < total {
            let take = (64 - self.len).min(total - i);
            self.buf[self.len..self.len + take].copy_from_slice(&tail[i..i + take]);
            self.len += take;
            i += take;
            if self.len == 64 {
                compress(&self.buf);
                self.len = 0;
            }
        }
    }
}

/// Reads a 64-byte block as 16 little-endian words (MD5).
pub(crate) fn words_le(block: &[u8; 64]) -> [u32; 16] {
    let mut w = [0u32; 16];
    for (i, word) in w.iter_mut().enumerate() {
        *word = u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
    }
    w
}

/// Reads a 64-byte block as 16 big-endian words (SHA-1, SHA-256).
pub(crate) fn words_be(block: &[u8; 64]) -> [u32; 16] {
    let mut w = [0u32; 16];
    for (i, word) in w.iter_mut().enumerate() {
        *word = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
    }
    w
}
