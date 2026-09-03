//! The local REST and WebSocket API, and the server that hosts the web UI.
//!
//! It binds to loopback only and requires a bearer token on every request. The
//! surface it exposes — starting downloads, writing files anywhere the user
//! can — would be a serious hole otherwise.

pub mod routes;
pub mod server;
pub mod websocket;

pub use server::{ApiServer, Bound, HttpRequest};
