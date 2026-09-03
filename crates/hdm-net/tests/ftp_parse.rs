//! Unit coverage for the FTP reply parsers.
//!
//! The transfer path itself is exercised by the engine's integration tests;
//! these cover the parsing that a hostile or unusual server can drive.

use hdm_net::ftp::{parse_epsv_for_test, parse_pasv_for_test};

#[test]
fn epsv_replies_parse() {
    assert_eq!(
        parse_epsv_for_test("Entering Extended Passive Mode (|||51234|)"),
        Some(51234)
    );
    // The delimiter is whatever the server picks, not necessarily '|'.
    assert_eq!(
        parse_epsv_for_test("Extended Passive Mode (!!!1024!)"),
        Some(1024)
    );
    assert_eq!(parse_epsv_for_test("no parentheses"), None);
    assert_eq!(parse_epsv_for_test("(|||notanumber|)"), None);
}

#[test]
fn pasv_replies_parse() {
    // 195 * 256 + 149 = 50069
    assert_eq!(
        parse_pasv_for_test("Entering Passive Mode (127,0,0,1,195,149)"),
        Some(50069)
    );
    assert_eq!(parse_pasv_for_test("(10,0,0,1,0,21)"), Some(21));
    assert_eq!(parse_pasv_for_test("(1,2,3,4,5)"), None, "too few fields");
    assert_eq!(
        parse_pasv_for_test("(1,2,3,4,999,999)"),
        None,
        "field out of range"
    );
    assert_eq!(parse_pasv_for_test("garbage"), None);
}
