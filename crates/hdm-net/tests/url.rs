use hdm_net::{percent_decode_str, percent_encode, Url};

#[test]
fn parses_a_full_url() {
    let u = Url::parse("https://user:pw@example.com:8443/a/b.iso?x=1&y=2#frag").unwrap();
    assert_eq!(u.scheme, "https");
    assert_eq!(u.username, "user");
    assert_eq!(u.password, "pw");
    assert_eq!(u.host, "example.com");
    assert_eq!(u.port, Some(8443));
    assert_eq!(u.path, "/a/b.iso");
    assert_eq!(u.query.as_deref(), Some("x=1&y=2"));
    assert_eq!(u.fragment.as_deref(), Some("frag"));
}

#[test]
fn defaults_the_port_per_scheme() {
    assert_eq!(Url::parse("http://a.com/").unwrap().effective_port(), 80);
    assert_eq!(Url::parse("https://a.com/").unwrap().effective_port(), 443);
    assert_eq!(Url::parse("ftp://a.com/").unwrap().effective_port(), 21);
    assert_eq!(Url::parse("ftps://a.com/").unwrap().effective_port(), 443);
    assert_eq!(
        Url::parse("http://a.com:8080/").unwrap().effective_port(),
        8080
    );
}

#[test]
fn normalizes_scheme_and_host_case() {
    let u = Url::parse("HTTPS://Example.COM/Path").unwrap();
    assert_eq!(u.scheme, "https");
    assert_eq!(u.host, "example.com");
    assert_eq!(
        u.path, "/Path",
        "the path is case-sensitive and must not be folded"
    );
}

#[test]
fn supplies_a_root_path_when_absent() {
    assert_eq!(Url::parse("http://a.com").unwrap().path, "/");
    assert_eq!(Url::parse("http://a.com?q=1").unwrap().path, "/");
    assert_eq!(Url::parse("http://a.com#f").unwrap().path, "/");
}

#[test]
fn handles_ipv6_literals() {
    let u = Url::parse("http://[2001:db8::1]:8080/f").unwrap();
    assert!(u.is_ipv6);
    assert_eq!(u.host, "2001:db8::1");
    assert_eq!(u.port, Some(8080));
    assert_eq!(u.host_header(), "[2001:db8::1]:8080");
    assert_eq!(Url::parse("http://[::1]/").unwrap().host_header(), "[::1]");
}

#[test]
fn host_header_omits_the_default_port() {
    assert_eq!(
        Url::parse("http://a.com:80/").unwrap().host_header(),
        "a.com"
    );
    assert_eq!(
        Url::parse("https://a.com:443/").unwrap().host_header(),
        "a.com"
    );
    assert_eq!(
        Url::parse("http://a.com:8080/").unwrap().host_header(),
        "a.com:8080"
    );
}

#[test]
fn userinfo_splits_on_the_last_at_sign() {
    // A password may legitimately contain '@'.
    let u = Url::parse("ftp://bob:p@ss@files.example.com/x").unwrap();
    assert_eq!(u.username, "bob");
    assert_eq!(u.password, "p@ss");
    assert_eq!(u.host, "files.example.com");
}

#[test]
fn percent_encoded_userinfo_is_decoded() {
    let u = Url::parse("ftp://a%40b:p%3Aw@h.com/").unwrap();
    assert_eq!(u.username, "a@b");
    assert_eq!(u.password, "p:w");
}

/// Credentials must never leak into logs, the UI or saved state.
#[test]
fn display_hides_credentials_unless_explicitly_requested() {
    let u = Url::parse("https://user:secret@example.com/f.iso").unwrap();
    let shown = u.to_string();
    assert!(!shown.contains("secret"), "password leaked: {shown}");
    assert!(!shown.contains("user"), "username leaked: {shown}");
    assert_eq!(shown, "https://example.com/f.iso");
    assert!(u.to_string_with_credentials().contains("secret"));
}

#[test]
fn resolves_dot_segments() {
    for (input, want) in [
        ("http://a.com/x/./y", "/x/y"),
        ("http://a.com/x/../y", "/y"),
        ("http://a.com/x/y/../../z", "/z"),
        ("http://a.com/../../etc/passwd", "/etc/passwd"),
        ("http://a.com/x//y", "/x/y"),
        ("http://a.com/x/", "/x/"),
        ("http://a.com/x/.", "/x/"),
    ] {
        assert_eq!(Url::parse(input).unwrap().path, want, "for {input}");
    }
}

#[test]
fn rejects_malformed_urls() {
    for bad in [
        "",
        "   ",
        "no-scheme",
        "http:/only-one-slash",
        "://a.com",
        "1http://a.com/",
        "http://",
        "http:///path",
        "http://a.com:99999/",
        "http://a.com:abc/",
        "http://[::1/",
    ] {
        assert!(Url::parse(bad).is_err(), "expected {bad:?} to be rejected");
    }
}

