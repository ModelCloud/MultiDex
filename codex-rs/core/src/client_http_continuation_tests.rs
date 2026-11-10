use super::super::HttpResumePoint;
use super::super::LastResponse;
use super::Prompt;
use super::test_model_client;
use super::test_model_info;
use super::test_responses_metadata_for_client;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test_case::test_case(false; "standard")]
#[test_case::test_case(true; "responses_lite")]
fn http_continuation_retains_pending_completion_and_checks_reuse_boundaries(
    responses_lite: bool,
) -> anyhow::Result<()> {
    for scenario in [
        "same_provider",
        "other_provider",
        "other_owner",
        "other_model",
        "edited_history",
        "changed_instructions",
        "changed_tools",
    ] {
        let base = test_model_client(SessionSource::Cli);
        let mut provider = base.provider_info().clone();
        provider.supports_responses_continuation = true;
        let client =
            base.with_provider(create_model_provider(provider, /*auth_manager*/ None));
        let first: ResponseItem = serde_json::from_value(json!({
            "type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "first prompt"}],
        }))?;
        let next: ResponseItem = serde_json::from_value(json!({
            "type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "next prompt"}],
        }))?;
        let mut model = test_model_info();
        model.use_responses_lite = responses_lite;
        let mut tool = ResponsesApiTool {
            name: "lookup".to_string(),
            description: "Find an item".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::default(),
            output_schema: None,
        };
        let mut prompt = Prompt {
            input: vec![first],
            tools: vec![ToolSpec::Function(tool.clone())].into(),
            base_instructions: BaseInstructions {
                text: "Stable base instructions".to_string(),
                provenance: None,
            },
            ..Default::default()
        };
        let build = |prompt: &Prompt| {
            client.build_responses_request(
                prompt,
                &model,
                /*effort*/ None,
                ReasoningSummary::None,
                /*service_tier*/ None,
                &test_responses_metadata_for_client(
                    &client,
                    /*turn_id*/ None,
                    format!("{}:0", client.state.thread_id),
                    /*parent_thread_id*/ None,
                    super::TestCodexResponsesRequestKind::Turn,
                ),
                /*include_internal*/ false,
            )
        };
        let previous = build(&prompt)?;
        prompt.input.push(next.clone());
        if scenario == "changed_instructions" {
            prompt.base_instructions.text.push_str(" with an update");
        }
        if scenario == "changed_tools" {
            tool.description.push_str(" with an update");
            prompt.tools = vec![ToolSpec::Function(tool)].into();
        }
        if scenario == "edited_history" {
            prompt.input[0] = next.clone();
        }
        // Rebuild the prefix exactly as the next request does, so its generated IDs
        // and instructions must remain compatible with the stored response.
        let mut current = build(&prompt)?;
        let mut turn = client.new_session();
        turn.http_session.last_request = Some(previous);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        turn.http_session.last_response_rx = Some(receiver);
        assert!(turn.get_last_http_response().is_none());
        if scenario == "other_owner" {
            let manager = AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            );
            turn.http_session.auth_owner = Some(manager.auth_owner());
        }
        drop(turn);
        sender
            .send(LastResponse {
                response_id: "resp-1".to_string(),
                items_added: Vec::new(),
            })
            .unwrap();

        let next_client = if scenario == "other_provider" {
            let mut provider = create_oss_provider_with_base_url(
                "https://other.example.com/v1",
                WireApi::Responses,
            );
            provider.supports_responses_continuation = true;
            client.with_provider(create_model_provider(provider, /*auth_manager*/ None))
        } else {
            client.with_provider(client.state.provider.clone())
        };
        if scenario == "other_model" {
            current.model = "another-model".to_string();
        }
        let mut turn = next_client.new_session();
        assert_eq!(
            turn.prepare_http_request(&current)
                .map(|continuation| (continuation.response_id, continuation.items)),
            (scenario == "same_provider").then_some(("resp-1".to_string(), vec![next])),
            "{scenario}",
        );
    }
    Ok(())
}

/// A MultiDex steer preempts the live HTTP response before the provider stores
/// it. The replacement request must resume from the last stored response
/// instead of resending the whole conversation, and must replay the assistant
/// text that was already streamed to the client.
#[test_case::test_case(false; "standard")]
#[test_case::test_case(true; "responses_lite")]
fn http_continuation_resumes_from_last_stored_response_after_steer_preemption(
    responses_lite: bool,
) -> anyhow::Result<()> {
    let base = test_model_client(SessionSource::Cli);
    let mut provider = base.provider_info().clone();
    provider.supports_responses_continuation = true;
    let client = base.with_provider(create_model_provider(provider, /*auth_manager*/ None));

    let message = |role: &str, text: &str| -> anyhow::Result<ResponseItem> {
        let content_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        Ok(serde_json::from_value(json!({
            "type": "message", "role": role,
            "content": [{"type": content_type, "text": text}],
        }))?)
    };
    let user_prompt = message("user", "hello")?;
    let stored_answer = message("assistant", "stored answer")?;
    let streamed_partial = message("assistant", "partial answer")?;
    let steer_prompt = message("user", "steer")?;

    let mut model = test_model_info();
    model.use_responses_lite = responses_lite;
    let build = |input: Vec<ResponseItem>| -> anyhow::Result<super::super::ResponsesApiRequest> {
        Ok(client.build_responses_request(
            &Prompt {
                input,
                ..Default::default()
            },
            &model,
            /*effort*/ None,
            ReasoningSummary::None,
            /*service_tier*/ None,
            &test_responses_metadata_for_client(
                &client,
                /*turn_id*/ None,
                format!("{}:0", client.state.thread_id),
                /*parent_thread_id*/ None,
                super::TestCodexResponsesRequestKind::Turn,
            ),
            /*include_internal*/ false,
        )?)
    };

    // The stored response the preempted request was extending.
    let stored_request = build(vec![user_prompt.clone()])?;
    // The preempted request had already streamed a partial answer, and the
    // steer prompt was appended before the follow-up request was built.
    let follow_up_request = build(vec![
        user_prompt,
        stored_answer.clone(),
        streamed_partial.clone(),
        steer_prompt.clone(),
    ])?;

    let mut turn = client.new_session();
    turn.http_session.resume_point = Some(HttpResumePoint {
        request: stored_request,
        response: LastResponse {
            response_id: "resp-stored".to_string(),
            items_added: vec![stored_answer],
        },
    });
    // The preempted request replaced the live continuation state and its
    // response never resolved.
    turn.http_session.last_request = Some(follow_up_request.clone());
    turn.http_session.last_response = None;
    let (_sender, receiver) = tokio::sync::oneshot::channel();
    turn.http_session.last_response_rx = Some(receiver);

    assert!(
        turn.resume_http_continuation(),
        "a preempted continuation must resume from the last stored response"
    );
    assert_eq!(
        turn.prepare_http_request(&follow_up_request)
            .map(|continuation| (continuation.response_id, continuation.items)),
        Some((
            "resp-stored".to_string(),
            vec![streamed_partial, steer_prompt]
        )),
    );
    Ok(())
}
