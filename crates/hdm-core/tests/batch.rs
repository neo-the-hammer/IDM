//! Batch URL patterns.

use hdm_core::batch::{expand, is_pattern, BatchError, MAX_EXPANSION};

#[test]
fn expands_a_numeric_range() {
    let urls = expand("http://example.com/photo[1-4].jpg").unwrap();
    assert_eq!(
        urls,
        [
            "http://example.com/photo1.jpg",
            "http://example.com/photo2.jpg",
            "http://example.com/photo3.jpg",
            "http://example.com/photo4.jpg",
        ]
    );
}

/// A leading zero is how the user asks for fixed-width names, and getting it
/// wrong produces files that sort in the wrong order.
#[test]
fn preserves_zero_padding() {
    let urls = expand("http://a/img[008-011].png").unwrap();
    assert_eq!(
        urls,
        [
            "http://a/img008.png",
            "http://a/img009.png",
            "http://a/img010.png",
            "http://a/img011.png"
        ]
    );

    // Without a leading zero there is no padding, even across a digit boundary.
    let plain = expand("http://a/img[8-11].png").unwrap();
    assert_eq!(
        plain,
        [
            "http://a/img8.png",
            "http://a/img9.png",
            "http://a/img10.png",
            "http://a/img11.png"
        ]
    );
}

#[test]
fn expands_a_letter_range() {
    assert_eq!(
        expand("http://a/part[a-e].zip").unwrap(),
        [
            "http://a/parta.zip",
            "http://a/partb.zip",
            "http://a/partc.zip",
            "http://a/partd.zip",
            "http://a/parte.zip"
        ]
    );
    assert_eq!(
        expand("http://a/[X-Z].bin").unwrap(),
        ["http://a/X.bin", "http://a/Y.bin", "http://a/Z.bin"]
    );
}

#[test]
fn honours_a_step() {
    assert_eq!(
        expand("http://a/f[1-9:3].bin").unwrap(),
        ["http://a/f1.bin", "http://a/f4.bin", "http://a/f7.bin"]
    );
}

/// Several ranges produce every combination, with the last varying fastest --
/// the order someone reading the pattern expects.
#[test]
fn combines_several_ranges() {
    assert_eq!(
        expand("http://a/[1-2]/part[a-b].bin").unwrap(),
        [
            "http://a/1/parta.bin",
            "http://a/1/partb.bin",
            "http://a/2/parta.bin",
            "http://a/2/partb.bin",
        ]
    );
}

#[test]
fn a_single_value_range_is_allowed() {
    assert_eq!(expand("http://a/f[5-5].bin").unwrap(), ["http://a/f5.bin"]);
}

#[test]
fn recognizes_patterns() {
    assert!(is_pattern("http://a/f[1-10].bin"));
    assert!(is_pattern("http://a/f[a-z].bin"));
    assert!(!is_pattern("http://a/plain.bin"));
    // Real URLs contain brackets that are not ranges.
    assert!(!is_pattern("http://a/file[final].bin"));
    assert!(!is_pattern("http://a/?ids[]=1"));
}

/// Brackets that are not ranges must survive as literal text rather than
/// breaking the URL.
#[test]
fn leaves_non_range_brackets_alone() {
    let urls = expand("http://a/[tag]/f[1-2].bin").unwrap();
    assert_eq!(urls, ["http://a/[tag]/f1.bin", "http://a/[tag]/f2.bin"]);

    // An unmatched bracket is literal too.
    let unmatched = expand("http://a/f[1-2]-[x.bin").unwrap();
    assert_eq!(unmatched, ["http://a/f1-[x.bin", "http://a/f2-[x.bin"]);
}

#[test]
fn a_url_with_no_pattern_is_reported_as_such() {
    assert_eq!(expand("http://a/plain.bin"), Err(BatchError::NoPattern));
}

#[test]
fn rejects_backwards_and_malformed_ranges() {
    // A reversed range is a mistake, not an instruction to count down.
    assert_eq!(expand("http://a/f[10-1].bin"), Err(BatchError::NoPattern));
    assert_eq!(expand("http://a/f[z-a].bin"), Err(BatchError::NoPattern));
    assert_eq!(expand("http://a/f[-5].bin"), Err(BatchError::NoPattern));
    assert_eq!(expand("http://a/f[1-].bin"), Err(BatchError::NoPattern));
    assert_eq!(
        expand("http://a/f[abc-def].bin"),
        Err(BatchError::NoPattern)
    );
    // Mixed case letters would span the punctuation between the alphabets.
    assert_eq!(expand("http://a/f[a-Z].bin"), Err(BatchError::NoPattern));
}

/// `[1-1000000]` is far more likely to be a typo than an intention, and
/// materialising it would exhaust memory before anyone noticed.
#[test]
fn refuses_an_absurd_expansion() {
    let error = expand("http://a/f[1-2000000].bin").unwrap_err();
    assert!(
        matches!(error, BatchError::TooLarge(2_000_000)),
        "got {error:?}"
    );
    assert!(error.to_string().contains("more than"));

    // The limit applies to the product of several ranges, not each one.
    let combined = expand("http://a/[1-200]/[1-200]/[1-200].bin").unwrap_err();
    assert!(matches!(combined, BatchError::TooLarge(_)));
}

#[test]
fn expands_right_up_to_the_limit() {
    let urls = expand(&format!("http://a/f[1-{MAX_EXPANSION}].bin")).unwrap();
    assert_eq!(urls.len(), MAX_EXPANSION);
    assert_eq!(urls[0], "http://a/f1.bin");
    assert_eq!(
        urls[MAX_EXPANSION - 1],
        format!("http://a/f{MAX_EXPANSION}.bin")
    );
}

/// The query string is as valid a place for a pattern as the path.
#[test]
fn a_pattern_works_anywhere_in_the_url() {
    assert_eq!(
        expand("http://a/get?page=[1-3]&format=zip").unwrap(),
        [
            "http://a/get?page=1&format=zip",
            "http://a/get?page=2&format=zip",
            "http://a/get?page=3&format=zip",
        ]
    );
}
