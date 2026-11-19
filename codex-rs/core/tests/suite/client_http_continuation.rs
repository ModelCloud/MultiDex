use codex_core::TurnInputRequest;
use codex_model_provider_info::MULTIDEX_PROVIDER_ID;
use codex_model_provider_info::ToolInheritance;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_model_provider_info::merge_configured_model_providers;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::sse;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

#[test_case::test_case(false; "standard")]
#[test_case::test_case(true; "responses_lite")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn responses_http_continuation_survives_user_turns(
    responses_lite: bool,
) -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let response_mock = mount_response_sequence(
        &server,
        [
            (
                "turn-one-routing",
                vec![
                    ev_response_created("resp-1"),
                    ev_function_call("call-1", "unsupported_tool", "{}"),
                    ev_completed("resp-1"),
                ],
            ),
            (
                "turn-one-routing",
                vec![
                    ev_response_created("resp-2"),
                    ev_assistant_message("msg-1", "first answer"),
                    ev_completed("resp-2"),
                ],
            ),
            (
                "turn-two-routing",
                vec![
                    ev_response_created("resp-3"),
                    ev_function_call("call-2", "unsupported_tool", "{}"),
                    ev_completed("resp-3"),
                ],
            ),
            (
                "turn-two-routing",
                vec![
                    ev_response_created("resp-4"),
                    ev_assistant_message("msg-2", "second answer"),
                    ev_completed("resp-4"),
                ],
            ),
        ]
        .into_iter()
        .map(|(turn_state, events)| {
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-turn-state", turn_state)
                .set_body_string(sse(events))
        })
        .collect(),
    )
    .await;
    let mut providers = merge_configured_model_providers(
        Default::default(),
        std::collections::HashMap::from([(
            MULTIDEX_PROVIDER_ID.to_string(),
            create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses),
        )]),
    )
    .expect("MultiDex provider should normalize");
    let provider = providers.remove(MULTIDEX_PROVIDER_ID).unwrap();
    let test = test_codex()
        .with_model_info_override("DS v4.1 Flash 1017", move |model_info| {
            model_info.use_responses_lite = responses_lite;
        })
        .with_config(move |config| {
            config.model_provider_id = MULTIDEX_PROVIDER_ID.to_string();
            config
                .model_providers
                .insert(MULTIDEX_PROVIDER_ID.to_string(), provider.clone());
            config.model_provider = provider;
            config.base_instructions = Some("Stable base instructions".to_string());
        })
        .build_with_auto_env(&server)
        .await?;

    for text in ["turn one", "turn two"] {
        test.codex
            .start_or_steer_turn(
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }])
                .with_thread_settings(ThreadSettingsOverrides {
                    model: Some("DS v4.1 Flash 1017".to_string()),
                    ..Default::default()
                }),
            )
            .await?;
        let completed = wait_for_event_match(&test.codex, |event| match event {
            EventMsg::TurnComplete(completed) => Some(completed.clone()),
            _ => None,
        })
        .await;
        assert_eq!(completed.error, None, "MultiDex turn {text} failed");
    }

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 4);
    let first_input = requests[0].input();
    let instructions_index = usize::from(responses_lite);
    assert_eq!(first_input[instructions_index]["role"], "developer");
    assert_eq!(
        first_input[instructions_index]["content"],
        json!([{"type": "input_text", "text": "Stable base instructions"}]),
    );
    let first_body = requests[0].body_json();
    let tools = if responses_lite {
        assert_eq!(first_input[0]["type"], "additional_tools");
        first_input[0]["tools"].as_array().unwrap()
    } else {
        first_body["tools"].as_array().unwrap()
    };
    assert!(!tools.is_empty());
    if !responses_lite {
        assert!(tools.iter().all(|tool| tool["type"] != "namespace"));
    }
    assert_eq!(
        requests
            .iter()
            .map(|request| request.body_json()["previous_response_id"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!(null),
            json!("resp-1"),
            json!("resp-2"),
            json!("resp-3")
        ],
    );
    assert_eq!(
        requests
            .iter()
            .map(|request| request.header("x-codex-turn-state"))
            .collect::<Vec<_>>(),
        vec![
            None,
            Some("turn-one-routing".to_string()),
            None,
            Some("turn-two-routing".to_string()),
        ],
    );
    // MultiDex providers negotiate zstd request bodies by default; this endpoint accepts them.
    // Without `inherits_tools_on_continuation`, every request declares the same tools.
    for request in &requests {
        assert_eq!(request.header("content-encoding").as_deref(), Some("zstd"));
        if !responses_lite {
            assert_eq!(request.body_json()["tools"], first_body["tools"]);
        }
    }
    assert_eq!(requests[2].message_input_texts("user"), vec!["turn two"]);
    assert_eq!(requests[2].input().len(), 1);
    for (request, call_id) in [(&requests[1], "call-1"), (&requests[3], "call-2")] {
        assert_eq!(request.input().len(), 1);
        assert_eq!(request.input()[0]["type"], "function_call_output");
        assert_eq!(request.input()[0]["call_id"], call_id);
    }
    Ok(())
}

