use super::*;
use pretty_assertions::assert_eq;

fn headers(accept_encoding: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::ACCEPT_ENCODING,
        http::HeaderValue::from_str(accept_encoding).expect("header value"),
    );
    headers
}

#[test]
fn parses_rfc7694_advertisements() {
    for (value, expected) in [
        ("zstd, gzip", true),
        ("gzip;q=1, ZSTD;q=0.5", true),
        ("*", true),
        ("gzip", false),
        ("zstd;q=0, gzip", false),
        ("", false),
        ("identity", false),
    ] {
        assert_eq!(advertises_zstd(value), expected, "{value:?}");
    }
}

#[test]
fn rejected_probe_disables_until_advertised() {
    let endpoint = "http://10.255.0.1:9/v1/rejects";
    assert_eq!(negotiated(endpoint), Compression::Zstd);
    // Uncompressed failures never change the negotiation.
    assert!(!record_rejection(
        endpoint,
        Compression::None,
        StatusCode::BAD_REQUEST,
        None
    ));
    assert_eq!(negotiated(endpoint), Compression::Zstd);
    // Server errors are not attributed to the coding.
    assert!(!record_rejection(
        endpoint,
        Compression::Zstd,
        StatusCode::INTERNAL_SERVER_ERROR,
        None
    ));
    assert_eq!(negotiated(endpoint), Compression::Zstd);

    assert!(record_rejection(
        endpoint,
        Compression::Zstd,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        None
    ));
    assert_eq!(negotiated(endpoint), Compression::None);
    // An explicit refusal survives a failing resend.
    record_uncompressed_resend_failure(
        endpoint,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
    );
    assert_eq!(negotiated(endpoint), Compression::None);

    // A restarted server that advertises zstd re-enables compression.
    record_accepted(endpoint, Compression::None, Some("zstd, gzip"));
    assert_eq!(negotiated(endpoint), Compression::Zstd);
    // An advertisement without zstd disables it.
    record_accepted(endpoint, Compression::Zstd, Some("gzip"));
    assert_eq!(negotiated(endpoint), Compression::None);
}

#[test]
fn bad_request_unrelated_to_coding_is_forgotten() {
    let endpoint = "http://10.255.0.1:9/v1/bad-request";
    assert!(record_rejection(
        endpoint,
        Compression::Zstd,
        StatusCode::BAD_REQUEST,
        None
    ));
    assert_eq!(negotiated(endpoint), Compression::None);
    record_uncompressed_resend_failure(endpoint, StatusCode::BAD_REQUEST, StatusCode::BAD_REQUEST);
    assert_eq!(negotiated(endpoint), Compression::Zstd);
}

#[test]
fn advertisement_on_rejection_decides() {
    let endpoint = "http://10.255.0.1:9/v1/advertises";
    // A 400 that still advertises zstd is not a coding problem.
    assert!(!record_rejection(
        endpoint,
        Compression::Zstd,
        StatusCode::BAD_REQUEST,
        Some(&headers("zstd, gzip")),
    ));
    assert_eq!(negotiated(endpoint), Compression::Zstd);
    assert!(record_rejection(
        endpoint,
        Compression::Zstd,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        Some(&headers("gzip")),
    ));
    assert_eq!(negotiated(endpoint), Compression::None);
    // Accepted zstd proves support even without an advertisement.
    record_accepted(endpoint, Compression::Zstd, None);
    assert_eq!(negotiated(endpoint), Compression::Zstd);
}
