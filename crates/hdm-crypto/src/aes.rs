//! AES in CBC mode.
//!
//! Present for one reason: a large fraction of HLS streams are encrypted with
//! `METHOD=AES-128`, and a downloader that fetches those segments faithfully
//! and then writes undecipherable noise to disk is worse than one that refuses.
//!
//! Both directions are implemented. Hydra only ever decrypts, but a cipher
//! that can round-trip is a cipher whose decryption can be tested at every
//! length and alignment rather than only against the handful of published
//! known-answer vectors — and those vectors are here too, for both directions.
//!
//! The S-box is *derived* rather than typed in. A 256-entry table of constants
//! is exactly the kind of thing a single transposed digit ruins silently, and
//! generating it from the field inverse and the affine transform that define it
//! costs 256 iterations once per process.

use std::sync::OnceLock;

/// The AES block size, in bytes. Fixed at 128 bits for every key length.
pub const BLOCK: usize = 16;

struct Tables {
    sbox: [u8; 256],
    inv_sbox: [u8; 256],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(build_tables)
}

/// Multiplication in GF(2^8) modulo the AES polynomial x^8+x^4+x^3+x+1.
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        let high = a & 0x80;
        a <<= 1;
        if high != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    product
}

fn build_tables() -> Tables {
    // exp/log to the generator 3, which is what makes inversion a subtraction.
    let mut exp = [0u8; 256];
    let mut log = [0u8; 256];
    let mut value: u8 = 1;
    for (i, slot) in exp.iter_mut().enumerate().take(255) {
        *slot = value;
        log[value as usize] = i as u8;
        value = gmul(value, 3);
    }
    exp[255] = exp[0];

    let mut sbox = [0u8; 256];
    let mut inv_sbox = [0u8; 256];
    for x in 0..=255u8 {
        // Zero is its own "inverse" by definition of the S-box.
        let inverse = if x == 0 {
            0
        } else {
            exp[(255 - log[x as usize] as usize) % 255]
        };
        let s = inverse
            ^ inverse.rotate_left(1)
            ^ inverse.rotate_left(2)
            ^ inverse.rotate_left(3)
            ^ inverse.rotate_left(4)
            ^ 0x63;
        sbox[x as usize] = s;
        inv_sbox[s as usize] = x;
    }
    Tables { sbox, inv_sbox }
}

/// An expanded AES key, ready to decrypt blocks.
pub struct Aes {
    /// The round keys, four bytes at a time.
    words: Vec<[u8; 4]>,
    rounds: usize,
}

impl Aes {
    /// Expands a 16, 24 or 32 byte key.
    pub fn new(key: &[u8]) -> Result<Aes, String> {
        let nk = match key.len() {
            16 => 4,
            24 => 6,
            32 => 8,
            other => {
                return Err(format!(
                    "an AES key must be 16, 24 or 32 bytes, not {other}"
                ))
            }
        };
        let rounds = nk + 6;
        let sbox = &tables().sbox;

        let mut words: Vec<[u8; 4]> = key
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();

        let mut rcon: u8 = 1;
        for i in nk..4 * (rounds + 1) {
            let mut temp = words[i - 1];
            if i % nk == 0 {
                temp.rotate_left(1);
                for byte in temp.iter_mut() {
                    *byte = sbox[*byte as usize];
                }
                temp[0] ^= rcon;
                rcon = gmul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                for byte in temp.iter_mut() {
                    *byte = sbox[*byte as usize];
                }
            }
            let previous = words[i - nk];
            words.push([
                previous[0] ^ temp[0],
                previous[1] ^ temp[1],
                previous[2] ^ temp[2],
                previous[3] ^ temp[3],
            ]);
        }
        Ok(Aes { words, rounds })
    }

    fn add_round_key(&self, state: &mut [u8; BLOCK], round: usize) {
        for column in 0..4 {
            let word = self.words[round * 4 + column];
            for row in 0..4 {
                state[4 * column + row] ^= word[row];
            }
        }
    }

    /// Encrypts one block in place.
    pub fn encrypt_block(&self, state: &mut [u8; BLOCK]) {
        let sbox = &tables().sbox;
        self.add_round_key(state, 0);
        for round in 1..self.rounds {
            for byte in state.iter_mut() {
                *byte = sbox[*byte as usize];
            }
            shift_rows(state);
            mix_columns(state);
            self.add_round_key(state, round);
        }
        for byte in state.iter_mut() {
            *byte = sbox[*byte as usize];
        }
        shift_rows(state);
        self.add_round_key(state, self.rounds);
    }

    /// Decrypts one block in place.
    pub fn decrypt_block(&self, state: &mut [u8; BLOCK]) {
        let inv_sbox = &tables().inv_sbox;
        self.add_round_key(state, self.rounds);
        for round in (1..self.rounds).rev() {
            inv_shift_rows(state);
            for byte in state.iter_mut() {
                *byte = inv_sbox[*byte as usize];
            }
            self.add_round_key(state, round);
            inv_mix_columns(state);
        }
        inv_shift_rows(state);
        for byte in state.iter_mut() {
            *byte = inv_sbox[*byte as usize];
        }
        self.add_round_key(state, 0);
    }
}

