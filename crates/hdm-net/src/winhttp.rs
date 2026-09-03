//! The Windows transport, built on WinHTTP.
//!
//! Windows ships no OpenSSL, so the Unix path in [`crate::tls`] cannot be used
//! here. The alternative to this module is a hand-written Schannel/SSPI backend
//! — roughly four times the code, all of it intricate token-loop handling, and
//! none of it verifiable outside Windows. WinHTTP is the same stack the system
//! already trusts and brings the certificate store, the system proxy including
//! PAC autoconfiguration, and HTTP/2 with it.
//!
//! Redirects, cookies and authentication are deliberately disabled here and
//! handled by [`crate::client`] instead, so those behaviours are identical on
//! every platform and are covered by the tests that run on Unix.
//!
//! **This module has never been compiled.** It was written in a Linux container
//! with no Windows Rust target available. It is confined behind `#[cfg]` so it
//! cannot affect the tested paths, and the first Windows build is a real
//! checkpoint.

#![cfg(windows)]

use crate::headers::Headers;
use crate::http::Request;
use std::ffi::{c_void, OsStr};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

type Handle = *mut c_void;

// WinHTTP constants, from winhttp.h.
const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const ACCESS_TYPE_NO_PROXY: u32 = 1;
const ACCESS_TYPE_NAMED_PROXY: u32 = 3;
const FLAG_SECURE: u32 = 0x0080_0000;
const FLAG_REFRESH: u32 = 0x0000_0100;
const QUERY_STATUS_CODE: u32 = 19;
const QUERY_STATUS_TEXT: u32 = 20;
const QUERY_RAW_HEADERS_CRLF: u32 = 22;
const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;
const ADDREQ_FLAG_ADD: u32 = 0x2000_0000;
const ADDREQ_FLAG_REPLACE: u32 = 0x8000_0000;
const OPTION_SECURITY_FLAGS: u32 = 31;
const OPTION_DISABLE_FEATURE: u32 = 63;
const DISABLE_REDIRECTS: u32 = 0x0000_0002;
const DISABLE_COOKIES: u32 = 0x0000_0001;
const DISABLE_AUTHENTICATION: u32 = 0x0000_0004;
const SECURITY_FLAG_IGNORE_UNKNOWN_CA: u32 = 0x0000_0100;
const SECURITY_FLAG_IGNORE_CERT_WRONG_USAGE: u32 = 0x0000_0200;
const SECURITY_FLAG_IGNORE_CERT_CN_INVALID: u32 = 0x0000_1000;
const SECURITY_FLAG_IGNORE_CERT_DATE_INVALID: u32 = 0x0000_2000;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(
        agent: *const u16,
        access_type: u32,
        proxy: *const u16,
        bypass: *const u16,
        flags: u32,
    ) -> Handle;
    fn WinHttpConnect(session: Handle, host: *const u16, port: u16, reserved: u32) -> Handle;
    fn WinHttpOpenRequest(
        connect: Handle,
        verb: *const u16,
        object: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept_types: *const *const u16,
        flags: u32,
    ) -> Handle;
    fn WinHttpAddRequestHeaders(
        request: Handle,
        headers: *const u16,
        length: u32,
        modifiers: u32,
    ) -> i32;
    fn WinHttpSendRequest(
        request: Handle,
        headers: *const u16,
        headers_length: u32,
        optional: *const c_void,
        optional_length: u32,
        total_length: u32,
        context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(request: Handle, reserved: *mut c_void) -> i32;
    fn WinHttpQueryHeaders(
        request: Handle,
        info_level: u32,
        name: *const u16,
        buffer: *mut c_void,
        buffer_length: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn WinHttpReadData(
        request: Handle,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
    ) -> i32;
    fn WinHttpSetOption(
        handle: Handle,
        option: u32,
        buffer: *const c_void,
        buffer_length: u32,
    ) -> i32;
    fn WinHttpSetTimeouts(
        handle: Handle,
        resolve: i32,
        connect: i32,
        send: i32,
        receive: i32,
    ) -> i32;
    fn WinHttpCloseHandle(handle: Handle) -> i32;
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

/// An owned WinHTTP handle, closed on drop.
struct OwnedHandle(Handle);

// SAFETY: WinHTTP handles may be used from any thread; only concurrent use of
// the *same* request handle is disallowed, and each is owned by one thread.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

/// A request handle that another thread can close to abort a transfer.
///
/// Closing the handle makes a blocked `WinHttpReadData` return immediately,
/// which is how pause and cancel stay responsive.
pub struct RequestHandle(Arc<OwnedHandle>);

impl RequestHandle {
    pub fn abort(&self) {
        if !self.0 .0.is_null() {
            unsafe { WinHttpCloseHandle(self.0 .0) };
        }
    }
}

/// Session-wide configuration.
#[derive(Clone, Default)]
pub struct WinHttpConfig {
    pub insecure: bool,
    /// `host:port`, or `None` to use the system proxy configuration.
    pub proxy: Option<String>,
    pub proxy_bypass: Option<String>,
    pub connect_timeout: Option<Duration>,
    pub read_timeout: Option<Duration>,
}

/// The body of a WinHTTP response.
///
/// WinHTTP has already applied transfer decoding, so unlike the Unix path there
/// is no chunked framing to unpick here.
pub struct WinHttpBody {
    request: Arc<OwnedHandle>,
    #[allow(dead_code)]
    connect: OwnedHandle,
    #[allow(dead_code)]
    session: OwnedHandle,
    finished: bool,
}

impl WinHttpBody {
    pub fn abort_handle(&self) -> RequestHandle {
        RequestHandle(self.request.clone())
    }
}

impl io::Read for WinHttpBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.finished || buf.is_empty() {
            return Ok(0);
        }
        let mut read: u32 = 0;
        let ok = unsafe {
            WinHttpReadData(
                self.request.0,
                buf.as_mut_ptr() as *mut c_void,
                buf.len().min(u32::MAX as usize) as u32,
                &mut read,
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        if read == 0 {
            self.finished = true;
        }
        Ok(read as usize)
    }
}

/// What a WinHTTP exchange produced, before it becomes a [`crate::http::Response`].
pub struct WinHttpExchange {
    pub status: u16,
    pub reason: String,
    pub headers: Headers,
    pub body: WinHttpBody,
}

/// Performs one request. Redirects and authentication are the caller's job.
pub fn execute(request: &Request, config: &WinHttpConfig) -> io::Result<WinHttpExchange> {
    let url = &request.url;

    let (access_type, proxy_wide, bypass_wide) = match &config.proxy {
        Some(proxy) => (
            ACCESS_TYPE_NAMED_PROXY,
            Some(wide(proxy)),
            config.proxy_bypass.as_deref().map(wide),
        ),
        // Automatic honours WPAD and the user's LAN settings, which is what
        // makes Hydra work unattended on a corporate network.
        None => (ACCESS_TYPE_AUTOMATIC_PROXY, None, None),
    };

    let agent = wide(crate::http::USER_AGENT);
    let session = unsafe {
        WinHttpOpen(
            agent.as_ptr(),
            access_type,
            proxy_wide.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
            bypass_wide.as_ref().map_or(ptr::null(), |b| b.as_ptr()),
            0,
        )
    };
    if session.is_null() {
        // Automatic proxy detection is unavailable before Windows 8.1; fall
        // back to a direct connection rather than failing outright.
        let session = unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                ACCESS_TYPE_NO_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        };
        if session.is_null() {
            return Err(last_error());
        }
        return execute_on_session(OwnedHandle(session), request, config);
    }
    execute_on_session(OwnedHandle(session), request, config)
}

fn execute_on_session(
    session: OwnedHandle,
    request: &Request,
    config: &WinHttpConfig,
) -> io::Result<WinHttpExchange> {
    let url = &request.url;

    let millis = |d: Option<Duration>, default: i32| -> i32 {
        d.map(|d| d.as_millis().min(i32::MAX as u128) as i32)
            .unwrap_or(default)
    };
    unsafe {
        WinHttpSetTimeouts(
            session.0,
            30_000,
            millis(config.connect_timeout, 30_000),
            millis(config.read_timeout, 60_000),
            millis(config.read_timeout, 60_000),
        );
    }

    let host = wide(&url.host);
    let connect = unsafe { WinHttpConnect(session.0, host.as_ptr(), url.effective_port(), 0) };
    if connect.is_null() {
        return Err(last_error());
    }
    let connect = OwnedHandle(connect);

    let verb = wide(&request.method);
    let target = wide(&url.request_target());
    let mut flags = FLAG_REFRESH;
    if url.is_tls() {
        flags |= FLAG_SECURE;
    }
    let handle = unsafe {
        WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            target.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            flags,
        )
    };
    if handle.is_null() {
        return Err(last_error());
    }
    let request_handle = Arc::new(OwnedHandle(handle));

    // Redirects, cookies and authentication are handled by the shared client so
    // that every platform behaves the same way and the Unix tests cover both.
    let disable = DISABLE_REDIRECTS | DISABLE_COOKIES | DISABLE_AUTHENTICATION;
    unsafe {
        WinHttpSetOption(
            request_handle.0,
            OPTION_DISABLE_FEATURE,
            &disable as *const u32 as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }

    if config.insecure {
        // The same explicit escape hatch the Unix backend offers, for
        // appliances with self-signed certificates.
        let ignore = SECURITY_FLAG_IGNORE_UNKNOWN_CA
            | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
            | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID
            | SECURITY_FLAG_IGNORE_CERT_WRONG_USAGE;
        unsafe {
            WinHttpSetOption(
                request_handle.0,
                OPTION_SECURITY_FLAGS,
                &ignore as *const u32 as *const c_void,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }

    // Headers go in one block; WinHTTP supplies Host and Content-Length itself.
    let mut header_block = String::new();
    for (name, value) in request.headers.iter() {
        if name.eq_ignore_ascii_case("Host") || name.eq_ignore_ascii_case("Content-Length") {
            continue;
        }
        header_block.push_str(name);
        header_block.push_str(": ");
        header_block.push_str(value);
        header_block.push_str("\r\n");
    }
    if !header_block.is_empty() {
        let block = wide(&header_block);
        let ok = unsafe {
            WinHttpAddRequestHeaders(
                request_handle.0,
                block.as_ptr(),
                u32::MAX,
                ADDREQ_FLAG_ADD | ADDREQ_FLAG_REPLACE,
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
    }

    let body = request.body.as_deref().unwrap_or(&[]);
    let ok = unsafe {
        WinHttpSendRequest(
            request_handle.0,
            ptr::null(),
            0,
            if body.is_empty() {
                ptr::null()
            } else {
                body.as_ptr() as *const c_void
            },
            body.len() as u32,
            body.len() as u32,
            0,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    if unsafe { WinHttpReceiveResponse(request_handle.0, ptr::null_mut()) } == 0 {
        return Err(last_error());
    }

    let status = query_number(request_handle.0, QUERY_STATUS_CODE)?;
    let reason = query_string(request_handle.0, QUERY_STATUS_TEXT).unwrap_or_default();
    let raw = query_string(request_handle.0, QUERY_RAW_HEADERS_CRLF).unwrap_or_default();

    let mut headers = Headers::new();
    // The first line is the status line, which is not a header.
    for line in raw.split("\r\n").skip(1) {
        if line.is_empty() {
            continue;
        }
        let _ = headers.parse_line(line);
    }

    Ok(WinHttpExchange {
        status: status as u16,
        reason,
        headers,
        body: WinHttpBody {
            request: request_handle,
            connect,
            session,
            finished: false,
        },
    })
}

fn query_number(request: Handle, level: u32) -> io::Result<u32> {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let ok = unsafe {
        WinHttpQueryHeaders(
            request,
            level | QUERY_FLAG_NUMBER,
            ptr::null(),
            &mut value as *mut u32 as *mut c_void,
            &mut size,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(value)
}

fn query_string(request: Handle, level: u32) -> Option<String> {
    // Ask with a zero-length buffer to learn the size, then allocate.
    let mut size: u32 = 0;
    unsafe {
        WinHttpQueryHeaders(
            request,
            level,
            ptr::null(),
            ptr::null_mut(),
            &mut size,
            ptr::null_mut(),
        )
    };
    if size == 0 {
        return None;
    }
    if io::Error::last_os_error().raw_os_error().map(|e| e as u32)
        != Some(ERROR_INSUFFICIENT_BUFFER)
    {
        return None;
    }

    let mut buffer = vec![0u16; (size as usize / 2) + 1];
    let ok = unsafe {
        WinHttpQueryHeaders(
            request,
            level,
            ptr::null(),
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    let chars = size as usize / 2;
    Some(String::from_utf16_lossy(&buffer[..chars.min(buffer.len())]))
}
