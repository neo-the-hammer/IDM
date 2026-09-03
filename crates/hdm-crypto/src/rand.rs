//! Cryptographically secure random bytes from the operating system.
//!
//! Used for API bearer tokens and browser-extension pairing secrets, so a
//! predictable source here would let a local process take over the daemon.
//! There is no userspace PRNG fallback on purpose: if the OS cannot give us
//! entropy we fail loudly rather than quietly generating guessable tokens.

/// Fills `buf` with random bytes, or returns an error describing why it could not.
pub fn fill(buf: &mut [u8]) -> Result<(), String> {
    imp::fill(buf)
}

/// Returns `n` random bytes.
pub fn bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut v = vec![0u8; n];
    fill(&mut v)?;
    Ok(v)
}

/// Returns a URL-safe random token with `bytes` of entropy.
/// 32 bytes (256 bits) is the default for API tokens.
pub fn token(n: usize) -> Result<String, String> {
    Ok(crate::base64::encode_url_safe(&bytes(n)?))
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::io::Read;

    pub fn fill(buf: &mut [u8]) -> Result<(), String> {
        // /dev/urandom is the right call on modern Unix: it never blocks after
        // boot-time seeding and is the same CSPRNG as getrandom(2).
        let mut f =
            File::open("/dev/urandom").map_err(|e| format!("cannot open /dev/urandom: {e}"))?;
        f.read_exact(buf)
            .map_err(|e| format!("cannot read /dev/urandom: {e}"))
    }
}

#[cfg(windows)]
mod imp {
    // BCryptGenRandom with BCRYPT_USE_SYSTEM_PREFERRED_RNG is the supported
    // modern replacement for the deprecated CryptGenRandom, and needs no
    // algorithm handle.
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(
            h_algorithm: *mut core::ffi::c_void,
            pb_buffer: *mut u8,
            cb_buffer: u32,
            dw_flags: u32,
        ) -> i32;
    }

    pub fn fill(buf: &mut [u8]) -> Result<(), String> {
        if buf.is_empty() {
            return Ok(());
        }
        // Chunk because cbBuffer is a ULONG; buffers this large never occur in
        // practice but the cast must stay lossless.
        for chunk in buf.chunks_mut(u32::MAX as usize) {
            let status = unsafe {
                BCryptGenRandom(
                    core::ptr::null_mut(),
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                )
            };
            if status != 0 {
                return Err(format!(
                    "BCryptGenRandom failed with NTSTATUS 0x{status:08x}"
                ));
            }
        }
        Ok(())
    }
}
