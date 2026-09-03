//! The high-level client: redirects, authentication, cookies and proxying.

use crate::auth::{basic_header, select_challenge, Challenge, Credentials, DigestState};
use crate::cookie::CookieJar;
use crate::headers::Headers;
use crate::http::{Request, Response, MAX_REDIRECTS};
use crate::proxy::Proxy;
use crate::stream::{Stream, Timeouts};
use crate::url::Url;
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct ClientConfig {
    pub timeouts: Timeouts,
    #[cfg(unix)]
    pub tls: crate::tls::TlsConfig,
    pub proxy: Option<Proxy>,
    /// Skips certificate validation. On Unix this mirrors `tls.insecure`; on
    /// Windows it is the only place the flag lives.
    pub tls_insecure: bool,
    /// Credentials to offer when the server asks for them.
    pub credentials: Option<Credentials>,
    /// Extra headers applied to every request. The browser extension uses this
    /// to replay the exact `Referer` and `User-Agent` the page was using, which
    /// is what makes hotlink-protected links work.
    pub extra_headers: Headers,
    pub max_redirects: usize,
}

impl ClientConfig {
    pub fn new() -> ClientConfig {
        ClientConfig {
            max_redirects: MAX_REDIRECTS,
            ..Default::default()
        }
    }
}

/// A response together with the URL it was finally served from.
#[derive(Debug)]
pub struct Fetch {
    pub response: Response,
    /// The URL after any redirects — this is what segment requests must use,
    /// so that every connection lands on the same resource.
    pub final_url: Url,
}

pub struct Client {
    config: ClientConfig,
    #[cfg(unix)]
    tls: Option<Arc<crate::tls::TlsContext>>,
    cookies: Arc<Mutex<CookieJar>>,
}

impl Client {
    pub fn new(config: ClientConfig) -> io::Result<Client> {
        #[cfg(unix)]
        let tls = Some(Arc::new(crate::tls::TlsContext::new(&config.tls)?));
        Ok(Client {
            config,
            #[cfg(unix)]
            tls,
            cookies: Arc::new(Mutex::new(CookieJar::new())),
        })
    }

    /// A client that shares this one's TLS context and cookie jar.
    ///
    /// Every segment thread of a download gets one of these: the TLS context is
    /// expensive to build and safe to share, and the cookie jar must be common
    /// so a session cookie set on one connection applies to the rest.
    pub fn share(&self) -> Client {
        Client {
            config: self.config.clone(),
            #[cfg(unix)]
            tls: self.tls.clone(),
            cookies: self.cookies.clone(),
        }
    }

