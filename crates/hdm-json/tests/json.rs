use hdm_json::{json, parse, Json, MAX_DEPTH};

// ---------------------------------------------------------------- valid input

#[test]
fn parses_scalars() {
    assert_eq!(parse("null").unwrap(), Json::Null);
    assert_eq!(parse("true").unwrap(), Json::Bool(true));
    assert_eq!(parse("false").unwrap(), Json::Bool(false));
    assert_eq!(parse(r#""hi""#).unwrap(), Json::Str("hi".into()));
    assert_eq!(parse("0").unwrap(), Json::Num(0.0));
}

#[test]
fn parses_numbers_across_the_grammar() {
    for (src, want) in [
        ("0", 0.0),
        ("-0", -0.0),
        ("42", 42.0),
        ("-42", -42.0),
        ("123.456", 123.456),
        ("1e3", 1000.0),
        ("1E3", 1000.0),
        ("1e+3", 1000.0),
        ("1e-3", 0.001),
        ("-2.5E2", -250.0),
        ("123456789012345", 123456789012345.0),
    ] {
        assert_eq!(parse(src).unwrap().as_f64().unwrap(), want, "parsing {src}");
    }
}

#[test]
fn parses_nested_containers() {
    let v = parse(r#"{"a":[1,2,{"b":null}],"c":{}}"#).unwrap();
    assert_eq!(
        v.get("a").unwrap().idx(2).unwrap().get("b").unwrap(),
        &Json::Null
    );
    assert_eq!(v.get("c").unwrap().as_obj().unwrap().len(), 0);
}

#[test]
fn ignores_insignificant_whitespace() {
    let v = parse(" \r\n\t{ \"a\" : [ 1 , 2 ] } \n").unwrap();
    assert_eq!(v.get("a").unwrap().as_arr().unwrap().len(), 2);
}

#[test]
fn decodes_string_escapes() {
    let v = parse(r#""a\"b\\c\/d\be\ff\ng\rh\ti""#).unwrap();
    assert_eq!(v.as_str().unwrap(), "a\"b\\c/d\u{8}e\u{c}f\ng\rh\ti");
}

#[test]
fn decodes_unicode_escapes_and_surrogate_pairs() {
    assert_eq!(parse(r#""\u0041""#).unwrap().as_str().unwrap(), "A");
    assert_eq!(parse(r#""\u00e9""#).unwrap().as_str().unwrap(), "é");
    // Persian, since the UI ships an fa locale.
    assert_eq!(
        parse(r#""\u062f\u0627\u0646\u0644\u0648\u062f""#)
            .unwrap()
            .as_str()
            .unwrap(),
        "دانلود"
    );
    // A surrogate pair for U+1F409 DRAGON.
    assert_eq!(parse(r#""\ud83d\udc09""#).unwrap().as_str().unwrap(), "🐉");
}

#[test]
fn preserves_multibyte_utf8_passed_through_literally() {
    let v = parse("\"دانلود سریع 🐉 ok\"").unwrap();
    assert_eq!(v.as_str().unwrap(), "دانلود سریع 🐉 ok");
}

#[test]
fn last_duplicate_key_wins() {
    let v = parse(r#"{"a":1,"a":2}"#).unwrap();
    assert_eq!(v.get("a").unwrap().as_u64(), Some(2));
    assert_eq!(
        v.as_obj().unwrap().len(),
        1,
        "duplicate must not add a second entry"
    );
}

#[test]
fn parses_at_the_depth_limit() {
    let deep = format!("{}1{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert!(parse(&deep).is_ok());
}

// -------------------------------------------------------------- invalid input

#[test]
fn rejects_malformed_documents() {
    for src in [
        "",
        "   ", // empty
        "{",
        "[",
        "}",
        "]",          // unbalanced
        "{\"a\"}",    // missing value
        "{\"a\":}",   // missing value
        "{a:1}",      // unquoted key
        "{'a':1}",    // single quotes
        "[1,]",       // trailing comma
        "{\"a\":1,}", // trailing comma
        "[1 2]",      // missing comma
        "01",         // leading zero
        "-",          // lone sign
        "1.",         // trailing decimal point
        ".5",         // leading decimal point
        "1e",         // truncated exponent
        "1e+",        // truncated exponent
        "+1",         // explicit plus
        "NaN",
        "Infinity", // not JSON
        "nul",
        "tru",
        "fals",            // truncated literals
        "\"unterminated",  // unterminated string
        "\"bad\\escape\"", // invalid escape
        "\"\\u00\"",       // truncated \u
        "\"\\uZZZZ\"",     // bad hex
        "1 2",             // trailing content
        "{} {}",           // trailing content
        "// comment\n1",   // comments are not JSON
    ] {
        assert!(parse(src).is_err(), "expected {src:?} to be rejected");
    }
}

#[test]
fn rejects_raw_control_characters_in_strings() {
    assert!(parse("\"a\nb\"").is_err());
    assert!(parse("\"a\tb\"").is_err());
    assert!(parse("\"a\u{0}b\"").is_err());
}

#[test]
fn rejects_broken_surrogates() {
    assert!(parse(r#""\ud83d""#).is_err(), "lone high surrogate");
    assert!(parse(r#""\udc09""#).is_err(), "lone low surrogate");
    assert!(
        parse(r#""\ud83d\u0041""#).is_err(),
        "high surrogate + non-surrogate"
    );
    assert!(parse(r#""\ud83d\ud83d""#).is_err(), "two high surrogates");
}

/// A hostile payload of deeply nested arrays must be rejected, not crash the
/// process. The parser is recursive, so this is a real denial-of-service guard
/// on input that arrives from browser extensions and plugin subprocesses.
#[test]
fn rejects_nesting_past_the_depth_limit_without_overflowing() {
    let too_deep = format!(
        "{}1{}",
        "[".repeat(MAX_DEPTH + 1),
        "]".repeat(MAX_DEPTH + 1)
    );
    let err = parse(&too_deep).unwrap_err();
    assert!(err.message.contains("nesting"), "got: {}", err.message);

    // Unbalanced too: the limit must trip before the input is exhausted.
    let bomb = "[".repeat(100_000);
    assert!(parse(&bomb).is_err());
}

#[test]
fn error_reports_useful_position() {
    let err = parse("{\n  \"a\": 1,\n  \"b\": @\n}").unwrap_err();
    assert_eq!(err.line, 3);
    assert!(err.message.contains('@'), "got: {}", err.message);
}

// -------------------------------------------------------------- serialization

#[test]
fn serializes_compactly() {
    let v = json!({"b": 1, "a": [true, null, "x"]});
    assert_eq!(v.to_string_compact(), r#"{"b":1,"a":[true,null,"x"]}"#);
}

#[test]
fn preserves_key_insertion_order() {
    let v = json!({"z": 1, "m": 2, "a": 3});
    assert_eq!(v.to_string_compact(), r#"{"z":1,"m":2,"a":3}"#);
}

#[test]
fn serializes_integers_without_a_fractional_suffix() {
    assert_eq!(json!(8).to_string_compact(), "8");
    assert_eq!(json!(-1).to_string_compact(), "-1");
    assert_eq!(json!(0).to_string_compact(), "0");
    // A realistic byte count must not come out as 4.294967296e9.
    assert_eq!(
        Json::from(4_294_967_296u64).to_string_compact(),
        "4294967296"
    );
    assert_eq!(json!(1.5).to_string_compact(), "1.5");
}

#[test]
fn escapes_control_characters_and_quotes() {
    let v = Json::Str("q\"b\\s\nl\rr\tt\u{1}c".into());
    assert_eq!(v.to_string_compact(), r#""q\"b\\s\nl\rr\tt\u0001c""#);
}

#[test]
fn non_finite_numbers_become_null_rather_than_invalid_json() {
    assert_eq!(Json::Num(f64::NAN).to_string_compact(), "null");
    assert_eq!(Json::Num(f64::INFINITY).to_string_compact(), "null");
    // Whatever we emit must still parse.
    assert!(parse(&Json::Num(f64::NAN).to_string_compact()).is_ok());
}

#[test]
fn pretty_printing_is_indented_and_reparses() {
    let v = json!({"a": [1, 2], "b": {"c": true}, "empty": [], "eo": {}});
    let text = v.to_string_pretty();
    assert!(text.contains("\n  \"a\": ["), "got:\n{text}");
    assert!(
        text.contains("\"empty\": []"),
        "empty containers stay on one line"
    );
    assert_eq!(parse(&text).unwrap(), v);
}

#[test]
fn round_trips_through_text() {
    let original = json!({
        "url": "https://example.com/a b/файл.iso?x=1&y=2",
        "segments": [
            {"start": 0, "end": 1048575, "done": 1048576},
            {"start": 1048576, "end": 2097151, "done": 0}
        ],
        "etag": "\"abc\\123\"",
        "note": "دانلود 🐉",
        "paused": false,
        "limit": null
    });
    let once = original.to_string_compact();
    let back = parse(&once).unwrap();
    assert_eq!(back, original);
    assert_eq!(back.to_string_compact(), once, "serialization is stable");
}

// -------------------------------------------------------------------- the API

#[test]
fn accessors_return_none_on_shape_mismatch() {
    let v = json!({"a": 1});
    assert!(v.get("missing").is_none());
    assert!(v.idx(0).is_none(), "object indexed as array");
    assert!(json!([1]).get("a").is_none(), "array indexed as object");
    assert!(v.get("a").unwrap().as_str().is_none());
}

#[test]
fn integer_accessors_reject_lossy_reads() {
    assert_eq!(
        Json::Num(1.5).as_i64(),
        None,
        "fractional is not an integer"
    );
    assert_eq!(Json::Num(-1.0).as_u64(), None, "negative is not unsigned");
    assert_eq!(Json::Num(1e300).as_i64(), None, "out of i64 range");
    assert_eq!(Json::Num(-1.0).as_i64(), Some(-1));
}

#[test]
fn insert_replaces_in_place() {
    let mut v = json!({"a": 1, "b": 2});
    v.insert("a", json!(9));
    v.insert("c", json!(3));
    assert_eq!(v.to_string_compact(), r#"{"a":9,"b":2,"c":3}"#);
}

#[test]
fn defaulting_readers() {
    let v = json!({"name": "iso", "n": 4, "on": true});
    assert_eq!(v.str_or("name", "?"), "iso");
    assert_eq!(v.str_or("nope", "?"), "?");
    assert_eq!(v.u64_or("n", 1), 4);
    assert_eq!(v.u64_or("nope", 1), 1);
    assert!(v.bool_or("on", false));
    assert!(!v.bool_or("nope", false));
}

#[test]
fn json_macro_builds_expected_shapes() {
    assert_eq!(json!(null), Json::Null);
    assert_eq!(json!([]), Json::Arr(vec![]));
    assert_eq!(json!({}), Json::Obj(vec![]));
    let nested = json!({"a": {"b": [1, "two", false, null]}});
    assert_eq!(
        nested.to_string_compact(),
        r#"{"a":{"b":[1,"two",false,null]}}"#
    );
}

#[test]
fn conversions_from_rust_types() {
    assert_eq!(Json::from(Some(3u8)).as_u64(), Some(3));
    assert_eq!(Json::from(None::<u8>), Json::Null);
    assert_eq!(Json::from(vec![1u8, 2]).as_arr().unwrap().len(), 2);
    assert_eq!(Json::from("s"), Json::Str("s".into()));
}