/// State byte `4c + r` is row `r` of column `c`; row `r` rotates left by `r`.
fn shift_rows(state: &mut [u8; BLOCK]) {
    let original = *state;
    for row in 1..4 {
        for column in 0..4 {
            state[4 * column + row] = original[4 * ((column + row) % 4) + row];
        }
    }
}

fn mix_columns(state: &mut [u8; BLOCK]) {
    for column in 0..4 {
        let c = &mut state[4 * column..4 * column + 4];
        let (a0, a1, a2, a3) = (c[0], c[1], c[2], c[3]);
        c[0] = gmul(a0, 2) ^ gmul(a1, 3) ^ a2 ^ a3;
        c[1] = a0 ^ gmul(a1, 2) ^ gmul(a2, 3) ^ a3;
        c[2] = a0 ^ a1 ^ gmul(a2, 2) ^ gmul(a3, 3);
        c[3] = gmul(a0, 3) ^ a1 ^ a2 ^ gmul(a3, 2);
    }
}

/// The inverse: row `r` rotates right by `r`.
fn inv_shift_rows(state: &mut [u8; BLOCK]) {
    let original = *state;
    for row in 1..4 {
        for column in 0..4 {
            state[4 * column + row] = original[4 * ((column + 4 - row) % 4) + row];
        }
    }
}

fn inv_mix_columns(state: &mut [u8; BLOCK]) {
    for column in 0..4 {
        let c = &mut state[4 * column..4 * column + 4];
        let (a0, a1, a2, a3) = (c[0], c[1], c[2], c[3]);
        c[0] = gmul(a0, 14) ^ gmul(a1, 11) ^ gmul(a2, 13) ^ gmul(a3, 9);
        c[1] = gmul(a0, 9) ^ gmul(a1, 14) ^ gmul(a2, 11) ^ gmul(a3, 13);
        c[2] = gmul(a0, 13) ^ gmul(a1, 9) ^ gmul(a2, 14) ^ gmul(a3, 11);
        c[3] = gmul(a0, 11) ^ gmul(a1, 13) ^ gmul(a2, 9) ^ gmul(a3, 14);
    }
}

/// Encrypts with CBC and PKCS#7 padding.
///
/// Hydra itself never calls this; the tests do, which is what lets the
/// decryption path be exercised at every message length rather than only where
/// a published vector happens to exist.
pub fn cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    if iv.len() != BLOCK {
        return Err(format!("an AES-CBC IV must be {BLOCK} bytes"));
    }
    let aes = Aes::new(key)?;

    // PKCS#7 always adds padding, a whole block of it when the message already
    // ends on a boundary; that is what makes unpadding unambiguous.
    let mut padded = data.to_vec();
    let padding = BLOCK - (data.len() % BLOCK);
    padded.extend(std::iter::repeat(padding as u8).take(padding));

    let mut previous: [u8; BLOCK] = iv.try_into().expect("checked above");
    let mut out = Vec::with_capacity(padded.len());
    for chunk in padded.chunks_exact(BLOCK) {
        let mut block: [u8; BLOCK] = chunk.try_into().expect("chunks_exact");
        for i in 0..BLOCK {
            block[i] ^= previous[i];
        }
        aes.encrypt_block(&mut block);
        out.extend_from_slice(&block);
        previous = block;
    }
    Ok(out)
}

/// Decrypts CBC ciphertext and strips its PKCS#7 padding.
///
/// This is what an HLS `AES-128` segment is: the whole segment is one CBC
/// message, padded, with the IV either given by the playlist or derived from
/// the segment's media sequence number.
pub fn cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let mut plain = cbc_decrypt_raw(key, iv, data)?;
    let padding = *plain.last().ok_or("nothing to unpad")? as usize;
    if padding == 0 || padding > BLOCK || padding > plain.len() {
        return Err("the decrypted block has invalid padding: the key is wrong".into());
    }
    // Every padding byte must carry the same value, which is the cheapest
    // check there is that the key was actually right.
    if plain[plain.len() - padding..]
        .iter()
        .any(|&b| b as usize != padding)
    {
        return Err("the decrypted block has invalid padding: the key is wrong".into());
    }
    plain.truncate(plain.len() - padding);
    Ok(plain)
}

