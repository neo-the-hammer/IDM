//! Batch URL patterns.
//!
//! `http://example.com/photo[001-250].jpg` expands to 250 downloads. IDM has
//! had this for years and it is the fastest way to grab a numbered series.
//!
//! This lives in Rust rather than the Python plugin layer because it is
//! fundamental to adding downloads and must work whether or not Python is
//! installed.

/// Refuses to expand beyond this many URLs.
///
/// `[1-1000000]` is far more likely to be a typo than an intention, and
/// materialising it would exhaust memory before anyone noticed.
pub const MAX_EXPANSION: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchError {
    /// The pattern contained no `[...]` range.
    NoPattern,
    Malformed(String),
    TooLarge(usize),
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchError::NoPattern => write!(f, "no [from-to] pattern in the URL"),
            BatchError::Malformed(what) => write!(f, "cannot read the pattern `{what}`"),
            BatchError::TooLarge(n) => {
                write!(
                    f,
                    "that pattern expands to {n} URLs, more than the {MAX_EXPANSION} limit"
                )
            }
        }
    }
}

impl std::error::Error for BatchError {}

/// One `[...]` range found in a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Range {
    /// Numeric, optionally zero-padded to a fixed width.
    Number {
        from: u64,
        to: u64,
        step: u64,
        width: usize,
    },
    /// A single-letter alphabet range.
    Letter { from: char, to: char },
}

impl Range {
    fn len(&self) -> usize {
        match self {
            Range::Number { from, to, step, .. } => (((to - from) / (*step).max(1)) + 1) as usize,
            Range::Letter { from, to } => (*to as usize - *from as usize) + 1,
        }
    }

    fn value(&self, index: usize) -> String {
        match self {
            Range::Number {
                from, step, width, ..
            } => {
                let value = from + (index as u64) * (*step).max(1);
                format!("{value:0width$}")
            }
            Range::Letter { from, .. } => char::from_u32(*from as u32 + index as u32)
                .unwrap_or(*from)
                .to_string(),
        }
    }
}

/// Whether a string contains something that looks like a batch pattern.
pub fn is_pattern(input: &str) -> bool {
    parse(input)
        .map(|(_, ranges)| !ranges.is_empty())
        .unwrap_or(false)
}

/// Expands a pattern into the URLs it stands for.
///
/// Several ranges in one pattern produce every combination, with the last
/// range varying fastest — the order a person reading the pattern expects.
pub fn expand(input: &str) -> Result<Vec<String>, BatchError> {
    let (literals, ranges) = parse(input)?;
    if ranges.is_empty() {
        return Err(BatchError::NoPattern);
    }

    let total: usize = ranges.iter().map(Range::len).product();
    if total > MAX_EXPANSION {
        return Err(BatchError::TooLarge(total));
    }

    let mut out = Vec::with_capacity(total);
    let mut indices = vec![0usize; ranges.len()];
    for _ in 0..total {
        let mut url = String::with_capacity(input.len() + 8);
        for (i, literal) in literals.iter().enumerate() {
            url.push_str(literal);
            if let (Some(range), Some(index)) = (ranges.get(i), indices.get(i)) {
                url.push_str(&range.value(*index));
            }
        }
        out.push(url);

        // Odometer increment, last range fastest.
        for position in (0..ranges.len()).rev() {
            indices[position] += 1;
            if indices[position] < ranges[position].len() {
                break;
            }
            indices[position] = 0;
        }
    }
    Ok(out)
}

/// Splits a pattern into the literal text around each range, and the ranges.
///
/// `literals` always has exactly one more entry than `ranges`, so the two
/// interleave cleanly when rebuilding a URL.
fn parse(input: &str) -> Result<(Vec<String>, Vec<Range>), BatchError> {
    let mut literals = Vec::new();
    let mut ranges = Vec::new();
    let mut current = String::new();
    let mut rest = input;

    while let Some(open) = rest.find('[') {
        let Some(close_offset) = rest[open..].find(']') else {
            // An unmatched bracket is literal text, not a broken pattern:
            // brackets appear in real URLs.
            break;
        };
        let close = open + close_offset;
        let body = &rest[open + 1..close];

        match parse_range(body) {
            Some(range) => {
                current.push_str(&rest[..open]);
                literals.push(std::mem::take(&mut current));
                ranges.push(range);
                rest = &rest[close + 1..];
            }
            None => {
                // Not a range; keep it as literal text and carry on looking.
                current.push_str(&rest[..=close]);
                rest = &rest[close + 1..];
            }
        }
    }
    current.push_str(rest);
    literals.push(current);
    Ok((literals, ranges))
}

/// Reads `1-100`, `001-100`, `a-z`, or `1-100:2`.
fn parse_range(body: &str) -> Option<Range> {
    let (spec, step) = match body.split_once(':') {
        Some((spec, step)) => (spec, step.trim().parse::<u64>().ok().filter(|s| *s > 0)?),
        None => (body, 1),
    };
    let (from, to) = spec.split_once('-')?;
    let (from, to) = (from.trim(), to.trim());
    if from.is_empty() || to.is_empty() {
        return None;
    }

    if from.chars().all(|c| c.is_ascii_digit()) && to.chars().all(|c| c.is_ascii_digit()) {
        let start: u64 = from.parse().ok()?;
        let end: u64 = to.parse().ok()?;
        if end < start {
            return None;
        }
        // A leading zero means the caller wants fixed-width output.
        let width = if from.starts_with('0') && from.len() > 1 {
            from.len()
        } else {
            0
        };
        return Some(Range::Number {
            from: start,
            to: end,
            step,
            width,
        });
    }

    let mut from_chars = from.chars();
    let mut to_chars = to.chars();
    match (
        from_chars.next(),
        from_chars.next(),
        to_chars.next(),
        to_chars.next(),
    ) {
        (Some(a), None, Some(b), None)
            if a.is_ascii_alphabetic()
                && b.is_ascii_alphabetic()
                && a.is_lowercase() == b.is_lowercase()
                && a <= b =>
        {
            Some(Range::Letter { from: a, to: b })
        }
        _ => None,
    }
}
