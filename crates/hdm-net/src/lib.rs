//! Dependency-free HTTP/1.1, TLS and FTP client for Hydra Download Manager.

pub mod auth;
pub mod client;
pub mod cookie;
pub mod ftp;
pub mod headers;
pub mod http;
pub mod proxy;
pub mod punycode;
pub mod stream;
pub mod tls;
pub mod url;

pub use client::{Client, ClientConfig, Fetch};
pub use headers::Headers;
pub use http::{Request, Response};
pub use url::{percent_decode, percent_decode_str, percent_encode, Url, UrlError};
