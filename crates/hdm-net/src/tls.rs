//! TLS via the OpenSSL the operating system already ships.
//!
//! The library is loaded at run time with `dlopen` rather than linked, for two
//! reasons: Hydra builds on machines that have no `libssl-dev` installed, and a
//! binary built against `libssl.so.3` still runs where only `libssl.so.1.1`
//! exists. The cost is that every entry point has to be declared by hand, which
//! is what most of this file is.
//!
//! Windows does not use this module at all — see [`crate::transport::winhttp`].
#![cfg(unix)]
use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void, CStr, CString};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::io::AsRawFd;
use std::sync::OnceLock;
const RTLD_NOW: c_int = 2;
// OpenSSL constants, from ssl.h / x509_vfy.h.
const SSL_CTRL_SET_TLSEXT_HOSTNAME: c_int = 55;
const SSL_CTRL_SET_MIN_PROTO_VERSION: c_int = 123;
const TLSEXT_NAMETYPE_HOST_NAME: c_long = 0;
const TLS1_2_VERSION: c_long = 0x0303;
const SSL_VERIFY_NONE: c_int = 0;
const SSL_VERIFY_PEER: c_int = 1;
const X509_V_OK: c_long = 0;
const X509_CHECK_FLAG_NO_PARTIAL_WILDCARDS: c_uint = 0x4;
const SSL_ERROR_NONE: c_int = 0;
const SSL_ERROR_SSL: c_int = 1;
const SSL_ERROR_WANT_READ: c_int = 2;
const SSL_ERROR_WANT_WRITE: c_int = 3;
const SSL_ERROR_SYSCALL: c_int = 5;
const SSL_ERROR_ZERO_RETURN: c_int = 6;
/// `SSL_R_UNEXPECTED_EOF_WHILE_READING`. OpenSSL 3 raises this instead of a
/// silent EOF when a peer closes without sending `close_notify`.
const SSL_R_UNEXPECTED_EOF_WHILE_READING: c_ulong = 294;
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
/// Every OpenSSL entry point Hydra uses, resolved once at startup.
struct OpenSsl {
    tls_client_method: unsafe extern "C" fn() -> *const c_void,
    ctx_new: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    ctx_free: unsafe extern "C" fn(*mut c_void),
    ctx_set_verify: unsafe extern "C" fn(*mut c_void, c_int, *mut c_void),
    ctx_set_default_verify_paths: unsafe extern "C" fn(*mut c_void) -> c_int,
    ctx_load_verify_locations:
        unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    ctx_ctrl: unsafe extern "C" fn(*mut c_void, c_int, c_long, *mut c_void) -> c_long,
    ctx_set_alpn_protos: unsafe extern "C" fn(*mut c_void, *const c_uchar, c_uint) -> c_int,
    ssl_new: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    ssl_free: unsafe extern "C" fn(*mut c_void),
    ssl_set_fd: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    ssl_connect: unsafe extern "C" fn(*mut c_void) -> c_int,
    ssl_read: unsafe extern "C" fn(*mut c_void, *mut c_void, c_int) -> c_int,
    ssl_write: unsafe extern "C" fn(*mut c_void, *const c_void, c_int) -> c_int,
    ssl_shutdown: unsafe extern "C" fn(*mut c_void) -> c_int,
    ssl_get_error: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    ssl_ctrl: unsafe extern "C" fn(*mut c_void, c_int, c_long, *mut c_void) -> c_long,
    ssl_get0_param: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    ssl_get_verify_result: unsafe extern "C" fn(*mut c_void) -> c_long,
    ssl_get_version: unsafe extern "C" fn(*mut c_void) -> *const c_char,
    param_set1_host: unsafe extern "C" fn(*mut c_void, *const c_char, usize) -> c_int,
    param_set_hostflags: unsafe extern "C" fn(*mut c_void, c_uint),
    err_get_error: unsafe extern "C" fn() -> c_ulong,
    err_error_string_n: unsafe extern "C" fn(c_ulong, *mut c_char, usize),
    err_clear_error: unsafe extern "C" fn(),
}
// SAFETY: these are plain function pointers into a library that stays loaded
// for the life of the process, and OpenSSL 3 is internally thread-safe.
unsafe impl Send for OpenSsl {}
unsafe impl Sync for OpenSsl {}
static OPENSSL: OnceLock<Result<OpenSsl, String>> = OnceLock::new();
/// # Safety
/// `T` must be a function pointer type matching the real symbol's signature.
unsafe fn sym<T: Copy>(handle: *mut c_void, name: &str) -> Result<T, String> {
    let c = CString::new(name).map_err(|_| format!("bad symbol name {name}"))?;
    let p = dlsym(handle, c.as_ptr());
    if p.is_null() {
        return Err(format!("OpenSSL is missing the symbol `{name}`"));
    }
    Ok(*(&p as *const *mut c_void as *const T))
}
fn load() -> Result<OpenSsl, String> {
    // Newest soname first. macOS ships .dylib and, since Catalina, hides the
    // system OpenSSL, so Homebrew/MacPorts paths are tried too.
    const CRYPTO: &[&str] = &[
        "libcrypto.so.3",
        "libcrypto.so.1.1",
        "libcrypto.so",
        "libcrypto.3.dylib",
        "libcrypto.1.1.dylib",
        "libcrypto.dylib",
        "/opt/homebrew/opt/openssl@3/lib/libcrypto.3.dylib",
        "/usr/local/opt/openssl@3/lib/libcrypto.3.dylib",
    ];
    const SSL: &[&str] = &[
        "libssl.so.3",
        "libssl.so.1.1",
        "libssl.so",
        "libssl.3.dylib",
        "libssl.1.1.dylib",
        "libssl.dylib",
        "/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib",
        "/usr/local/opt/openssl@3/lib/libssl.3.dylib",
    ];
    let open_any = |names: &[&str]| -> Option<*mut c_void> {
        for n in names {
            let c = CString::new(*n).ok()?;
            let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
            if !h.is_null() {
                return Some(h);
            }
        }
        None
    };
    // libcrypto must be resolved first; libssl depends on it.
    let crypto = open_any(CRYPTO).ok_or_else(|| {
        "could not load libcrypto. Install OpenSSL 3 (Debian/Ubuntu: libssl3, \
         Fedora: openssl-libs, macOS: brew install openssl@3)."
            .to_string()
    })?;
    let ssl = open_any(SSL).ok_or_else(|| "could not load libssl".to_string())?;
    unsafe {
        Ok(OpenSsl {
            tls_client_method: sym(ssl, "TLS_client_method")?,
            ctx_new: sym(ssl, "SSL_CTX_new")?,
            ctx_free: sym(ssl, "SSL_CTX_free")?,
            ctx_set_verify: sym(ssl, "SSL_CTX_set_verify")?,
            ctx_set_default_verify_paths: sym(ssl, "SSL_CTX_set_default_verify_paths")?,
            ctx_load_verify_locations: sym(ssl, "SSL_CTX_load_verify_locations")?,
            ctx_ctrl: sym(ssl, "SSL_CTX_ctrl")?,
            ctx_set_alpn_protos: sym(ssl, "SSL_CTX_set_alpn_protos")?,
            ssl_new: sym(ssl, "SSL_new")?,
            ssl_free: sym(ssl, "SSL_free")?,
            ssl_set_fd: sym(ssl, "SSL_set_fd")?,
            ssl_connect: sym(ssl, "SSL_connect")?,
            ssl_read: sym(ssl, "SSL_read")?,
            ssl_write: sym(ssl, "SSL_write")?,
            ssl_shutdown: sym(ssl, "SSL_shutdown")?,
            ssl_get_error: sym(ssl, "SSL_get_error")?,
            ssl_ctrl: sym(ssl, "SSL_ctrl")?,
            ssl_get0_param: sym(ssl, "SSL_get0_param")?,
            ssl_get_verify_result: sym(ssl, "SSL_get_verify_result")?,
            ssl_get_version: sym(ssl, "SSL_get_version")?,
            param_set1_host: sym(crypto, "X509_VERIFY_PARAM_set1_host")?,
            param_set_hostflags: sym(crypto, "X509_VERIFY_PARAM_set_hostflags")?,
            err_get_error: sym(crypto, "ERR_get_error")?,
            err_error_string_n: sym(crypto, "ERR_error_string_n")?,
            err_clear_error: sym(crypto, "ERR_clear_error")?,
        })
    }
}
fn openssl() -> Result<&'static OpenSsl, io::Error> {
    match OPENSSL.get_or_init(load) {
        Ok(lib) => Ok(lib),
        Err(e) => Err(io::Error::new(io::ErrorKind::Unsupported, e.clone())),
    }
}
/// Drains OpenSSL's error queue, returning a readable message and the reason
/// code of each entry.
fn drain_errors(lib: &OpenSsl) -> (String, Vec<c_ulong>) {
    let mut parts = Vec::new();
    let mut reasons = Vec::new();
    loop {
        let code = unsafe { (lib.err_get_error)() };
        if code == 0 {
            break;
        }
        // ERR_GET_REASON
        reasons.push(code & 0x7F_FFFF);
        let mut buf = [0u8; 256];
        unsafe { (lib.err_error_string_n)(code, buf.as_mut_ptr() as *mut c_char, buf.len()) };
        let text = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) }
            .to_string_lossy()
            .into_owned();
        parts.push(text);
    }
    let message = if parts.is_empty() {
        "unknown TLS error".to_string()
    } else {
        parts.join("; ")
    };
    (message, reasons)
}