/// Decrypts CBC ciphertext, leaving any padding in place.
pub fn cbc_decrypt_raw(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    if iv.len() != BLOCK {
        return Err(format!("an AES-CBC IV must be {BLOCK} bytes"));
    }
    if data.is_empty() || data.len() % BLOCK != 0 {
        return Err(format!(
            "AES-CBC input must be a whole number of {BLOCK}-byte blocks, got {}",
            data.len()
        ));
    }
    let aes = Aes::new(key)?;
    let mut previous: [u8; BLOCK] = iv.try_into().expect("checked above");
    let mut out = Vec::with_capacity(data.len());

    for chunk in data.chunks_exact(BLOCK) {
        let ciphertext: [u8; BLOCK] = chunk.try_into().expect("chunks_exact");
        let mut block = ciphertext;
        aes.decrypt_block(&mut block);
        for i in 0..BLOCK {
            block[i] ^= previous[i];
        }
        out.extend_from_slice(&block);
        previous = ciphertext;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unhex;

    fn encrypt_one(key: &str, plaintext: &str) -> String {
        let mut block: [u8; BLOCK] = unhex(plaintext).unwrap().try_into().unwrap();
        Aes::new(&unhex(key).unwrap())
            .unwrap()
            .encrypt_block(&mut block);
        crate::hex(&block)
    }

    fn decrypt_one(key: &str, ciphertext: &str) -> String {
        let mut block: [u8; BLOCK] = unhex(ciphertext).unwrap().try_into().unwrap();
        Aes::new(&unhex(key).unwrap())
            .unwrap()
            .decrypt_block(&mut block);
        crate::hex(&block)
    }

    // FIPS-197 appendix C, all three key lengths.
    #[test]
    fn fips_197_aes_128() {
        assert_eq!(
            decrypt_one(
                "000102030405060708090a0b0c0d0e0f",
                "69c4e0d86a7b0430d8cdb78070b4c55a"
            ),
            "00112233445566778899aabbccddeeff"
        );
    }

    #[test]
    fn fips_197_aes_192() {
        assert_eq!(
            decrypt_one(
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "dda97ca4864cdfe06eaf70a0ec0d7191"
            ),
            "00112233445566778899aabbccddeeff"
        );
    }

    #[test]
    fn fips_197_aes_256() {
        assert_eq!(
            decrypt_one(
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8ea2b7ca516745bfeafc49904b496089"
            ),
            "00112233445566778899aabbccddeeff"
        );
    }

    /// NIST SP 800-38A F.2.2, CBC-AES128.Decrypt, all four blocks.
    #[test]
    fn sp_800_38a_cbc_aes_128() {
        let key = unhex("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = unhex("000102030405060708090a0b0c0d0e0f").unwrap();
        let ciphertext = unhex(
            "7649abac8119b246cee98e9b12e9197d\
             5086cb9b507219ee95db113a917678b2\
             73bed6b8e3c1743b7116e69e22229516\
             3ff1caa1681fac09120eca307586e1a7",
        )
        .unwrap();
        assert_eq!(
            crate::hex(&cbc_decrypt_raw(&key, &iv, &ciphertext).unwrap()),
            "6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710"
        );
    }

    #[test]
    fn fips_197_forward_direction() {
        for (key, expected) in [
            (
                "000102030405060708090a0b0c0d0e0f",
                "69c4e0d86a7b0430d8cdb78070b4c55a",
            ),
            (
                "000102030405060708090a0b0c0d0e0f1011121314151617",
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ] {
            assert_eq!(
                encrypt_one(key, "00112233445566778899aabbccddeeff"),
                expected
            );
        }
    }

    #[test]
    fn cbc_round_trips_at_every_length_and_alignment() {
        let key = unhex("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = unhex("000102030405060708090a0b0c0d0e0f").unwrap();
        for length in 0..80 {
            let message: Vec<u8> = (0..length).map(|i| (i * 7 + 3) as u8).collect();
            let ciphertext = cbc_encrypt(&key, &iv, &message).unwrap();
            // Padding is always added, so the ciphertext always grows.
            assert!(ciphertext.len() > message.len());
            assert_eq!(ciphertext.len() % BLOCK, 0);
            assert_eq!(cbc_decrypt(&key, &iv, &ciphertext).unwrap(), message);
        }
    }

    /// The S-box has fixed, well-known values; if the derivation is wrong these
    /// are the first things that differ.
    #[test]
    fn derived_sbox_matches_the_standard() {
        let t = tables();
        assert_eq!(t.sbox[0x00], 0x63);
        assert_eq!(t.sbox[0x01], 0x7c);
        assert_eq!(t.sbox[0x53], 0xed);
        assert_eq!(t.sbox[0xff], 0x16);
        for x in 0..=255u8 {
            assert_eq!(t.inv_sbox[t.sbox[x as usize] as usize], x);
        }
    }

    #[test]
    fn a_wrong_key_is_reported_rather_than_returning_noise() {
        let data = vec![0u8; 32];
        let error = cbc_decrypt(&[0u8; 16], &[0u8; 16], &data).unwrap_err();
        assert!(error.contains("padding"), "{error}");
    }

    #[test]
    fn rejects_input_that_is_not_a_whole_number_of_blocks() {
        assert!(cbc_decrypt_raw(&[0u8; 16], &[0u8; 16], &[0u8; 17]).is_err());
        assert!(cbc_decrypt_raw(&[0u8; 16], &[0u8; 15], &[0u8; 16]).is_err());
        assert!(cbc_decrypt_raw(&[0u8; 17], &[0u8; 16], &[0u8; 16]).is_err());
    }
}
