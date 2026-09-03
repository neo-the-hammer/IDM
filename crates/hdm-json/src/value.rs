//! The JSON value type and its accessors.

use std::collections::BTreeMap;
use std::fmt;

/// A JSON value.
///
/// Objects keep their keys in insertion order rather than using a map, which
/// keeps API responses byte-stable across runs and makes diffing saved state
/// files readable. Hydra's objects are small (tens of keys at most), so the
/// linear key lookup is not worth trading for hashing.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Builds an object from any iterable of key/value pairs.
    pub fn object<K, I>(pairs: I) -> Json
    where
        K: Into<String>,
        I: IntoIterator<Item = (K, Json)>,
    {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Builds an array from any iterable of values.
    pub fn array<I: IntoIterator<Item = Json>>(items: I) -> Json {
        Json::Arr(items.into_iter().collect())
    }

    /// Looks up a key. Returns `None` for non-objects, so chained lookups on
    /// unexpected shapes degrade to `None` rather than panicking.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Looks up a key for mutation.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Obj(pairs) => pairs.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Inserts or replaces a key, preserving the position of an existing key.
    pub fn insert(&mut self, key: impl Into<String>, value: Json) {
        let key = key.into();
        if let Json::Obj(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key, value)),
            }
        }
    }

    /// Indexes an array.
    pub fn idx(&self, i: usize) -> Option<&Json> {
        match self {
            Json::Arr(items) => items.get(i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// Reads an integer, rejecting values that are fractional or out of range
    /// rather than silently truncating them.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 => {
                Some(*n as i64)
            }
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= u64::MAX as f64 => {
                Some(*n as u64)
            }
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(pairs) => Some(pairs),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Convenience readers that fall back to a default. These make parsing
    /// configuration and API requests far less noisy at call sites.
    pub fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).and_then(Json::as_str).unwrap_or(default)
    }

    pub fn u64_or(&self, key: &str, default: u64) -> u64 {
        self.get(key).and_then(Json::as_u64).unwrap_or(default)
    }

    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.get(key).and_then(Json::as_bool).unwrap_or(default)
    }
}

impl fmt::Display for Json {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_compact())
    }
}

macro_rules! from_num {
    ($($t:ty),*) => {$(
        impl From<$t> for Json {
            fn from(v: $t) -> Json { Json::Num(v as f64) }
        }
    )*};
}
from_num!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64);

impl From<bool> for Json {
    fn from(v: bool) -> Json {
        Json::Bool(v)
    }
}
impl From<&str> for Json {
    fn from(v: &str) -> Json {
        Json::Str(v.to_string())
    }
}
impl From<String> for Json {
    fn from(v: String) -> Json {
        Json::Str(v)
    }
}
impl From<&String> for Json {
    fn from(v: &String) -> Json {
        Json::Str(v.clone())
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Json {
        v.map_or(Json::Null, Into::into)
    }
}
impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(v: Vec<T>) -> Json {
        Json::Arr(v.into_iter().map(Into::into).collect())
    }
}
impl<T: Into<Json> + Clone> From<&[T]> for Json {
    fn from(v: &[T]) -> Json {
        Json::Arr(v.iter().cloned().map(Into::into).collect())
    }
}
impl<T: Into<Json>> From<BTreeMap<String, T>> for Json {
    fn from(v: BTreeMap<String, T>) -> Json {
        Json::Obj(v.into_iter().map(|(k, val)| (k, val.into())).collect())
    }
}
