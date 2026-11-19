use super::*;
use pretty_assertions::assert_eq;

/// The header Inference-Ultra Lite sends on every `/v1/` response.
const LITE_CAPABILITIES: &str = "responses.previous_response_id/1, responses.tools-inheritance/1, request-encoding/1;codings=zstd+gzip, responses.stream-tools/1;mode=final";

#[test]
fn parses_lite_capabilities() {
    let capabilities = Capabilities::parse(Some(LITE_CAPABILITIES));
    assert_eq!(
        capabilities,
        Capabilities {
            tools_inheritance: true,
            request_zstd: true,
            stream_tools_mode: Some("final".to_string()),
        }
    );
    assert!(capabilities.echoes_completed_tools());
}

#[test]
fn ignores_unknown_tokens_versions_and_parameters() {
    for (header, expected) in [
        (None, Capabilities::default()),
        (Some(""), Capabilities::default()),
        (
            Some("responses.tools-inheritance/2"),
            Capabilities::default(),
        ),
        (Some("responses.tools-inheritance"), Capabilities::default()),
        (
            Some("future/9;x=y, RESPONSES.TOOLS-INHERITANCE/1;extra=1"),
            Capabilities {
                tools_inheritance: true,
                ..Default::default()
            },
        ),
        (
            Some("request-encoding/1;codings=gzip"),
            Capabilities::default(),
        ),
        (
            Some("responses.stream-tools/1;mode=omit"),
            Capabilities {
                stream_tools_mode: Some("omit".to_string()),
                ..Default::default()
            },
        ),
    ] {
        assert_eq!(Capabilities::parse(header), expected, "{header:?}");
    }
    assert!(
        !Capabilities::parse(Some("responses.stream-tools/1;mode=omit")).echoes_completed_tools()
    );
    assert!(Capabilities::parse(None).echoes_completed_tools());
}

#[test]
fn failed_verification_is_sticky_per_endpoint() {
    let endpoint = "http://10.253.0.1:9/v1/capabilities";
    assert!(!tools_inheritance_disabled(endpoint));
    disable_tools_inheritance(&format!("{endpoint}/"));
    assert!(tools_inheritance_disabled(endpoint));
    assert!(!tools_inheritance_disabled(
        "http://10.253.0.2:9/v1/capabilities"
    ));
}
