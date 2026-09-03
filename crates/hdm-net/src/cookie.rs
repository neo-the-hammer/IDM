//! A cookie jar.
//!
//! Two things need this. The browser extension hands over the cookies the page
//! was using, without which many download links return a login page instead of
//! a file. And redirect chains — especially the sign-in-then-bounce-back kind
//! used by file hosts — set cookies partway through that the final request has
//! to carry.

use crate::url::Url;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Unix seconds; `None` means a session cookie.
    pub expires: Option<i64>,
    pub secure: bool,
    /// True when the cookie had no `Domain` attribute, so it matches only the
    /// exact host that set it.
    pub host_only: bool,
}

impl Cookie {
    fn matches(&self, url: &Url, now: i64) -> bool {
        if let Some(expiry) = self.expires {
            if expiry <= now {
                return false;
            }
        }
        if self.secure && !url.is_tls() {
            return false;
        }
        if !domain_matches(&url.host, &self.domain, self.host_only) {
            return false;
        }
        path_matches(&url.path, &self.path)
    }
}

/// RFC 6265 domain matching.
fn domain_matches(host: &str, domain: &str, host_only: bool) -> bool {
    let host = host.to_ascii_lowercase();
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    if host_only {
        return host == domain;
    }
    host == domain
        || (host.ends_with(&domain) && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

/// RFC 6265 path matching.
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')
}

/// The default path for a cookie set on `url`, per RFC 6265 section 5.1.4.
fn default_path(url: &Url) -> String {
    match url.path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => url.path[..i].to_string(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    pub fn new() -> CookieJar {
        CookieJar::default()
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Stores every `Set-Cookie` from a response.
    pub fn store_response(&mut self, headers: &crate::headers::Headers, url: &Url) {
        for value in headers.get_all("Set-Cookie") {
            self.store(value, url);
        }
    }

    /// Parses and stores one `Set-Cookie` value.
    pub fn store(&mut self, header: &str, url: &Url) {
        let Some(cookie) = parse_set_cookie(header, url) else {
            return;
        };
        // A later Set-Cookie replaces an earlier one with the same identity.
        self.cookies.retain(|c| {
            !(c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
        });
        // A cookie with an expiry in the past is a deletion.
        if cookie.expires.map(|e| e > now_secs()).unwrap_or(true) {
            self.cookies.push(cookie);
        }
    }

    /// Adds a raw `name=value` pair for a host, as handed over by the browser
    /// extension, which has already done the matching for us.
    pub fn add_raw(&mut self, name: &str, value: &str, url: &Url) {
        self.cookies
            .retain(|c| !(c.name == name && c.domain == url.host));
        self.cookies.push(Cookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: url.host.clone(),
            path: "/".into(),
            expires: None,
            secure: false,
            host_only: false,
        });
    }

    /// Parses a whole `Cookie:` header the browser handed us.
    pub fn add_cookie_header(&mut self, header: &str, url: &Url) {
        for pair in header.split(';') {
            if let Some((name, value)) = pair.split_once('=') {
                self.add_raw(name.trim(), value.trim(), url);
            }
        }
    }

    /// Builds the `Cookie` header value for a request, or `None` if no cookie
    /// applies.
    ///
    /// Longer paths sort first, as RFC 6265 requires.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        let now = now_secs();
        let mut matching: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.matches(url, now))
            .collect();
        if matching.is_empty() {
            return None;
        }
        matching.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        Some(
            matching
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Drops expired cookies.
    pub fn purge_expired(&mut self) {
        let now = now_secs();
        self.cookies
            .retain(|c| c.expires.map(|e| e > now).unwrap_or(true));
    }
}

fn parse_set_cookie(header: &str, url: &Url) -> Option<Cookie> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    let mut cookie = Cookie {
        name: name.to_string(),
        value: value.trim().trim_matches('"').to_string(),
        domain: url.host.clone(),
        path: default_path(url),
        expires: None,
        secure: false,
        host_only: true,
    };

    let mut max_age: Option<i64> = None;
    let mut expires: Option<i64> = None;

    for attr in parts {
        let (key, val) = match attr.split_once('=') {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().to_string()),
            None => (attr.trim().to_ascii_lowercase(), String::new()),
        };
        match key.as_str() {
            "domain" if !val.is_empty() => {
                let domain = val.trim_start_matches('.').to_ascii_lowercase();
                // A site may only widen a cookie to a domain it belongs to.
                // Without this check a response from evil.com could set a
                // cookie for example.com.
                if domain_matches(&url.host, &domain, false) && domain.contains('.') {
                    cookie.domain = domain;
                    cookie.host_only = false;
                }
            }
            "path" if val.starts_with('/') => cookie.path = val,
            "secure" => cookie.secure = true,
            "max-age" => max_age = val.parse::<i64>().ok().map(|d| now_secs() + d),
            "expires" => expires = parse_http_date(&val),
            _ => {}
        }
    }
    // Max-Age wins over Expires when both are present.
    cookie.expires = max_age.or(expires);
    Some(cookie)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parses an HTTP date into Unix seconds.
///
/// Handles the RFC 1123 form every real server uses, plus the two obsolete
/// formats RFC 7231 still requires clients to accept.
pub fn parse_http_date(value: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let cleaned = value.trim().replace(['-', ','], " ");
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    if tokens.len() < 5 {
        return None;
    }

    // Locate the time field, then read the date parts around it.
    let time_idx = tokens.iter().position(|t| t.contains(':'))?;
    let time: Vec<u32> = tokens[time_idx]
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<u32>>>()?;
    if time.len() != 3 {
        return None;
    }

    // asctime form puts the year last: "Sun Nov  6 08:49:37 1994".
    let (day, month_name, year) = if time_idx == 3 {
        (tokens[2], tokens[1], tokens.get(4)?)
    } else {
        (tokens[1], tokens[2], tokens.get(3)?)
    };

    let day: i64 = day.parse().ok()?;
    let month = MONTHS
        .iter()
        .position(|m| month_name.to_ascii_lowercase().starts_with(m))? as i64;
    let mut year: i64 = year.parse().ok()?;
    // Two-digit years, per RFC 6265's 70-year window.
    if year < 100 {
        year += if year < 70 { 2000 } else { 1900 };
    }

    Some(
        days_from_civil(year, month + 1, day) * 86400
            + time[0] as i64 * 3600
            + time[1] as i64 * 60
            + time[2] as i64,
    )
}

/// Days since 1970-01-01, by Howard Hinnant's civil-date algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
