//! Negotiated (`auto`) request-body compression for Responses endpoints.
//!
//! An `auto` endpoint receives zstd request bodies until it advertises (RFC 7694 `Accept-Encoding`
//! response header) or demonstrates (HTTP 400/415/422 on a compressed request) that it cannot
//! decode them. The outcome is remembered per endpoint for the life of the process, so a
//! rejecting endpoint costs at most one rejected request per process, and a later
//! `Accept-Encoding` advertisement that lists zstd re-enables compression.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;

use codex_api::Compression;
use http::HeaderMap;
use http::StatusCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Support {
    Supported,
    Unsupported,
}

static ENDPOINT_SUPPORT: LazyLock<Mutex<HashMap<String, Support>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn with_support<T>(f: impl FnOnce(&mut HashMap<String, Support>) -> T) -> T {
    f(&mut ENDPOINT_SUPPORT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

fn endpoint_key(endpoint: &str) -> String {
    endpoint.trim_end_matches('/').to_ascii_lowercase()
}

/// Compression to use for the next request to an `auto` endpoint.
pub(crate) fn negotiated(endpoint: &str) -> Compression {
    match with_support(|support| support.get(&endpoint_key(endpoint)).copied()) {
        Some(Support::Unsupported) => Compression::None,
        Some(Support::Supported) | None => Compression::Zstd,
    }
}

/// Whether an RFC 7694 `Accept-Encoding` value accepts zstd request bodies.
fn advertises_zstd(accept_encoding: &str) -> bool {
    accept_encoding.split(',').any(|coding| {
        let mut parts = coding.split(';');
        let name = parts.next().unwrap_or_default().trim();
        let accepted = parts.all(|parameter| {
            let Some((key, value)) = parameter.split_once('=') else {
                return true;
            };
            !key.trim().eq_ignore_ascii_case("q")
                || value.trim().parse::<f32>().is_ok_and(|q| q > 0.0)
        });
        accepted && (name.eq_ignore_ascii_case("zstd") || name == "*")
    })
}

fn record_advertisement(endpoint: &str, accept_encoding: &str) {
    let support = if advertises_zstd(accept_encoding) {
        Support::Supported
    } else {
        Support::Unsupported
    };
    with_support(|state| state.insert(endpoint_key(endpoint), support));
}

/// Records a response the endpoint accepted. An advertisement wins; otherwise an accepted
/// zstd body proves support.
pub(crate) fn record_accepted(
    endpoint: &str,
    compression: Compression,
    accept_encoding: Option<&str>,
) {
    if let Some(accept_encoding) = accept_encoding {
        record_advertisement(endpoint, accept_encoding);
    } else if compression == Compression::Zstd {
        with_support(|state| state.insert(endpoint_key(endpoint), Support::Supported));
    }
}

/// Records an HTTP error for a request to an `auto` endpoint.
///
/// Returns `true` when a zstd request was rejected because of its coding, so the caller should
/// resend the same request uncompressed.
pub(crate) fn record_rejection(
    endpoint: &str,
    compression: Compression,
    status: StatusCode,
    headers: Option<&HeaderMap>,
) -> bool {
    let advertised = headers
        .and_then(|headers| headers.get(http::header::ACCEPT_ENCODING))
        .and_then(|value| value.to_str().ok());
    if let Some(accept_encoding) = advertised {
        record_advertisement(endpoint, accept_encoding);
    }
    if compression != Compression::Zstd {
        return false;
    }
    let rejected_coding = match advertised {
        Some(accept_encoding) => !advertises_zstd(accept_encoding),
        // Servers without RFC 7694 support report an undecodable body as invalid JSON (400) or
        // an unprocessable entity (422).
        None => matches!(
            status,
            StatusCode::BAD_REQUEST
                | StatusCode::UNSUPPORTED_MEDIA_TYPE
                | StatusCode::UNPROCESSABLE_ENTITY
        ),
    };
    if rejected_coding {
        with_support(|state| state.insert(endpoint_key(endpoint), Support::Unsupported));
    }
    rejected_coding
}

/// The uncompressed resend failed with the same client error as the compressed request, so
/// compression was not the cause. Forget the inference so a later request probes again,
/// unless the endpoint explicitly refused the coding.
pub(crate) fn record_uncompressed_resend_failure(
    endpoint: &str,
    probe_status: StatusCode,
    resend_status: StatusCode,
) {
    if probe_status != StatusCode::UNSUPPORTED_MEDIA_TYPE && probe_status == resend_status {
        with_support(|state| {
            if state.get(&endpoint_key(endpoint)) == Some(&Support::Unsupported) {
                state.remove(&endpoint_key(endpoint));
            }
        });
    }
}

#[cfg(test)]
#[path = "request_compression_tests.rs"]
mod tests;
