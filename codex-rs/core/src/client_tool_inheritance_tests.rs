use super::super::Compression;
use super::super::ResponsesApiRequest;
use super::super::ResponsesHttpExecutor;
use super::super::WireToolsHash;
use super::super::omit_inherited_tools;
use super::ModelClient;
use super::Prompt;
use super::test_model_client;
use super::test_model_info;
use super::test_responses_metadata_for_client;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::RequestCompression;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;

/// Production turns advertise about this many tools.
const PRODUCTION_TOOL_COUNT: usize = 246;

const WORDS: &[&str] = &[
    "read",
    "file",
    "path",
    "workspace",
    "repository",
    "command",
    "output",
    "patch",
    "search",
    "pattern",
    "directory",
    "session",
    "terminal",
    "process",
    "timeout",
    "argument",
    "result",
    "schema",
    "server",
    "resource",
    "message",
    "thread",
    "issue",
    "branch",
    "commit",
    "review",
    "browser",
    "page",
    "selector",
    "screenshot",
    "query",
    "index",
    "token",
    "limit",
    "offset",
    "cursor",
    "filter",
    "label",
    "status",
    "project",
    "artifact",
    "comment",
    "upload",
    "export",
];

/// Deterministic, non-repetitive prose so compression ratios are not inflated.
fn prose(seed: &mut u64, words: usize) -> String {
    (0..words)
        .map(|_| {
            *seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            WORDS[(*seed >> 33) as usize % WORDS.len()]
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A tool list shaped like a production MultiDex turn: 246 function tools whose JSON totals
/// roughly 400 KB.
fn production_sized_tools() -> anyhow::Result<Vec<ToolSpec>> {
    let mut seed = 0x5eed_u64;
    (0..PRODUCTION_TOOL_COUNT)
        .map(|index| {
            let mut properties = serde_json::Map::new();
            for property in 0..8 {
                properties.insert(
                    format!("arg_{property}_{}", prose(&mut seed, 1)),
                    json!({"type": "string", "description": prose(&mut seed, 14)}),
                );
            }
            let parameters: JsonSchema = serde_json::from_value(json!({
                "type": "object",
                "properties": properties,
                "required": ["arg_0"],
                "additionalProperties": false,
            }))?;
            Ok(ToolSpec::Function(ResponsesApiTool {
                name: format!("mcp__server_{}__tool_{index}", index % 17),
                description: prose(&mut seed, 33),
                strict: false,
                defer_loading: None,
                parameters,
                output_schema: None,
            }))
        })
        .collect()
}

fn user_message(text: &str) -> anyhow::Result<ResponseItem> {
    Ok(serde_json::from_value(json!({
        "type": "message", "role": "user",
        "content": [{"type": "input_text", "text": text}],
    }))?)
}

fn build_request(
    client: &ModelClient,
    tools: Vec<ToolSpec>,
    input: Vec<ResponseItem>,
) -> anyhow::Result<ResponsesApiRequest> {
    Ok(client.build_responses_request(
        &Prompt {
            input,
            tools: tools.into(),
            ..Default::default()
        },
        &test_model_info(),
        /*effort*/ None,
        ReasoningSummary::None,
        /*service_tier*/ None,
        &test_responses_metadata_for_client(
            client,
            /*turn_id*/ None,
            format!("{}:0", client.state.thread_id),
            /*parent_thread_id*/ None,
            super::TestCodexResponsesRequestKind::Turn,
        ),
        /*include_internal*/ false,
    )?)
}

fn lookup_tool(description: &str) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: "lookup".to_string(),
        description: description.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::default(),
        output_schema: None,
    })
}

fn continuation(mut request: ResponsesApiRequest) -> ResponsesApiRequest {
    request.previous_response_id = Some("resp-1".to_string());
    request
}