/// Drains the queue for a message only.
fn error_queue(lib: &OpenSsl) -> String {
    drain_errors(lib).0
}
/// How strictly to validate the server's certificate.
#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    /// Skips certificate and hostname validation entirely.
    ///
    /// This exists because IDM has the same escape hatch and users genuinely
    /// need it for appliances with self-signed certificates. It is never a
    /// default and the UI marks any download using it.
    pub insecure: bool,
    /// An extra CA bundle to trust in addition to the system store.
    pub ca_file: Option<String>,
}
/// A configured client context. Cheap to clone across download threads: OpenSSL
/// contexts are designed to be shared, with one `SSL` per connection.
pub struct TlsContext {
    ctx: *mut c_void,
    insecure: bool,
}
// SAFETY: SSL_CTX is reference-counted and safe for concurrent use by multiple
// threads once configured, which is exactly how the segment threads use it.
unsafe impl Send for TlsContext {}
unsafe impl Sync for TlsContext {}
impl TlsContext {
    pub fn new(config: &TlsConfig) -> io::Result<TlsContext> {
        let lib = openssl()?;
        unsafe {
            (lib.err_clear_error)();
            let ctx = (lib.ctx_new)((lib.tls_client_method)());
            if ctx.is_null() {
                return Err(io::Error::other(format!(
                    "SSL_CTX_new failed: {}",
                    error_queue(lib)
                )));
            }
            let guard = TlsContext {
                ctx,
                insecure: config.insecure,
            };
            // TLS 1.0 and 1.1 are deprecated and broken; refuse them outright.
            (lib.ctx_ctrl)(
                ctx,
                SSL_CTRL_SET_MIN_PROTO_VERSION,
                TLS1_2_VERSION,
                std::ptr::null_mut(),
            );
            // Advertise HTTP/1.1 only. The wire format is length-prefixed:
            // one length byte per protocol name.
            let alpn: &[u8] = b"\x08http/1.1";
            (lib.ctx_set_alpn_protos)(ctx, alpn.as_ptr(), alpn.len() as c_uint);
            if config.insecure {
                (lib.ctx_set_verify)(ctx, SSL_VERIFY_NONE, std::ptr::null_mut());
            } else {
                if (lib.ctx_set_default_verify_paths)(ctx) != 1 {
                    return Err(io::Error::other(format!(
                        "cannot load the system CA store: {}",
                        error_queue(lib)
                    )));
                }
                if let Some(path) = &config.ca_file {
                    let c = CString::new(path.as_str())
                        .map_err(|_| io::Error::other("CA path contains a NUL byte"))?;
                    if (lib.ctx_load_verify_locations)(ctx, c.as_ptr(), std::ptr::null()) != 1 {
                        return Err(io::Error::other(format!(
                            "cannot load CA file `{path}`: {}",
                            error_queue(lib)
                        )));
                    }
                }
                (lib.ctx_set_verify)(ctx, SSL_VERIFY_PEER, std::ptr::null_mut());
            }
            Ok(guard)
        }
    }
    /// Performs the handshake over an already-connected socket.
    ///
    /// `hostname` drives both SNI and certificate hostname verification, so it
    /// must be the name the user asked for — not an IP the DNS resolved to.
    pub fn connect(&self, hostname: &str, tcp: TcpStream) -> io::Result<TlsStream> {
        let lib = openssl()?;
        unsafe {
            (lib.err_clear_error)();
            let ssl = (lib.ssl_new)(self.ctx);
            if ssl.is_null() {
                return Err(io::Error::other(format!(
                    "SSL_new failed: {}",
                    error_queue(lib)
                )));
            }
            let mut stream = TlsStream { ssl, tcp, lib };
            let host = CString::new(hostname)
                .map_err(|_| io::Error::other("hostname contains a NUL byte"))?;
            // SNI. Servers behind a shared IP need this to pick a certificate.
            (lib.ssl_ctrl)(
                ssl,
                SSL_CTRL_SET_TLSEXT_HOSTNAME,
                TLSEXT_NAMETYPE_HOST_NAME,
                host.as_ptr() as *mut c_void,
            );
            if !self.insecure {
                // Hostname verification must be requested explicitly: OpenSSL
                // will happily accept a valid certificate for another site
                // otherwise.
                let param = (lib.ssl_get0_param)(ssl);
                (lib.param_set_hostflags)(param, X509_CHECK_FLAG_NO_PARTIAL_WILDCARDS);
                if (lib.param_set1_host)(param, host.as_ptr(), 0) != 1 {
                    return Err(io::Error::other("cannot set the expected hostname"));
                }
            }
            (lib.ssl_set_fd)(ssl, stream.tcp.as_raw_fd());
            let rc = (lib.ssl_connect)(ssl);
            if rc != 1 {
                let code = (lib.ssl_get_error)(ssl, rc);
                let detail = error_queue(lib);
                return Err(match code {
                    SSL_ERROR_WANT_READ | SSL_ERROR_WANT_WRITE => io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("TLS handshake with {hostname} timed out"),
                    ),
                    _ => {
                        io::Error::other(format!("TLS handshake with {hostname} failed: {detail}"))
                    }
                });
            }
            if !self.insecure {
                let result = (lib.ssl_get_verify_result)(ssl);
                if result != X509_V_OK {
                    return Err(io::Error::other(format!(
                        "certificate verification failed for {hostname} (X509 code {result})"
                    )));
                }
            }
            stream.ssl = ssl;
            Ok(stream)
        }
    }
}
impl Drop for TlsContext {
    fn drop(&mut self) {
        if let Ok(lib) = openssl() {
            unsafe { (lib.ctx_free)(self.ctx) };
        }
    }
}
/// An established TLS connection.
pub struct TlsStream {
    ssl: *mut c_void,
    tcp: TcpStream,
    lib: &'static OpenSsl,
}
// SAFETY: an SSL object is owned by exactly one TlsStream, and a TlsStream is
// only ever used from the thread that owns it.
unsafe impl Send for TlsStream {}
impl TlsStream {
    /// The negotiated protocol version, for diagnostics.
    pub fn protocol_version(&self) -> String {
        unsafe {
            let p = (self.lib.ssl_get_version)(self.ssl);
            if p.is_null() {
                "unknown".into()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }
    pub fn tcp(&self) -> &TcpStream {
        &self.tcp
    }
}
impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        unsafe {
            (self.lib.err_clear_error)();
            let n = (self.lib.ssl_read)(
                self.ssl,
                buf.as_mut_ptr() as *mut c_void,
                buf.len().min(c_int::MAX as usize) as c_int,
            );
            if n > 0 {
                return Ok(n as usize);
            }
            match (self.lib.ssl_get_error)(self.ssl, n) {
                // The peer sent close_notify: a clean end of stream.
                SSL_ERROR_ZERO_RETURN => Ok(0),
                // With a blocking socket these mean the SO_RCVTIMEO fired.
                SSL_ERROR_WANT_READ | SSL_ERROR_WANT_WRITE => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "TLS read timed out",
                )),
                SSL_ERROR_SYSCALL => {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(0) || n == 0 {
                        // A truncated stream with no close_notify. Plenty of
                        // real servers do this; report EOF and let the
                        // Content-Length check decide whether it was short.
                        Ok(0)
                    } else {
                        Err(err)
                    }
                }
                SSL_ERROR_SSL => {
                    let (message, reasons) = drain_errors(self.lib);
                    if reasons.contains(&SSL_R_UNEXPECTED_EOF_WHILE_READING) {
                        // The peer vanished without a close_notify. This is
                        // ordinary on the public internet, so report it as end
                        // of stream and let the HTTP layer's Content-Length
                        // check decide whether the body was actually short.
                        // Treating it as a protocol failure here would turn
                        // every complete-but-untidy download into an error.
                        return Ok(0);
                    }
                    Err(io::Error::other(format!("TLS read failed: {message}")))
                }
                SSL_ERROR_NONE => Ok(0),
                other => Err(io::Error::other(format!("TLS read failed (code {other})"))),
            }
        }
    }
}
impl Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        unsafe {
            (self.lib.err_clear_error)();
            let n = (self.lib.ssl_write)(
                self.ssl,
                buf.as_ptr() as *const c_void,
                buf.len().min(c_int::MAX as usize) as c_int,
            );
            if n > 0 {
                return Ok(n as usize);
            }
            match (self.lib.ssl_get_error)(self.ssl, n) {
                SSL_ERROR_WANT_READ | SSL_ERROR_WANT_WRITE => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "TLS write timed out",
                )),
                SSL_ERROR_ZERO_RETURN => Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "TLS connection closed while writing",
                )),
                SSL_ERROR_SYSCALL => {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(0) {
                        Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "TLS write failed",
                        ))
                    } else {
                        Err(err)
                    }
                }
                _ => Err(io::Error::other(format!(
                    "TLS write failed: {}",
                    error_queue(self.lib)
                ))),
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}
impl Drop for TlsStream {
    fn drop(&mut self) {
        unsafe {
            // One non-blocking attempt at close_notify. We deliberately do not
            // wait for the peer's reply: a hung server must not stall the
            // thread that is tearing down a cancelled download.
            (self.lib.ssl_shutdown)(self.ssl);
            (self.lib.ssl_free)(self.ssl);
        }
    }
}
/// Reports whether a usable OpenSSL is present, for a clear startup diagnostic.
pub fn availability() -> Result<(), String> {
    match OPENSSL.get_or_init(load) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.clone()),
    }
}
