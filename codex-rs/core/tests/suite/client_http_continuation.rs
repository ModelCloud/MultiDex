use codex_core::TurnInputRequest;
use codex_model_provider_info::MULTIDEX_PROVIDER_ID;
use codex_model_provider_info::RequestCompression;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lite_provider_inherits_unchanged_tools_and_compresses_requests() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let sse_response = |events| {
        ResponseTemplate::new(/*status*/ 200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse(events))
    };
    let response_mock = mount_response_sequence(
        &server,
        vec![
            sse_response(vec![
                ev_response_created("resp-1"),
                ev_function_call("call-1", "unsupported_tool", "{}"),
                ev_completed("resp-1"),
            ]),
            sse_response(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-1", "first answer"),
                ev_completed("resp-2"),
            ]),
            // The server lost resp-2 (for example after a restart), so the continuation that
            // relied on its stored tools is rejected.
            ResponseTemplate::new(/*status*/ 404).set_body_json(json!({
                "error": {
                    "message": "Previous response not found",
                    "type": "invalid_request_error",
                    "param": "previous_response_id",
                    "code": "response_not_found",
                }
            })),
            sse_response(vec![
                ev_response_created("resp-3"),
                ev_function_call("call-2", "unsupported_tool", "{}"),
                ev_completed("resp-3"),
            ]),
            sse_response(vec![
                ev_response_created("resp-4"),
                ev_assistant_message("msg-2", "second answer"),
                ev_completed("resp-4"),
            ]),
        ],
    )
    .await;
    let mut configured =
        create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses);
    configured.request_compression = Some(RequestCompression::Zstd);
    configured.inherits_tools_on_continuation = true;
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
        assert_eq!(completed.error, None, "MultiDex turn {text} failed");
    }

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 5);
    let bodies = requests
        .iter()
        .map(|request| {
            assert_eq!(request.header("content-encoding").as_deref(), Some("zstd"));
            let decoded = zstd::stream::decode_all(std::io::Cursor::new(request.body_bytes()))
                .expect("request body should be zstd");
            serde_json::from_slice::<serde_json::Value>(&decoded).expect("JSON body")
        })
        .collect::<Vec<_>>();
    let tools = bodies[0]["tools"].clone();
    assert!(!tools.as_array().expect("tools array").is_empty());
    assert_eq!(
        bodies
            .iter()
            .map(|body| (
                body["previous_response_id"].clone(),
                body.get("tools").is_some()
            ))
            .collect::<Vec<_>>(),
        vec![
            (json!(null), true),
            (json!("resp-1"), false),
            (json!("resp-2"), false),
            // After the rejection, the full history and full tools are resent.
            (json!(null), true),
            (json!("resp-3"), false),
        ],
    );
    assert_eq!(bodies[3]["tools"], tools);
    // The resend replays the full history, including both user turns.
    let full_input = bodies[3]["input"].to_string();
    assert!(full_input.contains("turn one") && full_input.contains("turn two"));
    assert!(!bodies[2]["input"].to_string().contains("turn one"));
    for body in &bodies {
        assert_eq!(body["tool_choice"], bodies[0]["tool_choice"]);
        assert_eq!(
            body["parallel_tool_calls"],
            bodies[0]["parallel_tool_calls"]
        );
    }
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
