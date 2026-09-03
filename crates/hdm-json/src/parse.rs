//! A strict RFC 8259 parser.
//!
//! Strict on purpose: this parses input from browser extensions, plugin
//! subprocesses and the network, so trailing commas, comments, `NaN` and
//! unquoted keys are all rejected rather than guessed at.

use crate::Json;
use std::fmt;

/// The maximum container nesting depth.
///
/// The parser is recursive, so without a limit a hostile payload of 100k open
/// brackets would overflow the stack and abort the process. 128 is far beyond
/// anything Hydra legitimately exchanges.
pub const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub message: String,
    /// Byte offset where parsing failed.
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at line {} column {}",
            self.message, self.line, self.column
        )
    }
}

impl std::error::Error for Error {}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: usize,
}

/// Parses a complete JSON document. Trailing content after the top-level value
/// is an error.
pub fn parse(input: &str) -> Result<Json, Error> {
    let mut p = Parser {
        src: input.as_bytes(),
        pos: 0,
        depth: 0,
    };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos < p.src.len() {
        return Err(p.err("trailing characters after JSON value"));
    }
    Ok(value)
}

impl<'a> Parser<'a> {
    fn err(&self, message: impl Into<String>) -> Error {
        // Recompute line/column only on the error path; it never costs us
        // anything during successful parses.
        let (mut line, mut column) = (1usize, 1usize);
        for &b in &self.src[..self.pos.min(self.src.len())] {
            if b == b'\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        Error {
            message: message.into(),
            offset: self.pos,
            line,
            column,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn eat(&mut self, b: u8) -> Result<(), Error> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(format!("expected `{}`", b as char)))
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, Error> {
        if self.src[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn value(&mut self) -> Result<Json, Error> {
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(c) => Err(self.err(format!("unexpected character `{}`", c as char))),
        }
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err(format!("nesting deeper than {MAX_DEPTH} levels")));
        }
        Ok(())
    }

    fn array(&mut self) -> Result<Json, Error> {
        self.enter()?;
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(self.err("expected `,` or `]`")),
            }
        }
    }

    fn object(&mut self) -> Result<Json, Error> {
        self.enter()?;
        self.pos += 1; // consume '{'
        let mut pairs: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("expected a string key"));
            }
            let key = self.string()?;
            self.skip_ws();
            self.eat(b':')?;
            self.skip_ws();
            let value = self.value()?;
            // Last duplicate key wins, matching JavaScript's own semantics.
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key, value)),
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    self.depth -= 1;
                    return Ok(Json::Obj(pairs));
                }
                _ => return Err(self.err("expected `,` or `}`")),
            }
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1; // consume opening quote
        let mut out = String::new();
        loop {
            let b = self.peek().ok_or_else(|| self.err("unterminated string"))?;
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let esc = self.peek().ok_or_else(|| self.err("unterminated escape"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.err("invalid escape sequence")),
                    }
                }
                // Raw control characters are not permitted inside strings.
                0x00..=0x1f => return Err(self.err("unescaped control character in string")),
                _ => {
                    // Copy one whole UTF-8 sequence. The input is a &str, so it
                    // is already valid UTF-8 and we only need the length.
                    let len = utf8_len(b);
                    let end = (self.pos + len).min(self.src.len());
                    out.push_str(
                        std::str::from_utf8(&self.src[self.pos..end])
                            .map_err(|_| self.err("invalid UTF-8 in string"))?,
                    );
                    self.pos = end;
                }
            }
        }
    }

    /// Reads the four hex digits of a `\u` escape, joining surrogate pairs.
    fn unicode_escape(&mut self) -> Result<char, Error> {
        let first = self.hex4()?;
        // Lone low surrogates are invalid; high surrogates must be followed by
        // a `\uDC00`-`\uDFFF` escape to form a single scalar value.
        if (0xD800..0xDC00).contains(&first) {
            if self.peek() != Some(b'\\') {
                return Err(self.err("high surrogate not followed by a low surrogate"));
            }
            self.pos += 1;
            if self.peek() != Some(b'u') {
                return Err(self.err("high surrogate not followed by a low surrogate"));
            }
            self.pos += 1;
            let second = self.hex4()?;
            if !(0xDC00..0xE000).contains(&second) {
                return Err(self.err("invalid low surrogate"));
            }
            let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
            return char::from_u32(combined).ok_or_else(|| self.err("invalid code point"));
        }
        if (0xDC00..0xE000).contains(&first) {
            return Err(self.err("unpaired low surrogate"));
        }
        char::from_u32(first).ok_or_else(|| self.err("invalid code point"))
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        if self.pos + 4 > self.src.len() {
            return Err(self.err("truncated \\u escape"));
        }
        let mut value = 0u32;
        for _ in 0..4 {
            let d = self.src[self.pos];
            let digit = match d {
                b'0'..=b'9' => d - b'0',
                b'a'..=b'f' => d - b'a' + 10,
                b'A'..=b'F' => d - b'A' + 10,
                _ => return Err(self.err("invalid hex digit in \\u escape")),
            };
            value = value * 16 + digit as u32;
            self.pos += 1;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, Error> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // Integer part: either a lone `0` or a non-zero digit run. Leading
        // zeros ("01") are invalid JSON.
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.err("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("expected a digit after the decimal point"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("expected a digit in the exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| self.err("invalid number"))?;
        let n: f64 = text.parse().map_err(|_| self.err("number out of range"))?;
        if !n.is_finite() {
            return Err(self.err("number overflows to infinity"));
        }
        Ok(Json::Num(n))
    }
}

/// Length in bytes of the UTF-8 sequence starting with `first`.
fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}
