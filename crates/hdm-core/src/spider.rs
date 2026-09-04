//! The site grabber.
//!
//! Walks a site and collects the files on it. The division of labour is
//! deliberate: **Rust fetches, Python parses**. Keeping the fetching here means
//! cookies, authentication, proxies and TLS all behave exactly as they do for a
//! normal download, instead of being reimplemented in the plugin layer.

use crate::plugins::PluginHost;
use hdm_json::{json, Json};
use hdm_net::client::{Client, ClientConfig};
use hdm_net::http::Request;
use hdm_net::url::Url;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Largest page the crawler will read. A page bigger than this is not a page.
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct CrawlOptions {
    /// How many links deep to follow. 0 means the starting page only.
    pub max_depth: u32,
    /// Stop after visiting this many pages.
    pub max_pages: usize,
    /// Stop after finding this many files.
    pub max_files: usize,
    /// Never *navigate* to another host.
    pub same_host: bool,
    /// Only collect files served by the starting host.
    ///
    /// Off by default, and deliberately separate from `same_host`: a site's
    /// documents very often live on a CDN or an assets subdomain, and dropping
    /// them because the hostname differs would quietly lose most of what the
    /// user asked for. Restricting where the crawler *goes* is a different
    /// question from restricting what it *collects*.
    pub files_same_host: bool,
    /// Never rise above the starting directory.
    pub stay_under_path: bool,
    /// Only collect these extensions. Empty means every extension.
    pub include_extensions: Vec<String>,
    pub exclude_extensions: Vec<String>,
    /// Wait this long between requests, so a crawl does not look like an attack.
    pub delay: Duration,
    /// Honour the site's `robots.txt`.
    pub respect_robots: bool,
    /// Give up on the whole crawl after this long.
    pub deadline: Duration,
}

impl Default for CrawlOptions {
    fn default() -> Self {
        CrawlOptions {
            max_depth: 1,
            max_pages: 100,
            max_files: 1000,
            same_host: true,
            files_same_host: false,
            stay_under_path: true,
            include_extensions: Vec::new(),
            exclude_extensions: Vec::new(),
            // Fast enough to be useful, slow enough to be a guest rather than a
            // load test.
            delay: Duration::from_millis(250),
            respect_robots: true,
            deadline: Duration::from_secs(300),
        }
    }
}

impl CrawlOptions {
    pub fn from_json(value: &Json) -> CrawlOptions {
        let list = |key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(Json::as_arr)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        CrawlOptions {
            // Depth is capped: each level multiplies the work, and an
            // accidental 20 would crawl for hours.
            max_depth: value.u64_or("depth", 1).min(8) as u32,
            max_pages: value.u64_or("maxPages", 100).min(5_000) as usize,
            max_files: value.u64_or("maxFiles", 1000).min(20_000) as usize,
            same_host: value.bool_or("sameHost", true),
            files_same_host: value.bool_or("filesSameHost", false),
            stay_under_path: value.bool_or("stayUnderPath", true),
            include_extensions: list("include"),
            exclude_extensions: list("exclude"),
            delay: Duration::from_millis(value.u64_or("delayMs", 250).min(10_000)),
            respect_robots: value.bool_or("respectRobots", true),
            deadline: Duration::from_secs(value.u64_or("timeoutSeconds", 300).clamp(5, 3600)),
        }
    }
}

/// A file the crawl found.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundFile {
    pub url: String,
    pub filename: String,
    pub extension: String,
    /// The page it was linked from, which becomes the download's Referer.
    pub found_on: String,
    /// The link's text, which is often a better title than the filename.
    pub text: String,
}