    pub fn cookies(&self) -> &Mutex<CookieJar> {
        &self.cookies
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    #[cfg(unix)]
    fn tls_ref(&self) -> Option<&crate::tls::TlsContext> {
        self.tls.as_deref()
    }

    #[cfg(not(unix))]
    fn tls_ref(&self) -> Option<&crate::stream::TlsContextRef> {
        None
    }

    /// Sends one request with no redirect or authentication handling.
    pub fn execute(&self, mut request: Request) -> io::Result<Response> {
        for (k, v) in self.config.extra_headers.iter() {
            request.headers.set_if_absent(k, v);
        }
        if let Some(header) = self.cookies.lock().unwrap().header_for(&request.url) {
            request.headers.set_if_absent("Cookie", header);
        }

        let response = self.execute_transport(&mut request)?;
        self.cookies
            .lock()
            .unwrap()
            .store_response(&response.headers, &request.url);
        Ok(response)
    }

    /// Performs one exchange on whichever transport this platform uses.
    #[cfg(not(windows))]
    fn execute_transport(&self, request: &mut Request) -> io::Result<Response> {
        let use_proxy = self
            .config
            .proxy
            .as_ref()
            .filter(|p| !p.bypasses(&request.url.host));

        let (mut stream, absolute_target) = match use_proxy {
            Some(proxy) => proxy.connect(&request.url, &self.config.timeouts, self.tls_ref())?,
            None => (
                Stream::connect(&request.url, &self.config.timeouts, self.tls_ref())?,
                false,
            ),
        };

        let method = request.method.clone();
        // Ranged reads hold the connection for a long time, so there is nothing
        // to gain from keep-alive and a dedicated connection per segment is
        // simpler to reason about.
        request.write_to(&mut stream, false, absolute_target)?;
        Response::read(stream, &method)
    }

    /// Windows goes through WinHTTP, which supplies the TLS stack, the system
    /// certificate store and the system proxy configuration. Everything above
    /// this call -- redirects, cookies, authentication, segmentation -- is the
    /// same code on every platform.
    #[cfg(windows)]
    fn execute_transport(&self, request: &mut Request) -> io::Result<Response> {
        use crate::winhttp::{self, WinHttpConfig};

        // Apply the same default headers the socket path would.
        request.prepare_headers();

        let proxy = self
            .config
            .proxy
            .as_ref()
            .filter(|p| !p.bypasses(&request.url.host));
        let config = WinHttpConfig {
            insecure: self.config.tls_insecure,
            proxy: proxy.map(|p| format!("{}:{}", p.host, p.port)),
            proxy_bypass: proxy
                .filter(|p| !p.bypass.is_empty())
                .map(|p| p.bypass.join(";")),
            connect_timeout: Some(self.config.timeouts.connect),
            read_timeout: Some(self.config.timeouts.read),
        };
        Ok(Response::from_winhttp(winhttp::execute(request, &config)?))
    }

    /// Sends a request, following redirects and answering authentication
    /// challenges.
    pub fn send(&self, request: Request) -> io::Result<Fetch> {
        let mut request = request;
        let mut digest = DigestState::default();
        let mut visited: Vec<String> = Vec::new();
        let mut auth_attempts = 0;

        visited.push(request.url.to_string_safe());
        // Redirects and authentication retries share this loop, so the budget
        // covers both; only redirects count towards loop detection.
        for _ in 0..=self.config.max_redirects + 2 {
            let response = self.execute(request.clone())?;

            if response.is_redirect() {
                let Some(target) = response.location(&request.url) else {
                    return Err(io::Error::other(
                        "redirect without a usable Location header",
                    ));
                };
                // Drop the body so the redirect does not leave bytes unread.
                response.shutdown();

                let same_origin = target.host == request.url.host
                    && target.scheme == request.url.scheme
                    && target.effective_port() == request.url.effective_port();
                if !same_origin {
                    // Credentials are scoped to the origin that asked for them.
                    // Carrying an Authorization header across a redirect would
                    // hand the password to whatever host the server names.
                    request.headers.remove("Authorization");
                    request.headers.remove("Cookie");
                    digest = DigestState::default();
                }
                // 303, and 301/302 on a non-GET, become a GET (RFC 7231 6.4).
                let rewrite_to_get = matches!(response.status, 301..=303)
                    && request.method != "GET"
                    && request.method != "HEAD";
                if rewrite_to_get {
                    request.method = "GET".into();
                    request.body = None;
                    request.headers.remove("Content-Length");
                    request.headers.remove("Content-Type");
                }
                let key = target.to_string_safe();
                if visited.contains(&key) {
                    return Err(io::Error::other(format!("redirect loop at {key}")));
                }
                if visited.len() > self.config.max_redirects {
                    return Err(io::Error::other(format!(
                        "more than {} redirects",
                        self.config.max_redirects
                    )));
                }
                visited.push(key);
                request.url = target;
                continue;
            }

            // Answer an authentication challenge once per scheme.
            if (response.status == 401 || response.status == 407) && auth_attempts < 2 {
                let (challenge_header, response_header) = if response.status == 401 {
                    ("WWW-Authenticate", "Authorization")
                } else {
                    ("Proxy-Authenticate", "Proxy-Authorization")
                };
                let credentials = self.credentials_for(&request.url, response.status);
                if let (Some(challenge), Some(creds)) = (
                    select_challenge(&response.headers, challenge_header),
                    credentials,
                ) {
                    let value = match &challenge {
                        Challenge::Basic { .. } => basic_header(&creds),
                        Challenge::Digest(d) => {
                            digest.header(d, &creds, &request.method, &request.url.request_target())
                        }
                    };
                    response.shutdown();
                    request.headers.set(response_header, value);
                    auth_attempts += 1;
                    continue;
                }
            }

            let final_url = request.url.clone();
            return Ok(Fetch {
                response,
                final_url,
            });
        }
        Err(io::Error::other(format!(
            "more than {} redirects",
            self.config.max_redirects
        )))
    }

    /// Credentials for a request: those embedded in the URL take priority over
    /// the client-wide ones.
    fn credentials_for(&self, url: &Url, status: u16) -> Option<Credentials> {
        if status == 407 {
            return self
                .config
                .proxy
                .as_ref()
                .and_then(|p| p.credentials.clone());
        }
        if !url.username.is_empty() {
            return Some(Credentials {
                username: url.username.clone(),
                password: url.password.clone(),
            });
        }
        self.config.credentials.clone()
    }
}
