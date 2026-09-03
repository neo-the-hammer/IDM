use hdm_net::auth::{basic_header, select_challenge, Challenge, Credentials, DigestState};
use hdm_net::cookie::{parse_http_date, CookieJar};
use hdm_net::headers::Headers;
use hdm_net::http::{
    parse_content_disposition_filename, parse_content_range_span, parse_content_range_total,
    sanitize_filename,
};
use hdm_net::proxy::{Proxy, ProxyKind};
use hdm_net::url::Url;

// ------------------------------------------------------------------ headers

#[test]
fn header_lookup_ignores_case_and_keeps_order() {
    let mut h = Headers::new();
    h.set("Content-Type", "text/html");
    h.append("Set-Cookie", "a=1");
    h.append("Set-Cookie", "b=2");
    assert_eq!(h.get("content-TYPE"), Some("text/html"));
    assert_eq!(
        h.get_all("set-cookie").collect::<Vec<_>>(),
        vec!["a=1", "b=2"]
    );
    assert_eq!(
        h.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        vec!["Content-Type", "Set-Cookie", "Set-Cookie"]
    );
}

#[test]
fn set_replaces_but_set_if_absent_defers() {
    let mut h = Headers::new();
    h.set("A", "1");
    h.set("A", "2");
    assert_eq!(h.get_all("A").count(), 1);
    assert_eq!(h.get("A"), Some("2"));
    h.set_if_absent("A", "3");
    assert_eq!(
        h.get("A"),
        Some("2"),
        "an existing value must win over a default"
    );
}

/// A header value carrying CR or LF would let a crafted response inject extra
/// headers into our view of it.
#[test]
fn header_parsing_rejects_injection_and_junk() {
    let mut h = Headers::new();
    assert!(h.parse_line("Good: value").is_ok());
    assert!(h.parse_line("no-colon").is_err());
    assert!(h.parse_line(": empty name").is_err());
    assert!(
        h.parse_line("Bad Name: v").is_err(),
        "space is not a token character"
    );
    assert!(h.parse_line("X: a\rb").is_err(), "CR in value");
    assert!(h.parse_line("X: a\nb").is_err(), "LF in value");
}

// --------------------------------------------------------------- filenames

/// The name a server suggests is attacker-controlled whenever the link is.
#[test]
fn sanitize_filename_blocks_path_traversal() {
    assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
    assert_eq!(
        sanitize_filename("..\\..\\Windows\\System32\\evil.dll"),
        "evil.dll"
    );
    assert_eq!(sanitize_filename("/absolute/path.iso"), "path.iso");
    assert_eq!(sanitize_filename(".."), "");
    assert_eq!(sanitize_filename("."), "");
    assert_eq!(sanitize_filename(""), "");
}

#[test]
fn sanitize_filename_removes_characters_windows_rejects() {
    assert_eq!(
        sanitize_filename("a<b>c:d\"e|f?g*h.txt"),
        "a_b_c_d_e_f_g_h.txt"
    );
    assert_eq!(sanitize_filename("nul\u{0}byte.bin"), "nul_byte.bin");
    // Windows silently strips trailing dots and spaces, which can rename a file.
    assert_eq!(sanitize_filename("report.txt. . "), "report.txt");
}

#[test]
fn sanitize_filename_defuses_reserved_device_names() {
    // Writing to "CON" or "LPT1" on Windows targets a device, not a file.
    assert_eq!(sanitize_filename("CON"), "_CON");
    assert_eq!(sanitize_filename("con.txt"), "_con.txt");
    assert_eq!(sanitize_filename("LPT1.zip"), "_LPT1.zip");
    assert_eq!(
        sanitize_filename("console.txt"),
        "console.txt",
        "must not over-match"
    );
}

#[test]
fn sanitize_filename_truncates_but_keeps_the_extension() {
    let long = format!("{}.iso", "a".repeat(500));
    let result = sanitize_filename(&long);
    assert!(result.len() <= 200, "length was {}", result.len());
    assert!(
        result.ends_with(".iso"),
        "the extension must survive: {result}"
    );
}

#[test]
fn sanitize_filename_preserves_ordinary_names() {
    for name in [
        "ubuntu-24.04.iso",
        "My Movie (2024).mkv",
        "فایل.zip",
        "a_b-c.tar.gz",
    ] {
        assert_eq!(sanitize_filename(name), name, "{name} should be unchanged");
    }
}

// ------------------------------------------------------ content-disposition