/// Headers Inference-Ultra Lite sends on every `/v1/` response.
const LITE_ACCEPT_ENCODING: &str = "zstd, gzip";
const LITE_CAPABILITIES: &str = "responses.previous_response_id/1, responses.tools-inheritance/1, request-encoding/1;codings=zstd+gzip, responses.stream-tools/1;mode=final";

#[derive(Clone, Copy, Debug)]
enum Advertise {
    Always,
    Never,
    FirstResponseOnly,
}

#[derive(Clone, Copy, Debug)]
struct FakeLite {
    advertise: Advertise,
    /// `response.completed` echoes a different tool list than the stored one.
    echo_wrong_tools: bool,
    /// The request at this index is rejected with Lite's `response_not_found` 404.
    not_found_at: Option<usize>,
}

/// A wiremock responder that implements Lite's tool inheritance: a continuation without `tools`
/// inherits the stored tools of `previous_response_id`, and `response.completed` echoes them.
fn fake_lite_responder(
    lite: FakeLite,
) -> impl Fn(&wiremock::Request) -> ResponseTemplate + Send + Sync + 'static {
    let state = std::sync::Mutex::new((0usize, 0usize, std::collections::HashMap::new()));
    move |request: &wiremock::Request| {
        let body = if request.headers.contains_key("content-encoding") {
            zstd::stream::decode_all(std::io::Cursor::new(&request.body)).expect("zstd body")
        } else {
            request.body.clone()
        };
        let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON body");
        let mut state = state.lock().unwrap();
        let (requests, responses, stored) = &mut *state;
        let index = *requests;
        *requests += 1;
        let advertise = match lite.advertise {
            Advertise::Always => true,
            Advertise::Never => false,
            Advertise::FirstResponseOnly => index == 0,
        };
        let with_headers = |template: ResponseTemplate| {
            let template = template.insert_header("accept-encoding", LITE_ACCEPT_ENCODING);
            if advertise {
                template.insert_header("x-inference-ultra-capabilities", LITE_CAPABILITIES)
            } else {
                template
            }
        };
        let previous = body["previous_response_id"].as_str();
        if lite.not_found_at == Some(index) || previous.is_some_and(|id| !stored.contains_key(id)) {
            return with_headers(ResponseTemplate::new(/*status*/ 404).set_body_raw(
                r#"{"error":{"message":"Previous response not found","type":"invalid_request_error","param":"previous_response_id","code":"response_not_found"}}"#,
                "application/json",
            ));
        }
        let tools = match body.get("tools") {
            Some(tools) => tools.clone(),
            None => previous
                .and_then(|id| stored.get(id).cloned())
                .unwrap_or_else(|| json!([])),
        };
        *responses += 1;
        let number = *responses;
        let id = format!("resp-{number}");
        stored.insert(id.clone(), tools.clone());
        let echoed = if lite.echo_wrong_tools {
            json!([])
        } else {
            tools
        };
        let output = if number % 2 == 1 {
            ev_function_call(&format!("call-{number}"), "unsupported_tool", "{}")
        } else {
            ev_assistant_message(&format!("msg-{number}"), "answer")
        };
        with_headers(
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    ev_response_created(&id),
                    output,
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": id,
                            "tools": echoed,
                            "usage": {"input_tokens":0,"input_tokens_details":null,"output_tokens":0,"output_tokens_details":null,"total_tokens":0}
                        }
                    }),
                ])),
        )
    }
}

