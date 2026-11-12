use super::*;
use codex_login::CodexAuth;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::built_in_model_providers;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use core_test_support::responses;
use pretty_assertions::assert_eq;

#[test_case::test_case(false; "matching auth owner reuses pending work")]
#[test_case::test_case(true; "changed auth owner discards pending work")]
#[tokio::test]
async fn multidex_base_client_does_not_override_background_turn_provider(change_auth_owner: bool) {
    let local_server = responses::start_mock_server().await;
    let openai_server = responses::start_mock_server().await;
    let mut provider = built_in_model_providers(None)["openai"].clone();
    provider.base_url = Some(format!("{}/v1", openai_server.uri()));
    provider.supports_websockets = false;
    let server_uri = openai_server.uri();
    let (mut sess, turn, _events) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            Vec::new(),
            move |config| {
                config.model = Some("gpt-5.2".to_string());
                config.model_context_window = Some(100_000);
                config.model_auto_compact_token_limit = Some(90_000);
                config.model_provider = provider;
                config.chatgpt_base_url = server_uri;
                config.async_compaction_threshold_percent = 50;
                config.features.enable(Feature::AsyncCompaction).unwrap();
                config.features.disable(Feature::TokenBudget).unwrap();
            },
        )
        .await;
    let mut local_provider = create_oss_provider_with_base_url(
        &format!("{}/v1", local_server.uri()),
        WireApi::Responses,
    );
    local_provider.multidex_compatibility = true;
    let local_client = sess
        .services
        .model_client
        .with_provider(create_model_provider(local_provider, None));
    Arc::get_mut(&mut sess)
        .expect("unshared session")
        .services
        .model_client = local_client;
    let base_auth_owner = sess.services.model_client.auth_owner();
    assert_eq!(base_auth_owner, None);
    let model_client = sess.model_client_for_provider(&turn.provider).await;
    let auth_owner = model_client.auth_owner();
    assert!(auth_owner.is_some());

    let request = responses::mount_sse_once(
        &openai_server,
        responses::sse(vec![
            serde_json::json!({"type":"response.output_item.done", "item": {
                "type":"compaction", "encrypted_content":"openai-summary"
            }}),
            responses::ev_completed("prepared"),
        ]),
    )
    .await;
    let cancellation = CancellationToken::new();
    let step = sess
        .capture_step_context(Arc::clone(&turn), &cancellation)
        .await
        .expect("capture step");
    let original: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "message", "role": "user",
        "content": [{"type": "input_text", "text": "original turn"}]
    }))
    .expect("user message");
    sess.replace_history(vec![original], None).await;
    sess.update_token_usage_info(
        &turn,
        Some(&TokenUsage {
            input_tokens: 70_000,
            total_tokens: 70_000,
            ..Default::default()
        }),
        /*includes_reasoning*/ false,
    )
    .await
    .expect("record usage");
    let status = super::super::context_window::context_window_token_status(&sess, &turn).await;
    let world_state = Arc::new(WorldState::default());
    maybe_start(
        &sess,
        &step,
        &world_state,
        CompactionPhase::MidTurn,
        &cancellation,
        &status,
        (Some("routing".to_string()), base_auth_owner),
    )
    .await;
    assert!(
        sess.state
            .lock()
            .await
            .async_compaction
            .last_attempt
            .is_none()
    );

    maybe_start(
        &sess,
        &step,
        &world_state,
        CompactionPhase::MidTurn,
        &cancellation,
        &status,
        (Some("routing".to_string()), auth_owner.clone()),
    )
    .await;
    wait_for_background_compaction_for_test(&sess).await;
    let sent = request.single_request();
    assert_eq!(sent.body_json()["model"], "gpt-5.2");
    assert!(sent.header("authorization").is_some());
    assert_eq!(
        sent.header("x-codex-turn-state").as_deref(),
        Some("routing")
    );
    assert!(local_server.received_requests().await.unwrap().is_empty());

    if change_auth_owner {
        sess.services.auth_manager.reload().await;
        assert_ne!(model_client.auth_owner(), auth_owner);
    }
    let candidate = wait_for_pending(&sess, &step).await;
    assert_eq!(candidate.is_some(), !change_auth_owner);
    if let Some(candidate) = candidate {
        assert_eq!(candidate.prefix.auth_owner, auth_owner);
    }
    shutdown(&sess).await;
}
