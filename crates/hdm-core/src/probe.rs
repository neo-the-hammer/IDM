//! Working out what is at the other end of a link before downloading it.

use hdm_net::client::Client;
use hdm_net::http::Request;
use hdm_net::url::Url;
use std::io;

/// What the server told us about a resource.
#[derive(Debug, Clone)]
pub struct Probe {
    /// The URL after redirects. Segments must all use this one, or they can
    /// land on different mirrors and interleave two different files.
    pub final_url: Url,
    /// `None` when the server did not say — a chunked or streaming response.
    pub total: Option<u64>,
    pub supports_ranges: bool,
    pub filename: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub status: u16,
}

impl Probe {
    /// Whether this resource can be split across several connections.
    pub fn can_segment(&self) -> bool {
        self.supports_ranges && self.total.map(|t| t > 0).unwrap_or(false)
    }
}

/// Asks the server about `url`.
///
/// A `HEAD` is tried first because it is free, but a surprising number of
/// servers — CDNs with signed URLs, PHP download scripts, some object stores —
/// answer `HEAD` with 405, or with a 200 whose headers do not match what a
/// `GET` would return. So a single-byte ranged `GET` is the fallback, and it is
/// also the more trustworthy answer: a `206` response is direct proof that
/// range requests work, where `Accept-Ranges` is only a claim.
pub fn probe(client: &Client, url: &Url) -> io::Result<Probe> {
    if let Ok(probe) = probe_with_head(client, url) {
        if probe.status < 400 && probe.total.is_some() {
            return Ok(probe);
        }
    }
    probe_with_range(client, url)
}

fn probe_with_head(client: &Client, url: &Url) -> io::Result<Probe> {
    let fetch = client.send(Request::head(url.clone()))?;
    let response = &fetch.response;
    Ok(Probe {
        total: response.content_length(),
        supports_ranges: response.accepts_ranges(),
        filename: response
            .filename(&fetch.final_url)
            .unwrap_or_else(|| fallback_filename(&fetch.final_url)),
        etag: response.etag(),
        last_modified: response.last_modified(),
        content_type: response.content_type().map(str::to_string),
        status: response.status,
        final_url: fetch.final_url,
    })
}

fn probe_with_range(client: &Client, url: &Url) -> io::Result<Probe> {
    let request = Request::get(url.clone()).with_range(0, Some(0));
    let fetch = client.send(request)?;
    let response = &fetch.response;

    // A 206 proves ranges work and carries the true total in Content-Range.
    // A 200 means the server ignored Range and is about to send the whole file.
    let supports_ranges = response.is_partial();
    let total = response.total_size();

    let probe = Probe {
        total,
        supports_ranges,
        filename: response
            .filename(&fetch.final_url)
            .unwrap_or_else(|| fallback_filename(&fetch.final_url)),
        etag: response.etag(),
        last_modified: response.last_modified(),
        content_type: response.content_type().map(str::to_string),
        status: response.status,
        final_url: fetch.final_url,
    };
    // Nothing else needs this connection, and a 200 here would otherwise stream
    // the entire file into a socket we are about to drop.
    response.shutdown();
    Ok(probe)
}

/// A last-resort name for a URL that offers none, such as `https://host/`.
fn fallback_filename(url: &Url) -> String {
    let from_host = url.host.replace('.', "_");
    if from_host.is_empty() {
        "download".to_string()
    } else {
        format!("{from_host}_download")
    }
}