/// Runs two user turns (each a function call and a final answer) against a fake Lite and returns
/// the decoded request bodies.
async fn run_fake_lite_turns(
    lite: FakeLite,
    inherits_tools: Option<bool>,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(fake_lite_responder(lite))
        .mount(&server)
        .await;
    let mut configured =
        create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses);
    if let Some(inherits_tools) = inherits_tools {
        configured.inherits_tools_on_continuation = if inherits_tools {
            ToolInheritance::Enabled
        } else {
            ToolInheritance::Disabled
        };
    }
    let mut providers = merge_configured_model_providers(
        Default::default(),
        std::collections::HashMap::from([(MULTIDEX_PROVIDER_ID.to_string(), configured)]),
    )
    .expect("MultiDex provider should normalize");
    let provider = providers.remove(MULTIDEX_PROVIDER_ID).unwrap();
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider_id = MULTIDEX_PROVIDER_ID.to_string();
            config
                .model_providers
                .insert(MULTIDEX_PROVIDER_ID.to_string(), provider.clone());
            config.model_provider = provider;
        })
        .build_with_auto_env(&server)
        .await?;

    for text in ["turn one", "turn two"] {
        test.codex
            .start_or_steer_turn(
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }])
                .with_thread_settings(ThreadSettingsOverrides {
                    model: Some("DS v4.1 Flash 1017".to_string()),
                    ..Default::default()
                }),
            )
            .await?;
        let completed = wait_for_event_match(&test.codex, |event| match event {
            EventMsg::TurnComplete(completed) => Some(completed.clone()),
            _ => None,
        })
        .await;
        assert_eq!(completed.error, None, "turn {text} failed");
    }

    let bodies = server
        .received_requests()
        .await
        .expect("request recording")
        .into_iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .map(|request| {
            let body = if request.headers.contains_key("content-encoding") {
                zstd::stream::decode_all(std::io::Cursor::new(&request.body)).expect("zstd body")
            } else {
                request.body
            };
            serde_json::from_slice::<serde_json::Value>(&body).expect("JSON body")
        })
        .collect::<Vec<_>>();
    let tools = bodies[0]["tools"].clone();
    assert!(
        !tools
            .as_array()
            .expect("first request declares tools")
            .is_empty()
    );
    for body in &bodies {
        if let Some(sent) = body.get("tools") {
            assert_eq!(sent, &tools, "tools are sent in full or not at all");
        }
        assert_eq!(body["tool_choice"], bodies[0]["tool_choice"]);
        assert_eq!(
            body["parallel_tool_calls"],
            bodies[0]["parallel_tool_calls"]
        );
    }
    Ok(bodies)
}

/// `(previous_response_id, tools sent)` for each request.
fn continuation_shape(bodies: &[serde_json::Value]) -> Vec<(serde_json::Value, bool)> {
    bodies
        .iter()
        .map(|body| {
            (
                body["previous_response_id"].clone(),
                body.get("tools").is_some(),
            )
        })
        .collect()
}

