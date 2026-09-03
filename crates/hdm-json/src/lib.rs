//! A small, strict, dependency-free JSON implementation.
//!
//! Hydra needs JSON in four places: the local REST API, the on-disk state
//! store, the browser-extension protocol, and the Python plugin protocol.
//! Rather than take a dependency, this crate provides just enough: a value
//! type, a strict RFC 8259 parser with a nesting limit, and compact/pretty
//! serializers.
//!
//! ```
//! use hdm_json::{json, Json};
//!
//! let doc = json!({
//!     "url": "https://example.com/file.iso",
//!     "connections": 8,
//!     "resume": true
//! });
//! assert_eq!(doc.get("connections").and_then(Json::as_u64), Some(8));
//!
//! let parsed = hdm_json::parse(&doc.to_string_compact()).unwrap();
//! assert_eq!(parsed, doc);
//! ```

mod parse;
mod ser;
mod value;

pub use parse::{parse, Error, MAX_DEPTH};
pub use value::Json;

/// Builds a [`Json`] value from literal syntax.
///
/// Object values and array elements are parsed as single token trees, so a
/// multi-token expression needs parentheses: `json!({"n": (a + b)})`.
#[macro_export]
macro_rules! json {
    (null) => { $crate::Json::Null };
    ([]) => { $crate::Json::Arr(::std::vec::Vec::new()) };
    ([ $($elem:tt),+ $(,)? ]) => {
        $crate::Json::Arr(::std::vec![ $( $crate::json!($elem) ),+ ])
    };
    ({}) => { $crate::Json::Obj(::std::vec::Vec::new()) };
    ({ $($key:tt : $val:tt),+ $(,)? }) => {
        $crate::Json::Obj(::std::vec![
            $( (::std::string::String::from($key), $crate::json!($val)) ),+
        ])
    };
    ($other:expr) => { $crate::Json::from($other) };
}
