use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
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
        team_id: "team".into(),
        agent_id: if role == BgrRole::Brain { "a" } else { "b" }.repeat(32),
        session_id: thread.into(),
        task_id: "task".into(),
        attempt_id: "attempt".into(),
        epoch: 1,
        role,
    }
}

#[tokio::test]
async fn bgr_status_receipt_does_not_interrupt_tool_and_reply_continues_same_turn() -> Result<()> {
    let status = json!({
        "messageId": "reply-one", "inReplyTo": "query-one",
        "payload": { "currentAction": "continuing task", "completed": ["held tool"],
            "pending": ["assignment remains active"], "blockers": [], "headSha": null,
            "dirty": null, "coverageLimits": ["self-reported current state"] }
    });
    let server = create_mock_responses_server_sequence(vec![
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

    let brain: ThreadStartResponse = rpc(&mut mcp, "thread/start", json!({})).await?;
    let worker: ThreadStartResponse = rpc(&mut mcp, "thread/start", json!({"dynamicTools": [
        capabilities.report_tool,
        {"type": "function", "name": "hold_work", "description": "Work which must not be interrupted", "inputSchema": {"type": "object", "properties": {}}}
    ]})).await?;
    let brain_identity = identity(&"1".repeat(32), BgrRole::Brain);
    let worker_identity = identity(&"2".repeat(32), BgrRole::Grunt);
    assert_ne!(worker_identity.session_id, worker.thread.id);
    for (id, binding) in [
        (&brain.thread.id, &brain_identity),
        (&worker.thread.id, &worker_identity),
    ] {
        let _: BgrThreadStateResponse = rpc(
            &mut mcp,
            "bgr/thread/bind",
            json!({"threadId": id, "identity": binding}),
        )
        .await?;
    }
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
        message_id: "query-one".into(),
        sender_agent_id: brain_identity.agent_id.clone(),
        recipient_agent_id: worker_identity.agent_id.clone(),
        identity: worker_identity.clone(),
        kind: BgrMessageKind::StatusQuery,
        correlation_id: "corr-one".into(),
        in_reply_to: None,
        payload: json!({"scope": "now"}),
        head_sha: None,
    };
    let send = json!({"sourceThreadId": brain.thread.id, "threadId": worker.thread.id, "envelope": envelope});
    let receipt: BgrMessageSendResponse = rpc(&mut mcp, "bgr/message/send", send.clone()).await?;
    assert!(receipt.accepted);
    assert!(!receipt.duplicate);
    let duplicate: BgrMessageSendResponse = rpc(&mut mcp, "bgr/message/send", send).await?;
    assert!(duplicate.duplicate);
    let snapshot: BgrThreadStateResponse = rpc(
        &mut mcp,
        "bgr/thread/inspect",
        json!({"threadId": worker.thread.id}),
    )
    .await?;
    assert_eq!(snapshot.snapshot.pending, vec![envelope]);
    assert!(!snapshot.snapshot.cancelled);

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
    assert_eq!(reply.envelope.correlation_id, "corr-one");
    assert_eq!(reply.envelope.in_reply_to.as_deref(), Some("query-one"));
    assert_eq!(reply.envelope.identity, worker_identity);
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
    assert!(after_boundary["input"].to_string().contains("query-one"));
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
    let id = mcp
        .send_raw_request("bgr/capabilities/read", Some(json!({})))
        .await?;
    let error = timeout(
        TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    assert!(error.error.message.contains("experimental"));
    Ok(())
}