const ALWAYS_ADVERTISED: FakeLite = FakeLite {
    advertise: Advertise::Always,
    echo_wrong_tools: false,
    not_found_at: None,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_tool_inheritance_omits_tools_after_lite_advertises_it() -> anyhow::Result<()> {
    let bodies = run_fake_lite_turns(ALWAYS_ADVERTISED, /*inherits_tools*/ None).await?;
    assert_eq!(
        continuation_shape(&bodies),
        vec![
            // The first request always declares tools; its response advertises inheritance.
            (json!(null), true),
            (json!("resp-1"), false),
            (json!("resp-2"), false),
            (json!("resp-3"), false),
        ],
    );
    Ok(())
}

#[test_case::test_case(Advertise::Never, None; "auto_without_header")]
#[test_case::test_case(Advertise::Always, Some(false); "disabled_with_header")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_are_never_omitted_without_capability_or_when_disabled(
    advertise: Advertise,
    inherits_tools: Option<bool>,
) -> anyhow::Result<()> {
    let lite = FakeLite {
        advertise,
        ..ALWAYS_ADVERTISED
    };
    let bodies = run_fake_lite_turns(lite, inherits_tools).await?;
    assert_eq!(
        continuation_shape(&bodies),
        vec![
            (json!(null), true),
            (json!("resp-1"), true),
            (json!("resp-2"), true),
            (json!("resp-3"), true),
        ],
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabled_tool_inheritance_omits_without_capability() -> anyhow::Result<()> {
    let lite = FakeLite {
        advertise: Advertise::Never,
        ..ALWAYS_ADVERTISED
    };
    let bodies = run_fake_lite_turns(lite, /*inherits_tools*/ Some(true)).await?;
    assert_eq!(
        continuation_shape(&bodies),
        vec![
            (json!(null), true),
            (json!("resp-1"), false),
            (json!("resp-2"), false),
            (json!("resp-3"), false),
        ],
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_tool_inheritance_stops_when_capability_disappears() -> anyhow::Result<()> {
    let lite = FakeLite {
        advertise: Advertise::FirstResponseOnly,
        ..ALWAYS_ADVERTISED
    };
    let bodies = run_fake_lite_turns(lite, /*inherits_tools*/ None).await?;
    assert_eq!(
        continuation_shape(&bodies),
        vec![
            (json!(null), true),
            // resp-1 advertised inheritance.
            (json!("resp-1"), false),
            // resp-2 and later responses did not.
            (json!("resp-2"), true),
            (json!("resp-3"), true),
        ],
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_completed_tools_echo_disables_inheritance() -> anyhow::Result<()> {
    let lite = FakeLite {
        echo_wrong_tools: true,
        ..ALWAYS_ADVERTISED
    };
    let bodies = run_fake_lite_turns(lite, /*inherits_tools*/ None).await?;
    assert_eq!(
        continuation_shape(&bodies),
        vec![
            (json!(null), true),
            (json!("resp-1"), false),
            // resp-2 did not echo the inherited tools: full tools from now on.
            (json!("resp-2"), true),
            (json!("resp-3"), true),
        ],
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_not_found_resends_full_history_and_tools() -> anyhow::Result<()> {
    let lite = FakeLite {
        // The first continuation of turn two.
        not_found_at: Some(2),
        ..ALWAYS_ADVERTISED
    };
    let bodies = run_fake_lite_turns(lite, /*inherits_tools*/ None).await?;
    assert_eq!(
        continuation_shape(&bodies),
        vec![
            (json!(null), true),
            (json!("resp-1"), false),
            (json!("resp-2"), false),
            // Full input and tools without previous_response_id.
            (json!(null), true),
            (json!("resp-3"), false),
        ],
    );
    let full_input = bodies[3]["input"].to_string();
    assert!(full_input.contains("turn one") && full_input.contains("turn two"));
    Ok(())
}

/// A server that cannot decode compressed request bodies, like Lite before Inference-Ultra
/// 293d043f (400 "Invalid JSON body") or a server that rejects unknown codings (415).
#[test_case::test_case(400, json!({"error": {"message": "Invalid JSON body", "type": "invalid_request_error", "param": null, "code": null}}); "invalid_json_400")]
#[test_case::test_case(415, json!({"error": {"message": "Unsupported Content-Encoding", "type": "invalid_request_error", "param": null, "code": "unsupported_content_encoding"}}); "unsupported_coding_415")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_request_compression_falls_back_once_for_servers_without_zstd(
    status: u16,
    error: serde_json::Value,
) -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let responses = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let responder_responses = std::sync::Arc::clone(&responses);
    Mock::given(method("POST"))
        .and(path_regex(".*/responses$"))
        .respond_with(move |request: &wiremock::Request| {
            if request.headers.contains_key("content-encoding") {
                return ResponseTemplate::new(status).set_body_json(error.clone());
            }
            let index = responder_responses.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let id = format!("resp-{index}");
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    ev_response_created(&id),
                    ev_assistant_message(&format!("msg-{index}"), "answer"),
                    ev_completed(&id),
                ]))
        })
        .mount(&server)
        .await;
    let mut providers = merge_configured_model_providers(
        Default::default(),
        std::collections::HashMap::from([(
            MULTIDEX_PROVIDER_ID.to_string(),
            create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses),
        )]),
    )
    .expect("MultiDex provider should normalize");
    let provider = providers.remove(MULTIDEX_PROVIDER_ID).unwrap();
    assert_eq!(provider.request_compression, None, "auto is the default");
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider_id = MULTIDEX_PROVIDER_ID.to_string();
            config
                .model_providers
                .insert(MULTIDEX_PROVIDER_ID.to_string(), provider.clone());
            config.model_provider = provider;
        })
        .build_with_auto_env(&server)
        .await?;

    for text in ["turn one", "turn two"] {
        test.codex
            .start_or_steer_turn(
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }])
                .with_thread_settings(ThreadSettingsOverrides {
                    model: Some("DS v4.1 Flash 1017".to_string()),
                    ..Default::default()
                }),
            )
            .await?;
        let completed = wait_for_event_match(&test.codex, |event| match event {
            EventMsg::TurnComplete(completed) => Some(completed.clone()),
            _ => None,
        })
        .await;
        assert_eq!(
            completed.error, None,
            "turn {text} must not surface the fallback"
        );
    }

    let requests = server
        .received_requests()
        .await
        .expect("request recording")
        .into_iter()
        .filter(|request| request.url.path().ends_with("/responses"))
        .collect::<Vec<_>>();
    // One rejected zstd probe, its uncompressed resend, then plain bodies for later turns.
    assert_eq!(
        requests
            .iter()
            .map(|request| request
                .headers
                .get("content-encoding")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string))
            .collect::<Vec<_>>(),
        vec![Some("zstd".to_string()), None, None],
    );
    let probe: serde_json::Value = serde_json::from_slice(&zstd::stream::decode_all(
        std::io::Cursor::new(&requests[0].body),
    )?)?;
    let resend: serde_json::Value = serde_json::from_slice(&requests[1].body)?;
    assert_eq!(resend, probe, "the fallback resends the identical request");
    let next: serde_json::Value = serde_json::from_slice(&requests[2].body)?;
    assert_eq!(next["previous_response_id"], json!("resp-1"));
    Ok(())
}
