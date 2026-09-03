//! Punycode (RFC 3492) for internationalized domain names.
//!
//! Hydra ships a Persian locale, and Persian and Arabic script domains are in
//! real use, so a URL like `https://سایت.ir/file.zip` has to resolve. DNS only
//! carries ASCII, so each non-ASCII label is encoded to its `xn--` form before
//! the connection is made.

const BASE: u32 = 36;
const TMIN: u32 = 1;
const TMAX: u32 = 26;
const SKEW: u32 = 38;
const DAMP: u32 = 700;
const INITIAL_BIAS: u32 = 72;
const INITIAL_N: u32 = 128;
const DELIMITER: char = '-';

fn adapt(mut delta: u32, num_points: u32, first_time: bool) -> u32 {
    delta /= if first_time { DAMP } else { 2 };
    delta += delta / num_points;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
}

fn digit_to_char(d: u32) -> char {
    // 0..=25 map to 'a'..='z', 26..=35 map to '0'..='9'.
    if d < 26 {
        (b'a' + d as u8) as char
    } else {
        (b'0' + (d - 26) as u8) as char
    }
}

/// Encodes one label's Unicode content as Punycode (without the `xn--` prefix).
fn encode_label(input: &str) -> Option<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut output: String = chars.iter().filter(|c| c.is_ascii()).collect();
    let basic_len = output.chars().count() as u32;
    if basic_len > 0 {
        output.push(DELIMITER);
    }

    let mut n = INITIAL_N;
    let mut delta: u32 = 0;
    let mut bias = INITIAL_BIAS;
    let mut handled = basic_len;
    let total = chars.len() as u32;

    while handled < total {
        // Smallest code point not yet handled.
        let m = chars.iter().map(|&c| c as u32).filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(handled + 1)?)?;
        n = m;

        for &c in &chars {
            let cp = c as u32;
            if cp < n {
                delta = delta.checked_add(1)?;
            } else if cp == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    output.push(digit_to_char(t + ((q - t) % (BASE - t))));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                output.push(digit_to_char(q));
                bias = adapt(delta, handled + 1, handled == basic_len);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(output)
}

/// Converts a hostname to its ASCII (DNS-safe) form.
///
/// Labels that are already ASCII pass through untouched, so this is a no-op for
/// the overwhelming majority of URLs.
pub fn host_to_ascii(host: &str) -> Option<String> {
    if host.is_ascii() {
        return Some(host.to_ascii_lowercase());
    }
    let mut out = Vec::new();
    for label in host.split('.') {
        if label.is_ascii() {
            out.push(label.to_ascii_lowercase());
        } else {
            // Lowercase first: IDNA case-folds, and DNS is case-insensitive.
            let folded = label.to_lowercase();
            let encoded = encode_label(&folded)?;
            let label = format!("xn--{encoded}");
            // DNS labels are limited to 63 octets.
            if label.len() > 63 {
                return None;
            }
            out.push(label);
        }
    }
    Some(out.join("."))
}
