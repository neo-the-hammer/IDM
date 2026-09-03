//! A minimal TLS server for the test origin, bound to OpenSSL at run time.
//!
//! Deliberately separate from `hdm_net::tls`: the crate under test must not
//! supply the machinery that tests it, or a bug in the loader would make both
//! sides agree and the test pass anyway.
//!
//! The certificate below is a throwaway self-signed pair generated solely for
//! this test suite. It is committed on purpose so the tests need no external
//! tooling and stay deterministic; it is valid only for `localhost` and
//! `127.0.0.1`, and its private key is public by construction. It must never
//! be used for anything else.

use std::ffi::{c_char, c_int, c_long, c_void, CString};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::io::AsRawFd;
use std::sync::OnceLock;

use crate::Conn;

const RTLD_NOW: c_int = 2;
const SSL_FILETYPE_PEM: c_int = 1;
const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
const TLS1_2_VERSION: c_long = 0x0303;

pub const TEST_CERT_PEM: &str = r#"
-----BEGIN CERTIFICATE-----
MIIDfzCCAmegAwIBAgIUHRf586NcEu8Wy/YvECMvXukRfcgwDQYJKoZIhvcNAQEL
BQAwQDESMBAGA1UEAwwJbG9jYWxob3N0MSowKAYDVQQKDCFIeWRyYSBEb3dubG9h
ZCBNYW5hZ2VyIFRlc3QgU3VpdGUwIBcNMjYwOTAzMTE0MzU2WhgPMjEyNjA4MTAx
MTQzNTZaMEAxEjAQBgNVBAMMCWxvY2FsaG9zdDEqMCgGA1UECgwhSHlkcmEgRG93
bmxvYWQgTWFuYWdlciBUZXN0IFN1aXRlMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A
MIIBCgKCAQEApjO+VA7z5ewhnIndlwMGCX8lGJTCoZ8Pm4AS9Yqk82MEU6si9ImS
ufxjg8VpXKD3rD8hb5eZ4E38Nqra+iHRXjXyncjqBn8Rsn6d+Gfs2T8uU2qgPG7u
hxaDf4czxAvLQnpSaeHG7VqQnQB3z7ljYzg/7HOyGAF+rMqB/WqGsaWsTA+NZ9qF
czQEAUb/WZ14M7NRzV7MF15LGNLWaRwVjEYTZgRypw2gYDIuJlT90tlbpXtCIxRo
CT/t1kLGJuzNd38UxANw6f0Lt2qv678w691ZHY9ueErqCxmURw8bI1R+qzekP+7q
2qdg4/4Fylp+l47jVKfQczcvgR8PECOnzwIDAQABo28wbTAdBgNVHQ4EFgQUm1EO
uzUBB6GbYlk8w8R6u2ek4iUwHwYDVR0jBBgwFoAUm1EOuzUBB6GbYlk8w8R6u2ek
4iUwDwYDVR0TAQH/BAUwAwEB/zAaBgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEw
DQYJKoZIhvcNAQELBQADggEBADzB9QIRPAuVqe5uMnZkW4sqw/P1PjtB7byqiKCH
V91KLRdr+1sVVb0Xcq9R1+uiJe8Yi03NOK5Z7jyMbWG7tQ80cRqygZu6aKsHG91h
n2k1f7ndhFBZq8kKOfF3Nl7qQCdzk39RKu/91NR62G9ZK5DLCOWiBiTGmhhpKpPz
OiZBVs3QicMZ95Umr7it8k+6VaBCdeXKEZThhYPhEePS1syYnOC8+6x67n26bvCL
ui8FPONPLUmQwgdlyN5myBQ3qIZXIcpTLaeU7z1+3XLaUhpy2kxjIfwu7uhuOdnt
0lmhph45QLAw1ZXtFnm0CohUSovwKbQIG9juimxDNZfvDGk=
-----END CERTIFICATE-----
"#;