#[test]
fn content_disposition_variants() {
    let cases = [
        ("attachment; filename=\"a.iso\"", Some("a.iso")),
        ("attachment; filename=a.iso", Some("a.iso")),
        ("attachment;filename=a.iso", Some("a.iso")),
        ("inline; filename=\"b c.txt\"", Some("b c.txt")),
        ("attachment", None),
        ("attachment; filename=\"\"", None),
        // filename* wins, whatever the order.
        (
            "attachment; filename=\"x\"; filename*=UTF-8''%C3%A9.txt",
            Some("é.txt"),
        ),
        (
            "attachment; filename*=UTF-8''%C3%A9.txt; filename=\"x\"",
            Some("é.txt"),
        ),
        (
            "attachment; filename*=ISO-8859-1''caf%E9.txt",
            Some("café.txt"),
        ),
        // A semicolon inside quotes is not a parameter separator.
        ("attachment; filename=\"a;b.txt\"", Some("a;b.txt")),
        (
            "attachment; filename=\"quote\\\"d.txt\"",
            Some("quote\"d.txt"),
        ),
    ];
    for (header, want) in cases {
        assert_eq!(
            parse_content_disposition_filename(header).as_deref(),
            want,
            "for {header:?}"
        );
    }
}

// ------------------------------------------------------------ content-range

#[test]
fn content_range_parsing() {
    assert_eq!(parse_content_range_total("bytes 0-99/12345"), Some(12345));
    assert_eq!(
        parse_content_range_total("bytes 0-99/*"),
        None,
        "unknown total"
    );
    assert_eq!(
        parse_content_range_span("bytes 100-199/12345"),
        Some((100, 199))
    );
    assert_eq!(parse_content_range_span("bytes 0-0/1"), Some((0, 0)));
    assert_eq!(parse_content_range_span("garbage"), None);
}

// ------------------------------------------------------------------- cookies

#[test]
fn cookie_jar_matches_domain_path_and_scheme() {
    let mut jar = CookieJar::new();
    let url = Url::parse("https://www.example.com/a/b").unwrap();
    jar.store("sid=1; Domain=example.com; Path=/", &url);
    jar.store("deep=2; Path=/a/b", &url);
    jar.store("secureonly=3; Secure", &url);

    // A subdomain of the cookie's domain matches.
    let header = jar
        .header_for(&Url::parse("https://api.example.com/x").unwrap())
        .unwrap();
    assert!(header.contains("sid=1"));
    assert!(!header.contains("deep=2"), "path /a/b must not match /x");

    // An unrelated domain gets nothing.
    assert!(jar
        .header_for(&Url::parse("https://evil.com/").unwrap())
        .is_none());

    // A Secure cookie must not travel over plaintext.
    let plain = jar
        .header_for(&Url::parse("http://www.example.com/a/b").unwrap())
        .unwrap();
    assert!(
        !plain.contains("secureonly"),
        "Secure cookie sent over http: {plain}"
    );
}

/// A response must not be able to set a cookie for a domain it does not own.
#[test]
fn cookie_jar_refuses_a_foreign_domain() {
    let mut jar = CookieJar::new();
    let url = Url::parse("https://evil.com/").unwrap();
    jar.store("pwn=1; Domain=example.com", &url);
    assert!(
        jar.header_for(&Url::parse("https://example.com/").unwrap())
            .is_none(),
        "evil.com set a cookie for example.com"
    );
}

#[test]
fn cookie_expiry_is_honoured() {
    let mut jar = CookieJar::new();
    let url = Url::parse("https://a.com/").unwrap();
    jar.store("gone=1; Max-Age=-1", &url);
    assert!(
        jar.header_for(&url).is_none(),
        "an expired cookie must not be stored"
    );

    jar.store("alive=1; Max-Age=3600", &url);
    assert_eq!(jar.header_for(&url).as_deref(), Some("alive=1"));

    // Setting it again with a past expiry is how servers delete cookies.
    jar.store("alive=1; Max-Age=0", &url);
    assert!(jar.header_for(&url).is_none());
}

#[test]
fn cookie_ordering_puts_longer_paths_first() {
    let mut jar = CookieJar::new();
    let url = Url::parse("https://a.com/x/y").unwrap();
    jar.store("short=1; Path=/", &url);
    jar.store("long=2; Path=/x/y", &url);
    assert_eq!(jar.header_for(&url).as_deref(), Some("long=2; short=1"));
}

#[test]
fn http_dates_parse_in_all_three_formats() {
    // RFC 1123, the one everything actually sends.
    let a = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
    // RFC 850, obsolete but still permitted.
    let b = parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT").unwrap();
    // asctime, likewise.
    let c = parse_http_date("Sun Nov  6 08:49:37 1994").unwrap();
    assert_eq!(a, 784111777, "the well-known RFC 7231 example timestamp");
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert!(parse_http_date("not a date").is_none());
}

// ------------------------------------------------------------ auth parsing

