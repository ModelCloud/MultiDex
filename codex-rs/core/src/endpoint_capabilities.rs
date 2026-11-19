//! Responses capabilities an inference server advertises on each response.
//!
//! The server lists capabilities in [`codex_api::INFERENCE_CAPABILITIES_HEADER`] as
//! comma-separated `name/version[;key=value]*` tokens. A token's meaning is fixed per version;
//! unknown tokens and parameters are ignored.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::Mutex;

/// A continuation that omits `tools` inherits the stored tools of `previous_response_id`.
pub(crate) const TOOLS_INHERITANCE: &str = "responses.tools-inheritance/1";
/// Request bodies may use the codings listed in the `codings` parameter (`+`-separated).
pub(crate) const REQUEST_ENCODING: &str = "request-encoding/1";
/// How the streamed lifecycle events carry the response `tools` (`mode=final|echo|omit`).
pub(crate) const STREAM_TOOLS: &str = "responses.stream-tools/1";
/// With this stream-tools mode, `response.completed` carries no tools to verify.
const STREAM_TOOLS_MODE_OMIT: &str = "omit";

/// Capabilities of a single response.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// [`TOOLS_INHERITANCE`] was advertised.
    pub(crate) tools_inheritance: bool,
    /// [`REQUEST_ENCODING`] lists zstd.
    pub(crate) request_zstd: bool,
    /// `mode` of [`STREAM_TOOLS`], when advertised.
    pub(crate) stream_tools_mode: Option<String>,
}

impl Capabilities {
    pub(crate) fn parse(header: Option<&str>) -> Self {
        let mut capabilities = Self::default();
        for token in header.unwrap_or_default().split(',') {
            let mut parts = token.split(';');
            let name = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
            let parameter = |key: &str| {
                parts.clone().find_map(|parameter| {
                    let (name, value) = parameter.split_once('=')?;
                    name.trim()
                        .eq_ignore_ascii_case(key)
                        .then(|| value.trim().to_ascii_lowercase())
                })
            };
            match name.as_str() {
                TOOLS_INHERITANCE => capabilities.tools_inheritance = true,
                REQUEST_ENCODING => {
                    capabilities.request_zstd = parameter("codings")
                        .is_some_and(|codings| codings.split('+').any(|coding| coding == "zstd"));
                }
                STREAM_TOOLS => capabilities.stream_tools_mode = parameter("mode"),
                _ => {}
            }
        }
        capabilities
    }

    /// Whether `response.completed` is expected to echo the response tools.
    pub(crate) fn echoes_completed_tools(&self) -> bool {
        self.stream_tools_mode.as_deref() != Some(STREAM_TOOLS_MODE_OMIT)
    }
}

/// Endpoints whose tool inheritance failed verification in this process.
static TOOLS_INHERITANCE_DISABLED: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn endpoint_key(endpoint: &str) -> String {
    endpoint.trim_end_matches('/').to_ascii_lowercase()
}

/// Whether tool inheritance failed verification on `endpoint`.
pub(crate) fn tools_inheritance_disabled(endpoint: &str) -> bool {
    TOOLS_INHERITANCE_DISABLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&endpoint_key(endpoint))
}

/// Stops omitting tools for `endpoint` for the rest of the process.
pub(crate) fn disable_tools_inheritance(endpoint: &str) {
    TOOLS_INHERITANCE_DISABLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(endpoint_key(endpoint));
}

#[cfg(test)]
#[path = "endpoint_capabilities_tests.rs"]
mod tests;