pub const TEST_KEY_PEM: &str = r#"
-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCmM75UDvPl7CGc
id2XAwYJfyUYlMKhnw+bgBL1iqTzYwRTqyL0iZK5/GODxWlcoPesPyFvl5ngTfw2
qtr6IdFeNfKdyOoGfxGyfp34Z+zZPy5TaqA8bu6HFoN/hzPEC8tCelJp4cbtWpCd
AHfPuWNjOD/sc7IYAX6syoH9aoaxpaxMD41n2oVzNAQBRv9ZnXgzs1HNXswXXksY
0tZpHBWMRhNmBHKnDaBgMi4mVP3S2Vule0IjFGgJP+3WQsYm7M13fxTEA3Dp/Qu3
aq/rvzDr3Vkdj254SuoLGZRHDxsjVH6rN6Q/7urap2Dj/gXKWn6XjuNUp9BzNy+B
Hw8QI6fPAgMBAAECggEADKUa1yC3cWf4m1p+jPWSboOJaigwnK+ni4BVhkVGTSwx
5pRlLu9j8JmMEA5OI1mDLnbeejPcZbfujKbizS1of/9LwQcfDbUk6WHVGVKoLv6D
GElK/VNqphrSO2V+d0J3HveD21bDAJ5SKq0f8HfINi16k5NZL+wL3A/2OYvIHq4p
iE/WwN37nLG9zUbaqpIOrBfOfFCiIkbkX+CovW1TLyAuu4K+VztDdpwJya3imvcR
eUz6/zYNbRdILqsYhOCcGC1JTnPUoV0NanBD8u8dY8d+9EPf2v3V3VmtZRn4EWqC
NOwJooCVgf3ErY+m269hH0QIQEOp9hbF60jmTwPCIQKBgQDdwSMlMVduCV+tRdlZ
mdGBymZURLvN9nR8bO4phrGR2bu6BXlQdeEaBZkxlsbZbH+AOeOXoxJ8qqdL/6Y+
SyZ097kssVbUYPNx5fU5ChNZJhF+qd4NxDGU4Mj4C1G0KWmemnAXFjrDr4h0ZV3y
MZIXyxyt/DPIc6ldQUrwRtu9IQKBgQC/3mUm06qgOxk9cfvrUB/2eNmT9a5b6M/u
g+4otV5f+XWN6EAUQQZTLCIKdppJflph2VsECg4fivQgdXvTIshka+JXorCyjRNZ
nY0hWkDa5sFVVZKMmWq+LQiq4JA4mmil/iE5112Smzql/C61emSrxOqlvDK2a30V
V4PgW9VW7wKBgHDwhtfQc3jlaUc0hegugRebX9aXUxco6Fbem8WmhhWEUSoC07B4
+PZp14X8BraBncZOtW1rbmTz/VSllaOwXpu/9x2eDF0KK7LcrbIpQYVr8AkUtrVI
MQBkI7bA/RHG7bYLbf80ISW85sBxSBGr0X4wwiCSjEURMzb9pA8P56ZBAoGAWXu6
RzpumF4Xrm2LpTpwPb4tE3GAiQLyfvXuy/OSeUZZyf4obInLDl1F3wVjfaU9N+ds
KF0cKx/eLYk9X8IYHaWnIWIR8KQVAzWUjZqPJsh6IHdRatteSiWspi0ndg6lgc0c
5+IGlQpqduE/U4oqi2XCXduA90z4QEzZh3is7ecCgYEAk68dedhMKnkgl0HjpmVk
RsTchJRrwiqrWIvfHlWVlx5ZxSeQ9opVFI8+t1mnGSmMtLZy7vWP5HieQE3G3F8Z
wTIwQ8g+DQhPJNQwMZD9rT6TFIPaUTyaoXN6TkA5aV3Iv0zZbezN0ICQ6r48jNG3
MzQyxNii3NrXzDdHfefDEbc=
-----END PRIVATE KEY-----
"#;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

struct Lib {
    server_method: unsafe extern "C" fn() -> *const c_void,
    ctx_new: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    ctx_free: unsafe extern "C" fn(*mut c_void),
    ctx_ctrl: unsafe extern "C" fn(*mut c_void, c_int, c_long, *mut c_void) -> c_long,
    use_cert_file: unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int,
    use_key_file: unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int,
    ssl_new: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    ssl_free: unsafe extern "C" fn(*mut c_void),
    set_fd: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    accept: unsafe extern "C" fn(*mut c_void) -> c_int,
    read: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int,
    write: unsafe extern "C" fn(*mut c_void, *const c_void, c_int) -> c_int,
    shutdown: unsafe extern "C" fn(*mut c_void) -> c_int,
}
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

static LIB: OnceLock<Option<Lib>> = OnceLock::new();

unsafe fn sym<T: Copy>(h: *mut c_void, name: &str) -> Option<T> {
    let c = CString::new(name).ok()?;
    let p = dlsym(h, c.as_ptr());
    if p.is_null() {
        return None;
    }
    Some(*(&p as *const *mut c_void as *const T))
}

fn lib() -> Option<&'static Lib> {
    LIB.get_or_init(|| unsafe {
        let open = |names: &[&str]| -> Option<*mut c_void> {
            for n in names {
                let c = CString::new(*n).ok()?;
                let h = dlopen(c.as_ptr(), RTLD_NOW);
                if !h.is_null() {
                    return Some(h);
                }
            }
            None
        };
        open(&["libcrypto.so.3", "libcrypto.so.1.1", "libcrypto.dylib"])?;
        let s = open(&["libssl.so.3", "libssl.so.1.1", "libssl.dylib"])?;
        Some(Lib {
            server_method: sym(s, "TLS_server_method")?,
            ctx_new: sym(s, "SSL_CTX_new")?,
            ctx_free: sym(s, "SSL_CTX_free")?,
            ctx_ctrl: sym(s, "SSL_CTX_ctrl")?,
            use_cert_file: sym(s, "SSL_CTX_use_certificate_file")?,
            use_key_file: sym(s, "SSL_CTX_use_PrivateKey_file")?,
            ssl_new: sym(s, "SSL_new")?,
            ssl_free: sym(s, "SSL_free")?,
            set_fd: sym(s, "SSL_set_fd")?,
            accept: sym(s, "SSL_accept")?,
            read: sym(s, "SSL_read")?,
            write: sym(s, "SSL_write")?,
            shutdown: sym(s, "SSL_shutdown")?,
        })
    })
    .as_ref()
}