// ------------------------------------------------------------------- join

#[test]
fn join_resolves_rfc3986_examples() {
    let base = Url::parse("http://a/b/c/d;p?q").unwrap();
    for (reference, want) in [
        ("g", "http://a/b/c/g"),
        ("./g", "http://a/b/c/g"),
        ("g/", "http://a/b/c/g/"),
        ("/g", "http://a/g"),
        ("//g", "http://g/"),
        ("?y", "http://a/b/c/d;p?y"),
        ("g?y", "http://a/b/c/g?y"),
        ("#s", "http://a/b/c/d;p?q#s"),
        ("../g", "http://a/b/g"),
        ("../../g", "http://a/g"),
        ("../../../g", "http://a/g"),
        ("", "http://a/b/c/d;p?q"),
        ("http://other/x", "http://other/x"),
    ] {
        let got = base.join(reference).unwrap().to_string();
        assert_eq!(got, want, "base.join({reference:?})");
    }
}

#[test]
fn join_carries_the_scheme_for_protocol_relative_references() {
    let base = Url::parse("https://a.com/p").unwrap();
    let joined = base.join("//cdn.example.com/f.iso").unwrap();
    assert_eq!(joined.scheme, "https");
    assert_eq!(joined.host, "cdn.example.com");
}

/// A hostile `Location` must not be able to walk above the site root.
#[test]
fn join_cannot_escape_the_root() {
    let base = Url::parse("http://a.com/x/y").unwrap();
    assert_eq!(
        base.join("../../../../etc/passwd").unwrap().path,
        "/etc/passwd"
    );
}

// -------------------------------------------------------------------- IDN

#[test]
fn internationalized_hosts_become_punycode() {
    // The RFC 3492 sample: Bücher.de
    assert_eq!(
        Url::parse("http://bücher.de/").unwrap().host,
        "xn--bcher-kva.de"
    );
    // Persian, which the UI's fa locale makes a realistic case.
    let u = Url::parse("https://سایت.ir/فایل.zip").unwrap();
    assert!(u.host.starts_with("xn--"), "host not encoded: {}", u.host);
    assert!(u.host.ends_with(".ir"), "ASCII label mangled: {}", u.host);
    // Mixed labels: only the non-ASCII one is encoded.
    let mixed = Url::parse("http://www.مثال.com/").unwrap();
    assert!(mixed.host.starts_with("www.xn--"), "got {}", mixed.host);
}

#[test]
fn ascii_hosts_are_untouched_by_idn_handling() {
    assert_eq!(
        Url::parse("http://plain.example.com/").unwrap().host,
        "plain.example.com"
    );
}

// ------------------------------------------------------------- components

#[test]
fn request_target_joins_path_and_query() {
    assert_eq!(
        Url::parse("http://a/b?c=1").unwrap().request_target(),
        "/b?c=1"
    );
    assert_eq!(Url::parse("http://a/b").unwrap().request_target(), "/b");
}

#[test]
fn filename_comes_from_the_last_path_segment() {
    assert_eq!(
        Url::parse("http://a/x/y/file.iso")
            .unwrap()
            .filename()
            .as_deref(),
        Some("file.iso")
    );
    assert_eq!(Url::parse("http://a/x/").unwrap().filename(), None);
    assert_eq!(Url::parse("http://a/").unwrap().filename(), None);
    // Percent-encoded names are decoded for display.
    assert_eq!(
        Url::parse("http://a/my%20file%20(1).zip")
            .unwrap()
            .filename()
            .as_deref(),
        Some("my file (1).zip")
    );
    assert_eq!(
        Url::parse("http://a/%D9%81%D8%A7%DB%8C%D9%84.zip")
            .unwrap()
            .filename()
            .as_deref(),
        Some("فایل.zip")
    );
}

#[test]
fn percent_coding_round_trips() {
    for raw in [
        "plain",
        "a b",
        "a/b?c=d&e",
        "فایل.zip",
        "100%",
        "a+b",
        "~-._",
    ] {
        let encoded = percent_encode(raw, "");
        assert_eq!(percent_decode_str(&encoded), raw, "round trip of {raw:?}");
    }
    assert_eq!(percent_encode("a b", ""), "a%20b");
    assert_eq!(
        percent_encode("~-._a1", ""),
        "~-._a1",
        "unreserved set is untouched"
    );
}

#[test]
fn percent_decode_keeps_invalid_escapes_literal() {
    // A bare '%' in a filename must survive rather than eat the next characters.
    assert_eq!(percent_decode_str("100%"), "100%");
    assert_eq!(percent_decode_str("a%zz"), "a%zz");
    assert_eq!(percent_decode_str("a%2"), "a%2");
}
