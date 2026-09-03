//! HTTPS end to end, through the runtime-loaded OpenSSL backend.

#![cfg(unix)]

use hdm_net::client::{Client, ClientConfig};
use hdm_net::http::Request;
use hdm_net::url::Url;
use hdm_testserver::{test_data, tls, ServerBuilder};

/// A client that trusts only the test suite's own certificate.
fn tls_client(ca: &std::path::Path) -> Client {
    let mut config = ClientConfig::new();
    config.tls.ca_file = Some(ca.to_string_lossy().into_owned());
    Client::new(config).expect("client")
}

#[test]
fn openssl_is_available() {
    hdm_net::tls::availability().expect("the tests require a usable OpenSSL");
}

#[test]
fn fetches_over_https() {
    let data = test_data(40_000);
    let server = ServerBuilder::new()
        .file("/f.bin", data.clone())
        .start_tls(tls::self_signed().unwrap());
    let (ca, _keep) = tls::ca_file().unwrap();

    let mut fetch = tls_client(&ca)
        .send(Request::get(Url::parse(&server.url("/f.bin")).unwrap()))
        .expect("HTTPS request failed");

    assert_eq!(fetch.response.status, 200);
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn range_requests_work_over_https() {
    let data = test_data(20_000);
    let server = ServerBuilder::new()
        .file("/f.bin", data.clone())
        .start_tls(tls::self_signed().unwrap());
    let (ca, _keep) = tls::ca_file().unwrap();
    let client = tls_client(&ca);

    // Two segments over TLS must reassemble byte-exactly, the same as plaintext.
    let mut assembled = Vec::new();
    for (start, end) in [(0u64, 9_999u64), (10_000, 19_999)] {
        let request =
            Request::get(Url::parse(&server.url("/f.bin")).unwrap()).with_range(start, Some(end));
        let mut fetch = client.send(request).unwrap();
        assert_eq!(fetch.response.status, 206);
        assembled.extend_from_slice(&fetch.response.read_to_vec(usize::MAX).unwrap());
    }
    assert_eq!(assembled, data);
}

/// The whole point of certificate validation: an untrusted certificate must
/// stop the transfer, not merely warn.
#[test]
fn an_untrusted_certificate_is_rejected() {
    let server = ServerBuilder::new()
        .file("/f.bin", test_data(10))
        .start_tls(tls::self_signed().unwrap());

    // A default client trusts only the system store, which has never heard of
    // the test certificate.
    let client = Client::new(ClientConfig::new()).unwrap();
    let err = client
        .send(Request::get(Url::parse(&server.url("/f.bin")).unwrap()))
        .expect_err("a self-signed certificate must not be accepted by default");
    let message = err.to_string();
    assert!(
        message.contains("certificate") || message.contains("handshake"),
        "unexpected error: {message}"
    );
}

/// Connecting by IP when the certificate names `localhost` must fail, since
/// the certificate does not vouch for that identity... except this one does,
/// via an IP SAN, so instead check the inverse: a name the cert does not cover.
#[test]
fn hostname_verification_rejects_a_mismatched_name() {
    let data = test_data(10);
    let server = ServerBuilder::new()
        .file("/f.bin", data)
        .start_tls(tls::self_signed().unwrap());
    let (ca, _keep) = tls::ca_file().unwrap();

    // The certificate covers `localhost` and `127.0.0.1` only. Reaching the
    // same socket under a different name must be refused even though the
    // certificate itself is trusted.
    let url = format!("https://localhost.localdomain:{}/f.bin", server.port());
    let err = tls_client(&ca)
        .send(Request::get(Url::parse(&url).unwrap()))
        .expect_err("a certificate for another name must be rejected");
    assert!(
        err.to_string().contains("certificate")
            || err.to_string().contains("handshake")
            || err.kind() == std::io::ErrorKind::NotFound,
        "unexpected error: {err}"
    );
}

/// The explicit escape hatch: users with self-signed appliances need it, and
/// IDM offers the same. It must work, and it must be the only way through.
#[test]
fn insecure_mode_accepts_a_self_signed_certificate() {
    let data = test_data(1000);
    let server = ServerBuilder::new()
        .file("/f.bin", data.clone())
        .start_tls(tls::self_signed().unwrap());

    let mut config = ClientConfig::new();
    config.tls.insecure = true;
    let mut fetch = Client::new(config)
        .unwrap()
        .send(Request::get(Url::parse(&server.url("/f.bin")).unwrap()))
        .expect("insecure mode should connect");
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn a_truncated_tls_stream_is_reported_as_truncation() {
    let server = ServerBuilder::new()
        .file_with("/cut.bin", test_data(80_000), |f| f.cut_after = Some(2048))
        .start_tls(tls::self_signed().unwrap());
    let (ca, _keep) = tls::ca_file().unwrap();

    let mut fetch = tls_client(&ca)
        .send(Request::get(Url::parse(&server.url("/cut.bin")).unwrap()))
        .unwrap();

    use std::io::Read;
    let mut sink = Vec::new();
    let err = fetch
        .response
        .body
        .read_to_end(&mut sink)
        .expect_err("a truncated TLS body must not read as success");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof, "got: {err}");
}