/// Paths to a certificate and key on disk, kept alive for the server's life.
pub struct CertPaths {
    pub cert: std::path::PathBuf,
    pub key: std::path::PathBuf,
    _dir: TempDir,
}

/// Writes the embedded test certificate to a temporary directory.
pub fn self_signed() -> io::Result<CertPaths> {
    let dir = TempDir::new("hydra-testcert")?;
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, TEST_CERT_PEM.trim_start())?;
    std::fs::write(&key, TEST_KEY_PEM.trim_start())?;
    Ok(CertPaths {
        cert,
        key,
        _dir: dir,
    })
}

/// The embedded certificate written to a path the client can trust explicitly.
pub fn ca_file() -> io::Result<(std::path::PathBuf, TempDir)> {
    let dir = TempDir::new("hydra-testca")?;
    let path = dir.path().join("ca.pem");
    std::fs::write(&path, TEST_CERT_PEM.trim_start())?;
    Ok((path, dir))
}

pub struct ServerContext {
    ctx: *mut c_void,
    lib: &'static Lib,
}
unsafe impl Send for ServerContext {}
unsafe impl Sync for ServerContext {}

impl ServerContext {
    pub fn new(paths: &CertPaths) -> io::Result<ServerContext> {
        let lib = lib().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "OpenSSL unavailable for the test server",
            )
        })?;
        unsafe {
            let ctx = (lib.ctx_new)((lib.server_method)());
            if ctx.is_null() {
                return Err(io::Error::other("SSL_CTX_new failed"));
            }
            (lib.ctx_ctrl)(
                ctx,
                SSL_CTRL_SET_MIN_PROTO_VERSION,
                TLS1_2_VERSION,
                std::ptr::null_mut(),
            );
            let cert = CString::new(paths.cert.to_string_lossy().as_ref()).unwrap();
            let key = CString::new(paths.key.to_string_lossy().as_ref()).unwrap();
            if (lib.use_cert_file)(ctx, cert.as_ptr(), SSL_FILETYPE_PEM) != 1 {
                return Err(io::Error::other("cannot load the test certificate"));
            }
            if (lib.use_key_file)(ctx, key.as_ptr(), SSL_FILETYPE_PEM) != 1 {
                return Err(io::Error::other("cannot load the test key"));
            }
            Ok(ServerContext { ctx, lib })
        }
    }

    pub fn accept(&self, tcp: TcpStream) -> io::Result<TlsConn> {
        unsafe {
            let ssl = (self.lib.ssl_new)(self.ctx);
            if ssl.is_null() {
                return Err(io::Error::other("SSL_new failed"));
            }
            (self.lib.set_fd)(ssl, tcp.as_raw_fd());
            if (self.lib.accept)(ssl) != 1 {
                (self.lib.ssl_free)(ssl);
                return Err(io::Error::other("TLS handshake failed"));
            }
            Ok(TlsConn {
                ssl,
                tcp,
                lib: self.lib,
            })
        }
    }
}

impl Drop for ServerContext {
    fn drop(&mut self) {
        unsafe { (self.lib.ctx_free)(self.ctx) };
    }
}

pub struct TlsConn {
    ssl: *mut c_void,
    tcp: TcpStream,
    lib: &'static Lib,
}
unsafe impl Send for TlsConn {}

impl Read for TlsConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe {
            (self.lib.read)(
                self.ssl,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as c_int,
            )
        };
        Ok(if n > 0 { n as usize } else { 0 })
    }
}

impl Write for TlsConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = unsafe {
            (self.lib.write)(self.ssl, buf.as_ptr() as *const c_void, buf.len() as c_int)
        };
        if n > 0 {
            Ok(n as usize)
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "TLS write failed",
            ))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

impl Conn for TlsConn {
    fn hard_close(&mut self) {
        // Drop the TCP connection without a close_notify, so the client sees a
        // truncated body exactly as it would from a real network failure.
        let _ = self.tcp.shutdown(std::net::Shutdown::Both);
    }
}

impl Drop for TlsConn {
    fn drop(&mut self) {
        unsafe {
            (self.lib.shutdown)(self.ssl);
            (self.lib.ssl_free)(self.ssl);
        }
    }
}

/// A temporary directory removed on drop, so tests leave nothing behind.
pub struct TempDir(std::path::PathBuf);

impl TempDir {
    pub fn new(prefix: &str) -> io::Result<TempDir> {
        // Enough uniqueness for concurrent test binaries on one machine.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(TempDir(path))
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