impl FoundFile {
    pub fn to_json(&self) -> Json {
        json!({
            "url": (self.url.as_str()),
            "filename": (self.filename.as_str()),
            "extension": (self.extension.as_str()),
            "foundOn": (self.found_on.as_str()),
            "text": (self.text.as_str())
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct CrawlResult {
    pub files: Vec<FoundFile>,
    pub pages_visited: usize,
    /// Pages that could not be read, with the reason.
    pub errors: Vec<String>,
    /// True when a limit stopped the crawl before it ran out of links.
    pub truncated: bool,
}

impl CrawlResult {
    pub fn to_json(&self) -> Json {
        json!({
            "files": (Json::Arr(self.files.iter().map(FoundFile::to_json).collect())),
            "pagesVisited": (self.pages_visited as u64),
            "errors": (Json::Arr(self.errors.iter().map(|e| Json::Str(e.clone())).collect())),
            "truncated": (self.truncated)
        })
    }
}

/// Crawls `start` and returns the files it finds.
pub fn crawl(start: &str, options: &CrawlOptions) -> Result<CrawlResult, String> {
    let host = PluginHost::discover()
        .map_err(|e| format!("the site grabber needs Python for page parsing, but {e}"))?;

    let root = Url::parse(start).map_err(|e| format!("invalid URL: {e}"))?;
    let client = Client::new(ClientConfig::new()).map_err(|e| e.to_string())?;

    let robots = if options.respect_robots {
        Robots::fetch(&client, &root)
    } else {
        Robots::permissive()
    };

    // The directory the crawl is anchored to, when it must stay under it.
    let root_prefix = match root.path.rfind('/') {
        Some(index) => root.path[..=index].to_string(),
        None => "/".to_string(),
    };

    let mut result = CrawlResult::default();
    let mut queue: VecDeque<(Url, u32)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut found: HashSet<String> = HashSet::new();
    queue.push_back((root.clone(), 0));
    visited.insert(root.to_string_safe());

    let started = Instant::now();
    let mut first = true;

    while let Some((page, depth)) = queue.pop_front() {
        if result.pages_visited >= options.max_pages || result.files.len() >= options.max_files {
            result.truncated = true;
            break;
        }
        if started.elapsed() > options.deadline {
            result
                .errors
                .push(format!("stopped after {:?}", options.deadline));
            result.truncated = true;
            break;
        }

        // Politeness delay, but not before the very first request.
        if !first && !options.delay.is_zero() {
            std::thread::sleep(options.delay);
        }
        first = false;

        let html = match fetch_page(&client, &page) {
            Ok(Some(html)) => html,
            // Not a page: nothing to parse, and not an error worth reporting.
            Ok(None) => continue,
            Err(e) => {
                result
                    .errors
                    .push(format!("{}: {e}", page.to_string_safe()));
                continue;
            }
        };
        result.pages_visited += 1;

        let parsed = match host.links(&page.to_string_safe(), &html) {
            Ok(parsed) => parsed,
            Err(e) => {
                result
                    .errors
                    .push(format!("{}: {e}", page.to_string_safe()));
                continue;
            }
        };
        let Some(links) = parsed.get("links").and_then(Json::as_arr) else {
            continue;
        };

        for link in links {
            let Some(url_text) = link.get("url").and_then(Json::as_str) else {
                continue;
            };
            let Ok(url) = Url::parse(url_text) else {
                continue;
            };

            let extension = link
                .get("filename")
                .and_then(Json::as_str)
                .and_then(|name| name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()))
                .unwrap_or_default();

            if options.files_same_host && url.host != root.host {
                continue;
            }
            if wanted(&extension, options) {
                let canonical = url.to_string_safe();
                if found.insert(canonical.clone()) && result.files.len() < options.max_files {
                    result.files.push(FoundFile {
                        filename: link.str_or("filename", "").to_string(),
                        extension,
                        url: canonical,
                        found_on: page.to_string_safe(),
                        text: link.str_or("text", "").to_string(),
                    });
                }
                continue;
            }

            // Otherwise consider it as somewhere to go next.
            if depth >= options.max_depth || !link.bool_or("navigation", false) {
                continue;
            }
            if !may_follow(&url, &root, &root_prefix, options, &robots) {
                continue;
            }
            let canonical = url.to_string_safe();
            if visited.insert(canonical) {
                queue.push_back((url, depth + 1));
            }
        }
    }

    if !queue.is_empty() {
        result.truncated = true;
    }
    Ok(result)
}

/// Whether an extension is one the caller asked for.
fn wanted(extension: &str, options: &CrawlOptions) -> bool {
    if extension.is_empty() {
        return false;
    }
    if options.exclude_extensions.iter().any(|e| e == extension) {
        return false;
    }
    if !options.include_extensions.is_empty() {
        return options.include_extensions.iter().any(|e| e == extension);
    }
    // With no include list, anything that is not obviously another page counts.
    !matches!(
        extension,
        "html" | "htm" | "xhtml" | "shtml" | "php" | "asp" | "aspx" | "jsp" | "cgi"
    )
}

fn may_follow(
    url: &Url,
    root: &Url,
    root_prefix: &str,
    options: &CrawlOptions,
    robots: &Robots,
) -> bool {
    if !matches!(url.scheme.as_str(), "http" | "https") {
        return false;
    }
    if options.same_host && url.host != root.host {
        return false;
    }
    if options.stay_under_path && url.host == root.host && !url.path.starts_with(root_prefix) {
        return false;
    }
    if options.respect_robots && url.host == root.host && !robots.allows(&url.path) {
        return false;
    }
    true
}

/// Fetches a page, returning `None` when it is not HTML.
fn fetch_page(client: &Client, url: &Url) -> Result<Option<String>, String> {
    let mut fetch = client
        .send(Request::get(url.clone()))
        .map_err(|e| e.to_string())?;

    if fetch.response.status >= 400 {
        return Err(format!("the server answered {}", fetch.response.status));
    }
    let is_html = fetch
        .response
        .content_type()
        .map(|t| t.contains("html") || t.contains("xml"))
        // A server that says nothing might still be serving HTML.
        .unwrap_or(true);
    if !is_html {
        fetch.response.shutdown();
        return Ok(None);
    }

    fetch
        .response
        .read_to_string(MAX_PAGE_BYTES)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// The subset of `robots.txt` that matters for a crawler like this.
///
/// A site grabber that ignores `robots.txt` will get its user rate-limited or
/// banned, so honouring it is the default. Only the `User-agent: *` group is
/// read, and only `Disallow` and `Allow` within it — which is what a site is
/// actually expressing when it publishes one.
struct Robots {
    disallow: Vec<String>,
    allow: Vec<String>,
}

impl Robots {
    fn permissive() -> Robots {
        Robots {
            disallow: Vec::new(),
            allow: Vec::new(),
        }
    }

    fn fetch(client: &Client, root: &Url) -> Robots {
        let mut robots_url = root.clone();
        robots_url.path = "/robots.txt".into();
        robots_url.query = None;
        robots_url.fragment = None;

        let Ok(mut fetch) = client.send(Request::get(robots_url)) else {
            return Robots::permissive();
        };
        if fetch.response.status != 200 {
            fetch.response.shutdown();
            return Robots::permissive();
        }
        match fetch.response.read_to_string(512 * 1024) {
            Ok(text) => Robots::parse(&text),
            // An unreadable robots.txt is treated as absent rather than as a
            // blanket ban; the site clearly is not relying on it.
            Err(_) => Robots::permissive(),
        }
    }

    fn parse(text: &str) -> Robots {
        let mut robots = Robots::permissive();
        let mut in_wildcard_group = false;
        let mut seen_agent_in_group = false;

        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            let field = field.trim().to_ascii_lowercase();
            let value = value.trim();

            match field.as_str() {
                "user-agent" => {
                    // Consecutive User-agent lines share one group of rules.
                    if !seen_agent_in_group {
                        in_wildcard_group = false;
                    }
                    if value == "*" {
                        in_wildcard_group = true;
                    }
                    seen_agent_in_group = true;
                }
                "disallow" if in_wildcard_group => {
                    seen_agent_in_group = false;
                    // An empty Disallow means "nothing is disallowed".
                    if !value.is_empty() {
                        robots.disallow.push(value.to_string());
                    }
                }
                "allow" if in_wildcard_group => {
                    seen_agent_in_group = false;
                    if !value.is_empty() {
                        robots.allow.push(value.to_string());
                    }
                }
                _ => seen_agent_in_group = false,
            }
        }
        robots
    }

    /// The longest matching rule wins, with `Allow` beating `Disallow` on ties,
    /// which is how every major crawler resolves overlapping rules.
    fn allows(&self, path: &str) -> bool {
        let longest = |rules: &[String]| -> usize {
            rules
                .iter()
                .filter(|rule| path.starts_with(rule.as_str()))
                .map(|rule| rule.len())
                .max()
                .unwrap_or(0)
        };
        longest(&self.allow) >= longest(&self.disallow)
    }
}
