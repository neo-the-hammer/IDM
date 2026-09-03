//! An ordered, case-insensitive header collection.

use std::fmt;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    pub fn new() -> Headers {
        Headers(Vec::new())
    }

    /// Returns the first value for `name`, matched case-insensitively.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every value for `name`. `Set-Cookie` in particular repeats.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Sets `name`, replacing any existing values.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.remove(&name);
        self.0.push((name, value.into()));
    }

    /// Adds a value without disturbing existing ones.
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.push((name.into(), value.into()));
    }

    /// Adds `name` only if it is not already present, so caller-supplied
    /// headers always win over Hydra's defaults.
    pub fn set_if_absent(&mut self, name: &str, value: impl Into<String>) {
        if !self.contains(name) {
            self.0.push((name.to_string(), value.into()));
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.0.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Parses `name: value`, rejecting anything malformed.
    ///
    /// A header line containing a CR, LF or NUL would allow response splitting,
    /// so those are refused rather than sanitized.
    pub fn parse_line(&mut self, line: &str) -> Result<(), String> {
        let colon = line
            .find(':')
            .ok_or_else(|| format!("header without a colon: {line:?}"))?;
        let name = line[..colon].trim_end();
        let value = line[colon + 1..].trim();
        if name.is_empty() {
            return Err("header with an empty name".into());
        }
        if !name.bytes().all(is_token_byte) {
            return Err(format!("invalid character in header name {name:?}"));
        }
        if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
            return Err("control character in header value".into());
        }
        self.0.push((name.to_string(), value.to_string()));
        Ok(())
    }

    /// Serializes to wire format, including the terminating blank line.
    pub fn write_to(&self, out: &mut String) {
        for (k, v) in &self.0 {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
    }
}

/// RFC 7230 `tchar`.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Rejects values that would let a caller inject extra headers.
pub fn is_safe_header_value(value: &str) -> bool {
    !value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

impl fmt::Display for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (k, v) in &self.0 {
            writeln!(f, "{k}: {v}")?;
        }
        Ok(())
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Headers {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Headers {
        Headers(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}