#[test]
fn tools_are_omitted_only_on_unchanged_continuations() -> anyhow::Result<()> {
    let client = test_model_client(SessionSource::Cli);
    let input = vec![user_message("hello")?];
    let tools = vec![lookup_tool("Find an item")];
    let first = build_request(&client, tools.clone(), input.clone())?;
    let baseline = first.tools.as_ref().and_then(WireToolsHash::of);
    assert!(baseline.is_some());

    // Disabled (the default): byte-identical bodies, even for unchanged continuations.
    for request in [first.clone(), continuation(first.clone())] {
        let expected = serde_json::to_vec(&request)?;
        let mut sent = request;
        let (wire_tools, omitted) = omit_inherited_tools(
            /*inherits_tools_on_continuation*/ false, &mut sent, baseline,
        );
        assert_eq!((wire_tools, omitted), (baseline, false));
        assert_eq!(serde_json::to_vec(&sent)?, expected);
    }

    let changed = build_request(&client, vec![lookup_tool("Find a record")], input)?;
    for (scenario, request, previous, expect_omitted) in [
        ("first request", first.clone(), baseline, false),
        (
            "no baseline after error",
            continuation(first.clone()),
            None,
            false,
        ),
        ("changed tools", continuation(changed), baseline, false),
        (
            "unchanged continuation",
            continuation(first.clone()),
            baseline,
            true,
        ),
    ] {
        let mut sent = request.clone();
        let (wire_tools, omitted) = omit_inherited_tools(
            /*inherits_tools_on_continuation*/ true, &mut sent, previous,
        );
        assert_eq!(omitted, expect_omitted, "{scenario}");
        assert_eq!(
            wire_tools,
            request.tools.as_ref().and_then(WireToolsHash::of),
            "{scenario}: the baseline tracks the full contract"
        );
        let body = serde_json::to_value(&sent)?;
        let mut expected = serde_json::to_value(&request)?;
        if expect_omitted {
            expected
                .as_object_mut()
                .expect("request object")
                .remove("tools");
        }
        assert_eq!(body, expected, "{scenario}: only `tools` may change");
        assert_eq!(body["tool_choice"], json!("auto"), "{scenario}");
        assert!(body["parallel_tool_calls"].is_boolean(), "{scenario}");
    }
    Ok(())
}

#[test]
fn provider_request_compression_defaults_and_overrides() {
    let base = test_model_client(SessionSource::Cli);
    let remote = "https://example.com/v1";
    // Unique per test so the process-wide negotiation state starts empty.
    let local = "http://10.254.0.1:9/v1/compression-defaults";
    for (provider_kind, configured, endpoint, expected) in [
        // Other custom providers, local or remote, keep plain bodies unless configured.
        ("custom", None, remote, Compression::None),
        ("custom", None, local, Compression::None),
        (
            "custom",
            Some(RequestCompression::Zstd),
            remote,
            Compression::Zstd,
        ),
        (
            "custom",
            Some(RequestCompression::Auto),
            local,
            Compression::Zstd,
        ),
        // MultiDex providers (served by Lite) negotiate by default.
        ("multidex", None, local, Compression::Zstd),
        (
            "multidex",
            Some(RequestCompression::None),
            local,
            Compression::None,
        ),
        // OpenAI keeps its existing gating (no ChatGPT auth here), even on a local URL.
        ("openai", None, local, Compression::None),
        ("openai", None, remote, Compression::None),
    ] {
        let mut provider = if provider_kind == "openai" {
            codex_model_provider_info::ModelProviderInfo::create_openai_provider(Some(
                endpoint.to_string(),
            ))
        } else {
            base.provider_info().clone()
        };
        provider.multidex_compatibility = provider_kind == "multidex";
        provider.request_compression = configured;
        let client =
            base.with_provider(create_model_provider(provider, /*auth_manager*/ None));
        assert_eq!(
            ResponsesHttpExecutor::new(&client)
                .responses_request_compression(/*auth*/ None, endpoint),
            expected,
            "{provider_kind} {configured:?} {endpoint}"
        );
    }
}

#[test]
fn production_sized_continuation_savings() -> anyhow::Result<()> {
    let client = test_model_client(SessionSource::Cli);
    let full = continuation(build_request(
        &client,
        production_sized_tools()?,
        vec![user_message("run the tests and summarize failures")?],
    )?);
    let tools_bytes = serde_json::to_vec(full.tools.as_ref().expect("tools"))?.len();
    let baseline = full.tools.as_ref().and_then(WireToolsHash::of);
    let mut omitted = full.clone();
    let (_, was_omitted) = omit_inherited_tools(
        /*inherits_tools_on_continuation*/ true,
        &mut omitted,
        baseline,
    );
    assert!(was_omitted);

    let full_body = serde_json::to_vec(&full)?;
    let omitted_body = serde_json::to_vec(&omitted)?;
    let compress = |body: &[u8]| -> anyhow::Result<Vec<u8>> {
        Ok(zstd::stream::encode_all(std::io::Cursor::new(body), 0)?)
    };
    let full_zstd = compress(&full_body)?;
    let omitted_zstd = compress(&omitted_body)?;
    println!(
        "{PRODUCTION_TOOL_COUNT}-tool continuation: tools JSON {tools_bytes} B; body {} B \
         -> {} B with inherited tools; zstd {} B (full tools) / {} B (inherited tools)",
        full_body.len(),
        omitted_body.len(),
        full_zstd.len(),
        omitted_zstd.len(),
    );
    assert!(
        tools_bytes > 350_000,
        "tools JSON should be production sized"
    );
    assert_eq!(
        full_body.len() - omitted_body.len(),
        tools_bytes + r#","tools":"#.len()
    );
    assert!(omitted_body.len() * 15 < full_body.len());
    assert!(full_zstd.len() * 3 < full_body.len());
    assert_eq!(zstd::stream::decode_all(full_zstd.as_slice())?, full_body);
    Ok(())
}
