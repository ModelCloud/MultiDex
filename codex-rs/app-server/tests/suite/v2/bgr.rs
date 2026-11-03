use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use codex_app_server_protocol::*;
use codex_features::Feature;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(20);

async fn rpc<T: DeserializeOwned>(
    mcp: &mut TestAppServer,
    method: &str,
    params: Value,
) -> Result<T> {
    let id = mcp.send_raw_request(method, Some(params)).await?;
    Ok(timeout(TIMEOUT, mcp.read_response(id)).await??)
}

fn identity(thread: &str, role: BgrRole) -> BgrIdentity {
    BgrIdentity {
        team_id: "0".repeat(32),
        agent_id: if role == BgrRole::Brain { "a" } else { "b" }.repeat(32),
        session_id: thread.into(),
        task_id: "3".repeat(32),
        attempt_id: "4".repeat(32),
        epoch: 1,
        role,
    }
}

#[tokio::test]
async fn bgr_status_receipt_does_not_interrupt_tool_and_reply_continues_same_turn() -> Result<()> {
    bgr_status_round_trip(
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        false,
        false,
    )
    .await
}

#[tokio::test]
async fn bgr_status_reply_preserves_null_query_anchor_with_observed_head() -> Result<()> {
    bgr_status_round_trip(None, false, false).await
}

#[tokio::test]
async fn bgr_external_brain_status_reply_preserves_query_anchor_at_safe_boundary() -> Result<()> {
    bgr_status_round_trip(
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        true,
        false,
    )
    .await
}

#[tokio::test]
async fn bgr_external_brain_status_reply_preserves_null_anchor_at_safe_boundary() -> Result<()> {
    bgr_status_round_trip(None, true, false).await
}

#[tokio::test]
async fn bgr_cancel_while_busy_does_not_wake_queued_query() -> Result<()> {
    bgr_status_round_trip(None, true, true).await
}