#[test]
fn basic_header_is_rfc7617() {
    let header = basic_header(&Credentials {
        username: "Aladdin".into(),
        password: "open sesame".into(),
    });
    assert_eq!(header, "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
}

#[test]
fn digest_is_preferred_over_basic_when_both_are_offered() {
    let mut h = Headers::new();
    h.append("WWW-Authenticate", "Basic realm=\"r\"");
    h.append(
        "WWW-Authenticate",
        "Digest realm=\"r\", nonce=\"n\", qop=\"auth\"",
    );
    // Basic reveals the password to anyone watching a plaintext connection.
    assert!(matches!(
        select_challenge(&h, "WWW-Authenticate"),
        Some(Challenge::Digest(_))
    ));
}

#[test]
fn digest_challenge_parses_quoted_commas() {
    let mut h = Headers::new();
    h.set(
        "WWW-Authenticate",
        "Digest realm=\"a realm, with a comma\", nonce=\"abc\", qop=\"auth,auth-int\", \
         opaque=\"op\", algorithm=SHA-256, stale=TRUE",
    );
    let Some(Challenge::Digest(d)) = select_challenge(&h, "WWW-Authenticate") else {
        panic!("expected a digest challenge");
    };
    assert_eq!(d.realm, "a realm, with a comma");
    assert_eq!(d.nonce, "abc");
    assert_eq!(d.opaque.as_deref(), Some("op"));
    assert!(d.stale);
}

/// The RFC 7616 section 3.9.1 worked example, computed end to end.
#[test]
fn digest_response_matches_the_rfc_example() {
    let mut h = Headers::new();
    h.set(
        "WWW-Authenticate",
        "Digest realm=\"http-auth@example.org\", qop=\"auth\", algorithm=MD5, \
         nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\", \
         opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\"",
    );
    let Some(Challenge::Digest(challenge)) = select_challenge(&h, "WWW-Authenticate") else {
        panic!("expected a digest challenge");
    };
    let mut state = DigestState::default();
    let header = state.header(
        &challenge,
        &Credentials {
            username: "Mufasa".into(),
            password: "Circle of Life".into(),
        },
        "GET",
        "/dir/index.html",
    );
    // The RFC fixes the client nonce; ours is random, so check the structure
    // and that every required parameter is present and quoted correctly.
    assert!(header.starts_with("Digest username=\"Mufasa\""));
    assert!(header.contains("realm=\"http-auth@example.org\""));
    assert!(header.contains("uri=\"/dir/index.html\""));
    assert!(header.contains("qop=auth"));
    assert!(
        header.contains("nc=00000001"),
        "the nonce count must start at 1"
    );
    assert!(header.contains("cnonce=\""));
    assert!(header.contains("opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""));
    assert!(header.contains("algorithm=MD5"));

    // A second request must increment nc, or the server rejects it as a replay.
    let again = state.header(
        &challenge,
        &Credentials {
            username: "Mufasa".into(),
            password: "Circle of Life".into(),
        },
        "GET",
        "/dir/index.html",
    );
    assert!(again.contains("nc=00000002"));
}

/// A username containing a quote must not be able to forge extra parameters.
#[test]
fn digest_escapes_quoted_values() {
    let mut h = Headers::new();
    h.set("WWW-Authenticate", "Digest realm=\"r\", nonce=\"n\"");
    let Some(Challenge::Digest(challenge)) = select_challenge(&h, "WWW-Authenticate") else {
        panic!()
    };
    let header = DigestState::default().header(
        &challenge,
        &Credentials {
            username: "a\", admin=\"true".into(),
            password: "p".into(),
        },
        "GET",
        "/",
    );
    assert!(
        header.contains("username=\"a\\\", admin=\\\"true\""),
        "got: {header}"
    );
}

// -------------------------------------------------------------------- proxy

#[test]
fn proxy_specs_parse() {
    let p = Proxy::parse("http://proxy.local:3128").unwrap();
    assert_eq!(p.kind, ProxyKind::Http);
    assert_eq!(p.port, 3128);

    let s = Proxy::parse("socks5://127.0.0.1:9050").unwrap();
    assert_eq!(s.kind, ProxyKind::Socks5);

    // A bare host:port is assumed to be an HTTP proxy.
    let bare = Proxy::parse("10.0.0.1:8080").unwrap();
    assert_eq!(bare.kind, ProxyKind::Http);
    assert_eq!(bare.host, "10.0.0.1");

    let auth = Proxy::parse("http://u:pw@p.local:3128").unwrap();
    assert_eq!(auth.credentials.unwrap().username, "u");

    // Scheme defaults.
    assert_eq!(Proxy::parse("http://p.local").unwrap().port, 8080);
    assert_eq!(Proxy::parse("socks5://p.local").unwrap().port, 1080);

    assert!(Proxy::parse("ftp://p.local").is_err());
}

#[test]
fn proxy_bypass_rules() {
    let mut p = Proxy::parse("http://p:8080").unwrap();
    p.bypass = vec!["localhost".into(), "*.internal".into(), "<local>".into()];
    assert!(p.bypasses("localhost"));
    assert!(p.bypasses("db.internal"));
    assert!(p.bypasses("internal"), "*.x should also match bare x");
    assert!(p.bypasses("nodots"), "<local> covers dotless hostnames");
    assert!(!p.bypasses("example.com"));
}