#[tokio::test]
async fn bgr_cancel_idle_is_idempotent_and_rejects_new_queries() -> Result<()> {
    let home = TempDir::new()?;
    let server = create_mock_responses_server_sequence_unchecked(vec![
        create_final_assistant_message_sse_response("Unexpected wake.")?,
    ])
    .await;
    MockResponsesConfig::new(&server.uri())
        .disable_feature(Feature::Collab)
        .disable_feature(Feature::MultiAgentV2)
        .enable_feature(Feature::DeferMailboxPreemption)
        .write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_auto_env()
        .build()
        .await?;
    mcp.initialize_with_capabilities(
        ClientInfo {
            name: "bgr-idle-cancel-test".into(),
            title: None,
            version: "1".into(),
        },
        Some(InitializeCapabilities {
            experimental_api: true,
            ..Default::default()
        }),
    )
    .await?;
    let capabilities: BgrCapabilitiesReadResponse =
        rpc(&mut mcp, "bgr/capabilities/read", json!({})).await?;
    let worker: ThreadStartResponse = rpc(
        &mut mcp,
        "thread/start",
        json!({"dynamicTools": [capabilities.report_tool]}),
    )
    .await?;
    let sender = BgrSender::from(&identity("brain", BgrRole::Brain));
    let identity = identity("worker", BgrRole::Grunt);
    rpc::<BgrThreadStateResponse>(
        &mut mcp,
        "bgr/thread/bind",
        json!({"threadId": worker.thread.id, "identity": identity}),
    )
    .await?;
    let params = json!({"threadId": worker.thread.id, "identity": identity});
    let first: BgrThreadStateResponse = rpc(&mut mcp, "bgr/thread/cancel", params.clone()).await?;
    let second: BgrThreadStateResponse = rpc(&mut mcp, "bgr/thread/cancel", params).await?;
    assert!(first.snapshot.cancelled);
    assert_eq!(first.snapshot, second.snapshot);
    let id = mcp.send_raw_request("bgr/message/deliver", Some(json!({"threadId": worker.thread.id, "sender": sender, "envelope": {"version": 1, "messageId": "cancelled-query", "senderAgentId": sender.agent_id, "recipientAgentId": identity.agent_id, "identity": identity, "kind": "status_query", "correlationId": "cancelled-correlation", "payload": {"scope": "now"}}}))).await?;
    let rejected = timeout(
        TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    assert!(rejected.error.message.contains("cancelled"));
    assert!(
        timeout(
            Duration::from_millis(500),
            mcp.read_notification::<TurnStartedNotification>("turn/started")
        )
        .await
        .is_err()
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    Ok(())
}

async fn bgr_status_round_trip(
    query_head_sha: Option<&str>,
    external_brain: bool,
    cancel_while_busy: bool,
) -> Result<()> {
    let query_id = "c".repeat(32);
    let reply_id = "d".repeat(32);
    let correlation_id = "e".repeat(32);
    let observed_head_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let status = json!({
        "messageId": reply_id, "inReplyTo": query_id,
        "payload": { "currentAction": "continuing task", "completed": ["held tool"],
            "pending": ["assignment remains active"], "blockers": [], "headSha": observed_head_sha,
            "dirty": null, "coverageLimits": ["self-reported current state"] }
    });
    let server = create_mock_responses_server_sequence_unchecked(vec![
        responses::sse(vec![
            responses::ev_response_created("response-one"),
            responses::ev_function_call("hold-call", "hold_work", "{}"),
            responses::ev_completed("response-one"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("response-two"),
            responses::ev_function_call("report-call", BGR_REPORT_TOOL, &status.to_string()),
            responses::ev_completed("response-two"),
        ]),
        create_final_assistant_message_sse_response(
            "Continuing the same assignment after reporting.",
        )?,
    ])
    .await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .disable_feature(Feature::Collab)
        .disable_feature(Feature::MultiAgentV2)
        .enable_feature(Feature::DeferMailboxPreemption)
        .write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(TIMEOUT, mcp.initialize()).await??;
    let capabilities: BgrCapabilitiesReadResponse =
        rpc(&mut mcp, "bgr/capabilities/read", json!({})).await?;
    assert_eq!(capabilities.status_scopes, vec!["now"]);
    assert!(!capabilities.reviewer_sandbox);
    assert!(!capabilities.durable_mailbox);
    assert!(!capabilities.restart_recovery);
    assert!(capabilities.host_delivery);
    assert!(!capabilities.cross_host_delivery);

    let brain: Option<ThreadStartResponse> = if external_brain {
        None
    } else {
        Some(rpc(&mut mcp, "thread/start", json!({})).await?)
    };
    let worker: ThreadStartResponse = rpc(&mut mcp, "thread/start", json!({"dynamicTools": [
        capabilities.report_tool,
        {"type": "function", "name": "hold_work", "description": "Work which must not be interrupted", "inputSchema": {"type": "object", "properties": {}}}
    ]})).await?;
    let brain_sender = BgrSender {
        team_id: "0".repeat(32),
        agent_id: "a".repeat(32),
        session_id: "1".repeat(32),
        epoch: 1,
        role: BgrRole::Brain,
    };
    let worker_identity = identity(&"2".repeat(32), BgrRole::Grunt);
    assert_ne!(worker_identity.session_id, worker.thread.id);
    if let Some(brain) = &brain {
        let _: BgrThreadStateResponse = rpc(
            &mut mcp,
            "bgr/thread/bind",
            json!({"threadId": brain.thread.id, "identity": identity(&brain_sender.session_id, BgrRole::Brain)}),
        )
        .await?;
    } else {
        let threads: ThreadLoadedListResponse =
            rpc(&mut mcp, "thread/loaded/list", json!({})).await?;
        assert_eq!(threads.data, vec![worker.thread.id.clone()]);
    }
    let _: BgrThreadStateResponse = rpc(
        &mut mcp,
        "bgr/thread/bind",
        json!({"threadId": worker.thread.id, "identity": worker_identity}),
    )
    .await?;
    let turn: TurnStartResponse = rpc(&mut mcp, "turn/start", json!({"threadId": worker.thread.id,
        "input": [{"type": "text", "text": "Perform the assigned task; call hold_work, report status when asked, then continue."}]})).await?;
    let request = timeout(TIMEOUT, mcp.read_stream_until_request_message()).await??;
    let ServerRequest::DynamicToolCall { request_id, params } = request else {
        panic!("expected held work tool")
    };
    assert_eq!(params.tool, "hold_work");
    assert_eq!(params.turn_id, turn.turn.id);

    let envelope = BgrEnvelope {
        version: BGR_VERSION,
        message_id: query_id.clone(),
        sender_agent_id: brain_sender.agent_id.clone(),
        recipient_agent_id: worker_identity.agent_id.clone(),
        identity: worker_identity.clone(),
        kind: BgrMessageKind::StatusQuery,
        correlation_id: correlation_id.clone(),
        in_reply_to: None,
        payload: json!({"scope": "now"}),
        head_sha: query_head_sha.map(str::to_owned),
    };
    let (method, send) = if let Some(brain) = &brain {
        (
            "bgr/message/send",
            json!({"sourceThreadId": brain.thread.id, "threadId": worker.thread.id, "envelope": envelope}),
        )
    } else {
        let send = json!({"threadId": worker.thread.id,
            "sender": brain_sender, "envelope": envelope});
        let mut stale_identity = worker_identity.clone();
        stale_identity.attempt_id = "stale-attempt".into();
        for (field, value) in [
            ("agentId", json!("spoofed-agent")),
            ("teamId", json!("other-team")),
            ("epoch", json!(2)),
            ("role", json!("grunt")),
            ("sessionId", json!("")),
            ("taskId", json!("not-a-sender-field")),
        ] {
            let mut invalid = send.clone();
            invalid["sender"][field] = value;
            let id = mcp
                .send_raw_request("bgr/message/deliver", Some(invalid))
                .await?;
            let error = timeout(
                TIMEOUT,
                mcp.read_stream_until_error_message(RequestId::Integer(id)),
            )
            .await??;
            assert!(
                error.error.message.contains("BGR")
                    || error.error.message.contains("unknown field")
            );
        }
        for (field, value) in [
            (
                "payload",
                json!({"scope": "since_time", "since_time": "2026-11-03T00:00:00Z"}),
            ),
            ("kind", json!("task")),
            ("identity", json!(stale_identity)),
        ] {
            let mut invalid = send.clone();
            invalid["envelope"][field] = value;
            let id = mcp
                .send_raw_request("bgr/message/deliver", Some(invalid))
                .await?;
            let error = timeout(
                TIMEOUT,
                mcp.read_stream_until_error_message(RequestId::Integer(id)),
            )
            .await??;
            assert!(error.error.message.contains("BGR"));
        }
        ("bgr/message/deliver", send)
    };
    let receipt: BgrMessageSendResponse = rpc(&mut mcp, method, send.clone()).await?;
    assert!(receipt.accepted);
    assert!(!receipt.duplicate);
    let duplicate: BgrMessageSendResponse = rpc(&mut mcp, method, send).await?;
    assert!(duplicate.duplicate);
    let snapshot: BgrThreadStateResponse = rpc(
        &mut mcp,
        "bgr/thread/inspect",
        json!({"threadId": worker.thread.id}),
    )
    .await?;
    assert_eq!(snapshot.snapshot.pending, vec![envelope.clone()]);
    assert!(!snapshot.snapshot.cancelled);

    if cancel_while_busy {
        let started: TurnStartedNotification =
            timeout(TIMEOUT, mcp.read_notification("turn/started")).await??;
        assert_eq!(started.turn.id, turn.turn.id);
        let cancelled: BgrThreadStateResponse = rpc(
            &mut mcp,
            "bgr/thread/cancel",
            json!({"threadId": worker.thread.id, "identity": worker_identity}),
        )
        .await?;
        assert!(cancelled.snapshot.cancelled);
        let interrupted: TurnCompletedNotification =
            timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
        assert_eq!(interrupted.turn.id, turn.turn.id);
        assert_eq!(interrupted.turn.status, TurnStatus::Interrupted);
        let unexpected = timeout(
            Duration::from_secs(1),
            mcp.read_notification::<TurnStartedNotification>("turn/started"),
        )
        .await;
        if let Ok(Ok(ref started)) = unexpected {
            let completed: TurnCompletedNotification =
                timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
            eprintln!(
                "BGR_CANCEL_TRACE: unexpected turn {} completed {:?}",
                started.turn.id, completed.turn.status
            );
        }
        let reply = timeout(
            Duration::from_millis(100),
            mcp.read_notification::<BgrMessageNotification>("bgr/message"),
        )
        .await;
        let bodies = server.received_requests().await.unwrap();
        eprintln!(
            "BGR_CANCEL_TRACE: {} requests; reply={reply:?}",
            bodies.len()
        );
        for (index, request) in bodies.iter().enumerate() {
            let body: Value = serde_json::from_slice(&request.body)?;
            eprintln!(
                "BGR_CANCEL_TRACE: request {} contains queued query={}",
                index + 1,
                body["input"].to_string().contains(&envelope.message_id)
            );
        }
        assert!(
            unexpected.is_err(),
            "cancelled BGR query started another native turn: {unexpected:?}"
        );
        assert!(reply.is_err(), "cancelled query emitted a BGR reply");
        assert_eq!(
            bodies.len(),
            1,
            "cancelled BGR query caused another model request"
        );
        return Ok(());
    }

    mcp.send_response(
        request_id,
        serde_json::to_value(DynamicToolCallResponse {
            success: true,
            content_items: vec![DynamicToolCallOutputContentItem::InputText {
                text: "held work complete".into(),
            }],
        })?,
    )
    .await?;
    let reply: BgrMessageNotification =
        timeout(TIMEOUT, mcp.read_notification("bgr/message")).await??;
    assert_eq!(reply.envelope.correlation_id, correlation_id);
    assert_eq!(
        reply.envelope.in_reply_to.as_deref(),
        Some(query_id.as_str())
    );
    assert_eq!(reply.envelope.head_sha.as_deref(), query_head_sha);
    assert_eq!(reply.envelope.payload["headSha"], observed_head_sha);
    assert_eq!(reply.envelope.identity, worker_identity);
    assert_eq!(reply.envelope.sender_agent_id, worker_identity.agent_id);
    assert_eq!(reply.envelope.recipient_agent_id, brain_sender.agent_id);
    assert_eq!(reply.thread_id, worker.thread.id);
    let completed: TurnCompletedNotification =
        timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
    assert_eq!(completed.turn.id, turn.turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    let snapshot: BgrThreadStateResponse = rpc(
        &mut mcp,
        "bgr/thread/checkpoint",
        json!({"threadId": worker.thread.id, "identity": worker_identity}),
    )
    .await?;
    assert!(snapshot.snapshot.pending.is_empty());
    assert_eq!(snapshot.snapshot.replies, vec![reply.envelope]);
    let reattached: BgrThreadStateResponse = rpc(
        &mut mcp,
        "bgr/thread/bind",
        json!({"threadId": worker.thread.id, "identity": worker_identity}),
    )
    .await?;
    assert_eq!(reattached, snapshot);
    let cancelled: BgrThreadStateResponse = rpc(
        &mut mcp,
        "bgr/thread/cancel",
        json!({"threadId": worker.thread.id, "identity": worker_identity}),
    )
    .await?;
    assert!(cancelled.snapshot.cancelled);
    let bodies = server.received_requests().await.unwrap();
    assert_eq!(bodies.len(), 3);
    let after_boundary: Value = bodies[1].body_json()?;
    assert!(after_boundary["input"].to_string().contains(&query_id));
    assert!(
        after_boundary["input"]
            .to_string()
            .contains("held work complete")
    );
    let continued: Value = bodies[2].body_json()?;
    assert!(
        continued["input"]
            .to_string()
            .contains("Continue the same assignment")
    );
    Ok(())
}

#[tokio::test]
async fn bgr_named_tool_remains_host_owned_on_ordinary_threads() -> Result<()> {
    let server = create_mock_responses_server_sequence(vec![
        responses::sse(vec![
            responses::ev_response_created("ordinary-one"),
            responses::ev_function_call("ordinary-call", BGR_REPORT_TOOL, "{}"),
            responses::ev_completed("ordinary-one"),
        ]),
        create_final_assistant_message_sse_response("Ordinary callback completed.")?,
    ])
    .await;
    let home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(TIMEOUT, mcp.initialize()).await??;
    let thread: ThreadStartResponse = rpc(
        &mut mcp,
        "thread/start",
        json!({"dynamicTools": [{
            "type": "function", "name": BGR_REPORT_TOOL, "description": "Host-owned callback",
            "inputSchema": {"type": "object", "properties": {}}
        }]}),
    )
    .await?;
    let turn: TurnStartResponse = rpc(
        &mut mcp,
        "turn/start",
        json!({"threadId": thread.thread.id,
        "input": [{"type": "text", "text": "Call the host callback."}]}),
    )
    .await?;
    let request = timeout(TIMEOUT, mcp.read_stream_until_request_message()).await??;
    let ServerRequest::DynamicToolCall { request_id, params } = request else {
        panic!("ordinary tool call must still reach the host")
    };
    assert_eq!(params.tool, BGR_REPORT_TOOL);
    assert_eq!(params.turn_id, turn.turn.id);
    mcp.send_response(
        request_id,
        json!({"success": true, "contentItems": [{
            "type": "inputText", "text": "host-owned result"
        }]}),
    )
    .await?;
    let completed: TurnCompletedNotification =
        timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
    assert_eq!(completed.turn.id, turn.turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    Ok(())
}

#[tokio::test]
async fn bgr_capabilities_require_experimental_api() -> Result<()> {
    let home = TempDir::new()?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_auto_env()
        .build()
        .await?;
    mcp.initialize_with_capabilities(
        ClientInfo {
            name: "bgr-contract-test".into(),
            title: None,
            version: "1".into(),
        },
        Some(InitializeCapabilities {
            experimental_api: false,
            ..Default::default()
        }),
    )
    .await?;
    let target = identity("worker-session", BgrRole::Grunt);
    let sender = BgrSender::from(&identity("claude-brain-session", BgrRole::Brain));
    for (method, params) in [
        ("bgr/capabilities/read", json!({})),
        (
            "bgr/message/deliver",
            json!({"threadId": "unloaded-native-thread", "sender": sender,
            "envelope": {"version": 1, "messageId": "query", "senderAgentId": sender.agent_id,
                "recipientAgentId": target.agent_id, "identity": target, "kind": "status_query",
                "correlationId": "correlation", "payload": {"scope": "now"}}}),
        ),
    ] {
        let id = mcp.send_raw_request(method, Some(params)).await?;
        let error = timeout(
            TIMEOUT,
            mcp.read_stream_until_error_message(RequestId::Integer(id)),
        )
        .await??;
        assert!(error.error.message.contains("experimental"));
    }
    Ok(())
}
