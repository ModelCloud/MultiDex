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

#[tokio::test]
async fn bgr_host_records_source_before_routing_between_separate_app_servers() -> Result<()> {
    let brain_server=create_mock_responses_server_sequence_unchecked(vec![
        responses::sse(vec![responses::ev_response_created("answer"),responses::ev_function_call("answer","bgr_reply_message",&json!({"messageId":"answer","inReplyTo":"question","kind":"answer","payload":{"text":"Confirmed scope."}}).to_string()),responses::ev_completed("answer")]),
        create_final_assistant_message_sse_response("Answer sent.")?,
        create_final_assistant_message_sse_response("Worker result received.")?,
    ]).await;
    let worker_server=create_mock_responses_server_sequence_unchecked(vec![
        responses::sse(vec![responses::ev_response_created("question"),responses::ev_function_call("question","bgr_reply_message",&json!({"messageId":"question","inReplyTo":"task","kind":"question","payload":{"text":"Confirm scope?"}}).to_string()),responses::ev_completed("question")]),
        create_final_assistant_message_sse_response("Awaiting clarification.")?,
        responses::sse(vec![responses::ev_response_created("result"),responses::ev_function_call("result","bgr_reply_message",&json!({"messageId":"result","inReplyTo":"task","kind":"result","payload":{"text":"Completed."}}).to_string()),responses::ev_completed("result")]),
        create_final_assistant_message_sse_response("Work complete.")?,
    ]).await;
    let brain_home = TempDir::new()?;
    let worker_home = TempDir::new()?;
    for (home, server) in [(&brain_home, &brain_server), (&worker_home, &worker_server)] {
        MockResponsesConfig::new(&server.uri())
            .disable_feature(Feature::Collab)
            .disable_feature(Feature::MultiAgentV2)
            .enable_feature(Feature::DeferMailboxPreemption)
            .write(home.path())?;
    }
    let mut brain = TestAppServer::builder()
        .with_codex_home(brain_home.path())
        .without_auto_env()
        .build()
        .await?;
    let mut worker = TestAppServer::builder()
        .with_codex_home(worker_home.path())
        .without_auto_env()
        .build()
        .await?;
    timeout(TIMEOUT, brain.initialize()).await??;
    timeout(TIMEOUT, worker.initialize()).await??;
    let capabilities: BgrCapabilitiesReadResponse =
        rpc(&mut brain, "bgr/capabilities/read", json!({})).await?;
    assert!(capabilities.host_record);
    assert!(capabilities.reviewer_snapshot_no_lazy_fetch);
    let mut brain_id = identity("brain", BgrRole::Brain);
    brain_id.task_id = None;
    brain_id.attempt_id = None;
    let worker_id = identity("worker", BgrRole::Grunt);
    let mut threads = Vec::new();
    for (mcp, bound, peer) in [
        (&mut brain, &brain_id, &worker_id),
        (&mut worker, &worker_id, &brain_id),
    ] {
        let started:ThreadStartResponse=rpc(mcp,"thread/start",json!({"dynamicTools":[capabilities.report_tool,capabilities.send_tool,capabilities.reply_tool]})).await?;
        rpc::<BgrThreadStateResponse>(
            mcp,
            "bgr/thread/bind",
            json!({"threadId":started.thread.id,"identity":bound}),
        )
        .await?;
        rpc::<BgrThreadStateResponse>(
            mcp,
            "bgr/thread/peers",
            json!({"threadId":started.thread.id,"identity":bound,"peers":[peer]}),
        )
        .await?;
        threads.push(started.thread.id);
    }
    let task = BgrEnvelope {
        version: BGR_VERSION,
        message_id: "task".into(),
        sender_agent_id: brain_id.agent_id.clone(),
        recipient_agent_id: worker_id.agent_id.clone(),
        identity: brain_id.clone(),
        recipient_identity: Some(worker_id.clone()),
        kind: BgrMessageKind::Task,
        correlation_id: "correlation".into(),
        in_reply_to: None,
        payload: json!({"text":"Perform work and clarify scope."}),
        head_sha: Some("a".repeat(40)),
    };
    let record = json!({"threadId":threads[0],"identity":brain_id,"envelope":task});
    for mut invalid in [record.clone(), record.clone(), record.clone()]
        .into_iter()
        .enumerate()
    {
        match invalid.0 {
            0 => invalid.1["identity"]["epoch"] = json!(2),
            1 => {
                invalid.1["envelope"]["recipientIdentity"]["attemptId"] = json!("stale");
            }
            _ => {
                invalid.1["envelope"]["identity"]["agentId"] = json!("spoofed");
            }
        }
        let id = brain
            .send_raw_request("bgr/message/record", Some(invalid.1))
            .await?;
        timeout(
            TIMEOUT,
            brain.read_stream_until_error_message(RequestId::Integer(id)),
        )
        .await??;
    }
    let first: BgrMessageSendResponse =
        rpc(&mut brain, "bgr/message/record", record.clone()).await?;
    assert!(first.accepted && !first.duplicate);
    let duplicate: BgrMessageSendResponse =
        rpc(&mut brain, "bgr/message/record", record.clone()).await?;
    assert!(duplicate.accepted && duplicate.duplicate);
    let mut conflict = record.clone();
    conflict["envelope"]["payload"] = json!({"text":"changed"});
    let id = brain
        .send_raw_request("bgr/message/record", Some(conflict))
        .await?;
    timeout(
        TIMEOUT,
        brain.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    let source: BgrThreadStateResponse = rpc(
        &mut brain,
        "bgr/thread/inspect",
        json!({"threadId":threads[0]}),
    )
    .await?;
    assert_eq!(source.snapshot.replies, vec![task.clone()]);
    assert!(source.snapshot.pending.is_empty());
    assert!(brain_server.received_requests().await.unwrap().is_empty());
    assert!(worker_server.received_requests().await.unwrap().is_empty());
    rpc::<BgrMessageSendResponse>(
        &mut worker,
        "bgr/message/deliver",
        json!({"threadId":threads[1],"sender":BgrSender::from(&brain_id),"envelope":task}),
    )
    .await?;
    let mut callbacks = Vec::new();
    for (index, kind) in [
        BgrMessageKind::Question,
        BgrMessageKind::Answer,
        BgrMessageKind::Result,
    ]
    .into_iter()
    .enumerate()
    {
        let (source, target, target_thread, sender) = if index == 1 {
            (&mut brain, &mut worker, &threads[1], &brain_id)
        } else {
            (&mut worker, &mut brain, &threads[0], &worker_id)
        };
        let callback: BgrMessageNotification =
            timeout(TIMEOUT, source.read_notification("bgr/message")).await??;
        assert_eq!(callback.envelope.kind, kind);
        assert_eq!(callback.envelope.correlation_id, "correlation");
        assert_eq!(callback.envelope.head_sha, Some("a".repeat(40)));
        let completed: TurnCompletedNotification =
            timeout(TIMEOUT, source.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        rpc::<BgrMessageSendResponse>(target,"bgr/message/deliver",json!({"threadId":target_thread,"sender":BgrSender::from(sender),"envelope":callback.envelope})).await?;
        callbacks.push(callback);
    }
    let completed: TurnCompletedNotification =
        timeout(TIMEOUT, brain.read_notification("turn/completed")).await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    let mut progress = task.clone();
    progress.message_id = "late-progress".into();
    progress.kind = BgrMessageKind::Progress;
    progress.in_reply_to = Some("task".into());
    progress.identity = worker_id.clone();
    progress.sender_agent_id = worker_id.agent_id.clone();
    progress.recipient_identity = Some(brain_id.clone());
    progress.recipient_agent_id = brain_id.agent_id.clone();
    let mut stale = worker_id.clone();
    stale.attempt_id = Some("stale-attempt".into());
    let mut stale_progress = progress.clone();
    stale_progress.identity = stale.clone();
    let id = worker
        .send_raw_request(
            "bgr/message/record",
            Some(json!({"threadId":threads[1],"identity":stale,"envelope":stale_progress})),
        )
        .await?;
    timeout(
        TIMEOUT,
        worker.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    rpc::<BgrThreadStateResponse>(
        &mut worker,
        "bgr/thread/cancel",
        json!({"threadId":threads[1],"identity":worker_id}),
    )
    .await?;
    let before: BgrThreadStateResponse = rpc(
        &mut worker,
        "bgr/thread/inspect",
        json!({"threadId":threads[1]}),
    )
    .await?;
    let id = worker
        .send_raw_request(
            "bgr/message/record",
            Some(json!({"threadId":threads[1],"identity":worker_id,"envelope":progress})),
        )
        .await?;
    let error = timeout(
        TIMEOUT,
        worker.read_stream_until_error_message(RequestId::Integer(id)),
    )
    .await??;
    assert!(error.error.message.contains("cancelled"));
    let after: BgrThreadStateResponse = rpc(
        &mut worker,
        "bgr/thread/inspect",
        json!({"threadId":threads[1]}),
    )
    .await?;
    assert_eq!(before, after);
    assert_eq!(brain_server.received_requests().await.unwrap().len(), 3);
    assert_eq!(worker_server.received_requests().await.unwrap().len(), 4);
    if let Some(path) = std::env::var_os("BGR_RECORD_TRANSCRIPT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &json!({"capabilities":capabilities,"record":{"params":record,"result":first,"duplicate":duplicate},"callbacks":callbacks,"providerRequestCounts":{"brain":3,"worker":4}}),
            )?,
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn bgr_two_isolated_reviewers_and_grunt_clarify_and_return_out_of_order_results() -> Result<()>
{
    let repository = TempDir::new()?;
    let private = TempDir::new()?;
    let secret = private.path().join("private-token");
    std::fs::write(&secret, "FORGE_CREDENTIAL_CANARY_DO_NOT_EXPOSE")?;
    let git = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repository.path())
            .output()?;
        anyhow::ensure!(output.status.success(), "fixture git failed: {args:?}");
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    };
    git(&["init"])?;
    std::fs::write(
        repository.path().join("review.rs"),
        "pub const VALUE: u32 = 1;\n",
    )?;
    git(&["add", "review.rs"])?;
    git(&["commit", "-m", "immutable base"])?;
    let base = git(&["rev-parse", "HEAD"])?;
    std::fs::write(
        repository.path().join("review.rs"),
        "pub const VALUE: u32 = 2;\n",
    )?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&secret, repository.path().join("escape"))?;
    git(&["add", "."])?;
    git(&["commit", "-m", "immutable head"])?;
    let head = git(&["rev-parse", "HEAD"])?;
    // Repository diff helpers must never run in the trusted snapshot extractor.
    let external_marker = private.path().join("external-helper-ran");
    let textconv_marker = private.path().join("textconv-helper-ran");
    std::fs::write(
        repository.path().join(".gitattributes"),
        "review.rs diff=credential_probe\n",
    )?;
    git(&[
        "config",
        "diff.credential_probe.textconv",
        &format!("touch {}", textconv_marker.display()),
    ])?;
    git(&[
        "config",
        "diff.external",
        &format!("touch {}", external_marker.display()),
    ])?;
    git(&[
        "config",
        "core.pager",
        &format!("touch {}", external_marker.display()),
    ])?;
    let mut servers = Vec::new();
    for index in 0..3 {
        let question = json!({"messageId":format!("{:032x}",0x300+index),"inReplyTo":format!("{:032x}",0x100+index),
            "kind":"question","payload":{"text":"Is VALUE=2 the intended contract?"}});
        let result = json!({"messageId":format!("{:032x}",0x400+index),"inReplyTo":format!("{:032x}",0x100+index),
            "kind":"result","payload":{"outcome":if index==1 {"incomplete"} else {"no_findings"},
                "findings":[],"coverageLimits":["No tests executed; immutable source inspection only."]}});
        let mut sequence = Vec::new();
        if index < 2 {
            let mut calls = vec![responses::ev_response_created("snapshot")];
            for (read, path) in [
                "head/review.rs",
                "/etc/passwd",
                "../private-token",
                "base/../../private-token",
                "head/escape",
                "runtime/auth.json",
                "head/sibling-findings.json",
            ]
            .into_iter()
            .enumerate()
            {
                calls.push(responses::ev_function_call(
                    &format!("read-{read}"),
                    BGR_REVIEW_SNAPSHOT_TOOL,
                    &json!({"path":path}).to_string(),
                ));
            }
            calls.push(responses::ev_function_call(
                "forbidden-shell",
                "exec_command",
                &json!({"cmd":format!("cat {}", secret.display())}).to_string(),
            ));
            calls.push(responses::ev_function_call(
                "forbidden-spawn",
                "spawn_agent",
                "{\"message\":\"Bypass controller\"}",
            ));
            calls.push(responses::ev_function_call(
                "forbidden-mcp",
                "read_mcp_resource",
                "{\"server\":\"forge\",\"uri\":\"file:///auth.json\"}",
            ));
            calls.push(responses::ev_function_call("forbidden-write", "apply_patch", "{\"patch\":\"*** Begin Patch\\n*** Add File: forbidden-write\\n+bad\\n*** End Patch\"}"));
            calls.push(responses::ev_completed("snapshot"));
            sequence.push(responses::sse(calls));
        }
        sequence.extend([
            responses::sse(vec![
                responses::ev_response_created("question"),
                responses::ev_function_call("question", BGR_REPLY_TOOL, &question.to_string()),
                responses::ev_completed("question"),
            ]),
            create_final_assistant_message_sse_response(
                "Waiting for factual clarification from the brain.",
            )?,
            responses::sse(vec![
                responses::ev_response_created("result"),
                responses::ev_function_call("result", BGR_REPLY_TOOL, &result.to_string()),
                responses::ev_completed("result"),
            ]),
            create_final_assistant_message_sse_response(
                "Independent review result sealed for host publication.",
            )?,
        ]);
        servers.push(create_mock_responses_server_sequence_unchecked(sequence).await);
    }
    let home = TempDir::new()?;
    MockResponsesConfig::new(&servers[0].uri())
        .disable_feature(Feature::Collab)
        .disable_feature(Feature::MultiAgentV2)
        .enable_feature(Feature::DeferMailboxPreemption)
        .with_root_config("developer_instructions = \"HOST_PRIVATE_INSTRUCTION_CANARY\"")
        .with_extra_config(&format!(
            r#"
[model_providers.reviewer_b]
name = "Independent mock reviewer B"
base_url = "{}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
[model_providers.grunt]
name = "Independent mock grunt"
base_url = "{}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#,
            servers[1].uri(),
            servers[2].uri()
        ))
        .write(home.path())?;
    std::fs::write(home.path().join("AGENTS.md"), "PRIVATE_CONVERSATION_CANARY")?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_auto_env()
        .build()
        .await?;
    mcp.initialize().await?;
    let capabilities: BgrCapabilitiesReadResponse =
        rpc(&mut mcp, "bgr/capabilities/read", json!({})).await?;
    let brain = identity(&"a".repeat(32), BgrRole::Brain);
    let mut participants = Vec::new();
    let review = json!({"repository":repository.path(),"baseSha":base,"headSha":head});
    let mut trace = json!({"capabilities":capabilities,"review":review,"starts":[],"rejections":[],"questions":[],"answers":[],"results":[],"providerRequests":[]});
    for index in 0..3 {
        let role = if index < 2 {
            BgrRole::Reviewer
        } else {
            BgrRole::Grunt
        };
        let mut bound = identity(&format!("{:032x}", index + 0x10), role);
        bound.agent_id = format!("{:032x}", index + 0x20);
        bound.attempt_id = Some(format!("{:032x}", index + 0x30));
        let thread_id = if index < 2 {
            let params = json!({"identity":bound,"peers":[brain],"review":review,"modelProvider":if index==0 {"mock_provider"} else {"reviewer_b"}});
            if index == 0 {
                for key in ["dynamicTools", "config", "instructions", "cwd", "env"] {
                    let mut unsafe_params = params.clone();
                    unsafe_params[key] = json!({});
                    let id = mcp
                        .send_raw_request("bgr/reviewer/start", Some(unsafe_params))
                        .await?;
                    let error = timeout(
                        TIMEOUT,
                        mcp.read_stream_until_error_message(RequestId::Integer(id)),
                    )
                    .await??;
                    trace["rejections"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"field":key,"error":error.error}));
                }
                for (field, value) in [
                    ("identity", json!({"role":"grunt"})),
                    ("review", json!({"headSha":"HEAD"})),
                    ("review", json!({"headSha":"0".repeat(40)})),
                    ("modelProvider", json!("unknown-review-provider")),
                ] {
                    let mut unsafe_params = params.clone();
                    if value.is_object() {
                        for (key, value) in value.as_object().unwrap() {
                            unsafe_params[field][key] = value.clone();
                        }
                    } else {
                        unsafe_params[field] = value;
                    }
                    let id = mcp
                        .send_raw_request("bgr/reviewer/start", Some(unsafe_params))
                        .await?;
                    let error = timeout(
                        TIMEOUT,
                        mcp.read_stream_until_error_message(RequestId::Integer(id)),
                    )
                    .await??;
                    trace["rejections"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"field":field,"error":error.error}));
                }
                for server in &servers {
                    assert_eq!(server.received_requests().await.unwrap().len(), 0);
                }
            }
            let started: BgrReviewerStartResponse =
                rpc(&mut mcp, "bgr/reviewer/start", params.clone()).await?;
            let shell_id = mcp
                .send_raw_request(
                    "thread/shellCommand",
                    Some(json!({"threadId":started.thread_id,"command":"echo forbidden"})),
                )
                .await?;
            let error = timeout(
                TIMEOUT,
                mcp.read_stream_until_error_message(RequestId::Integer(shell_id)),
            )
            .await??;
            assert!(error.error.message.contains("forbidden"));
            for (method, params) in [
                (
                    "thread/settings/update",
                    json!({"threadId":started.thread_id,"sandboxPolicy":{"type":"dangerFullAccess"}}),
                ),
                (
                    "turn/start",
                    json!({"threadId":started.thread_id,"cwd":home.path(),"input":[{"type":"text","text":"unsafe runtime override"}]}),
                ),
            ] {
                let id = mcp.send_raw_request(method, Some(params)).await?;
                let error = timeout(
                    TIMEOUT,
                    mcp.read_stream_until_error_message(RequestId::Integer(id)),
                )
                .await??;
                trace["rejections"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"method":method,"error":error.error}));
            }
            assert_eq!(started.snapshot.identity, bound);
            assert_eq!(started.review.base_sha, base);
            assert_eq!(started.review.head_sha, head);
            assert!(
                started
                    .review
                    .changed_files
                    .contains(&"review.rs".to_string())
            );
            trace["starts"]
                .as_array_mut()
                .unwrap()
                .push(json!({"params":params,"result":started}));
            started.thread_id
        } else {
            let started: ThreadStartResponse = rpc(&mut mcp,"thread/start",json!({"modelProvider":"grunt","dynamicTools":[capabilities.report_tool,capabilities.send_tool,capabilities.reply_tool]})).await?;
            rpc::<BgrThreadStateResponse>(
                &mut mcp,
                "bgr/thread/bind",
                json!({"threadId":started.thread.id,"identity":bound}),
            )
            .await?;
            rpc::<BgrThreadStateResponse>(
                &mut mcp,
                "bgr/thread/peers",
                json!({"threadId":started.thread.id,"identity":bound,"peers":[brain]}),
            )
            .await?;
            started.thread.id
        };
        participants.push((thread_id, bound));
    }
    assert_ne!(participants[0].0, participants[1].0);
    assert_ne!(participants[0].1, participants[1].1);
    std::fs::write(
        repository.path().join("review.rs"),
        "WORKTREE_MUTATION_CANARY",
    )?;
    for (index, (thread_id, bound)) in participants.iter().enumerate() {
        let task = BgrEnvelope {
            version: BGR_VERSION,
            message_id: format!("{:032x}", index + 0x100),
            sender_agent_id: brain.agent_id.clone(),
            recipient_agent_id: bound.agent_id.clone(),
            identity: brain.clone(),
            recipient_identity: Some(bound.clone()),
            kind: BgrMessageKind::Task,
            correlation_id: format!("{:032x}", index + 0x200),
            in_reply_to: None,
            payload: json!({"text":"Read the frozen snapshot, ask the brain a factual question, then wait for its answer before reporting."}),
            head_sha: Some(head.clone()),
        };
        let delivery =
            json!({"threadId":thread_id,"sender":BgrSender::from(&brain),"envelope":task});
        if index < 2 {
            let mut wrong_head = delivery.clone();
            wrong_head["envelope"]["headSha"] = json!(base);
            let id = mcp
                .send_raw_request("bgr/message/deliver", Some(wrong_head))
                .await?;
            let error = timeout(
                TIMEOUT,
                mcp.read_stream_until_error_message(RequestId::Integer(id)),
            )
            .await??;
            assert!(error.error.message.contains("frozen review head"));
            assert_eq!(servers[index].received_requests().await.unwrap().len(), 0);
        }
        let receipt: BgrMessageSendResponse =
            rpc(&mut mcp, "bgr/message/deliver", delivery.clone()).await?;
        assert!(receipt.accepted && !receipt.duplicate);
        assert!(
            rpc::<BgrMessageSendResponse>(&mut mcp, "bgr/message/deliver", delivery)
                .await?
                .duplicate
        );
        let question: BgrMessageNotification =
            timeout(TIMEOUT, mcp.read_notification("bgr/message")).await??;
        assert_eq!(question.envelope.identity, *bound);
        assert_eq!(question.envelope.kind, BgrMessageKind::Question);
        assert_eq!(question.envelope.head_sha, Some(head.clone()));
        trace["questions"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(question)?);
        let completed: TurnCompletedNotification =
            timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        if index < 2 {
            rpc::<Value>(
                &mut mcp,
                "thread/settings/update",
                json!({"threadId":thread_id,"model":"mock-model"}),
            )
            .await?;
        }
    }
    for index in [1, 2, 0] {
        let (thread_id, bound) = &participants[index];
        let answer = BgrEnvelope {
            version: BGR_VERSION,
            message_id: format!("{:032x}", index + 0x500),
            sender_agent_id: brain.agent_id.clone(),
            recipient_agent_id: bound.agent_id.clone(),
            identity: brain.clone(),
            recipient_identity: Some(bound.clone()),
            kind: BgrMessageKind::Answer,
            correlation_id: format!("{:032x}", index + 0x200),
            in_reply_to: Some(format!("{:032x}", index + 0x300)),
            payload: json!({"text":"Confirmed: VALUE=2 is the intended public contract."}),
            head_sha: Some(head.clone()),
        };
        let params =
            json!({"threadId":thread_id,"sender":BgrSender::from(&brain),"envelope":answer});
        let receipt: BgrMessageSendResponse =
            rpc(&mut mcp, "bgr/message/deliver", params.clone()).await?;
        assert!(receipt.accepted && !receipt.duplicate);
        trace["answers"].as_array_mut().unwrap().push(params);
        let result: BgrMessageNotification =
            timeout(TIMEOUT, mcp.read_notification("bgr/message")).await??;
        assert_eq!(result.envelope.identity, *bound);
        assert_eq!(result.envelope.recipient_identity, Some(brain.clone()));
        assert_eq!(result.envelope.kind, BgrMessageKind::Result);
        assert_eq!(
            result.envelope.correlation_id,
            format!("{:032x}", index + 0x200)
        );
        assert_eq!(result.envelope.head_sha, Some(head.clone()));
        assert_eq!(
            result.envelope.payload["outcome"],
            if index == 1 {
                "incomplete"
            } else {
                "no_findings"
            }
        );
        trace["results"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(result)?);
        let completed: TurnCompletedNotification =
            timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
    }
    for (index, server) in servers.iter().enumerate() {
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), if index < 2 { 5 } else { 4 });
        let bodies = requests
            .iter()
            .map(wiremock::Request::body_json::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if index < 2 {
            for body in &bodies {
                let exposed = body["tools"].as_array().unwrap();
                assert_eq!(exposed.len(), 4, "reviewer leaked tools: {exposed:?}");
                for tool in exposed {
                    assert!(
                        [
                            BGR_REPORT_TOOL,
                            BGR_SEND_TOOL,
                            BGR_REPLY_TOOL,
                            BGR_REVIEW_SNAPSHOT_TOOL
                        ]
                        .contains(&tool["name"].as_str().unwrap()),
                        "unexpected reviewer tool {tool}"
                    );
                }
                let input = body.to_string();
                for forbidden in [
                    "FORGE_CREDENTIAL_CANARY_DO_NOT_EXPOSE",
                    "HOST_PRIVATE_INSTRUCTION_CANARY",
                    "PRIVATE_CONVERSATION_CANARY",
                    "WORKTREE_MUTATION_CANARY",
                ] {
                    assert!(!input.contains(forbidden), "reviewer exposed {forbidden}");
                }
            }
            let observed = bodies[1]["input"].to_string();
            assert!(observed.contains("pub const VALUE: u32 = 2;"));
            assert!(observed.contains("path outside frozen review snapshot"));
        }
        assert!(
            bodies[if index < 2 { 3 } else { 2 }]["input"]
                .to_string()
                .contains("VALUE=2 is the intended public contract")
        );
        trace["providerRequests"]
            .as_array_mut()
            .unwrap()
            .push(json!({"identity":participants[index].1,"count":requests.len(),"bodies":bodies}));
    }
    assert!(
        !external_marker.exists() && !textconv_marker.exists(),
        "git snapshot invoked external helper"
    );
    if let Ok(path) = std::env::var("BGR_REVIEWER_TRANSCRIPT") {
        std::fs::write(path, serde_json::to_vec_pretty(&trace)?)?;
    }
    eprintln!(
        "BGR_REVIEWER_TRACE: 2 isolated reviewers + grunt, provider requests [5,5,4], results B/grunt/A"
    );
    Ok(())
}

#[tokio::test]
async fn bgr_general_external_brain_clarification_callbacks_and_cancel_are_fenced() -> Result<()> {
    for cancel in [false, true] {
        let question = json!({"messageId": "worker-question", "inReplyTo": "assignment", "kind": "question", "payload": {"text": "Which behavior is intended?"}});
        let result = json!({"messageId": "worker-result", "inReplyTo": "assignment", "kind": "result", "payload": {"text": "Implemented the clarified behavior.", "outcome": "complete"}});
        let mut sequence = vec![
            responses::sse(vec![
                responses::ev_response_created("question"),
                responses::ev_function_call("question", BGR_REPLY_TOOL, &question.to_string()),
                responses::ev_completed("question"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("hold"),
                responses::ev_function_call("hold", "hold_work", "{}"),
                responses::ev_completed("hold"),
            ]),
        ];
        if !cancel {
            sequence.push(responses::sse(vec![
                responses::ev_response_created("result"),
                responses::ev_function_call("result", BGR_REPLY_TOOL, &result.to_string()),
                responses::ev_completed("result"),
            ]));
            sequence.push(create_final_assistant_message_sse_response(
                "Continuing the same assignment.",
            )?);
        }
        let server = create_mock_responses_server_sequence_unchecked(sequence).await;
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
        mcp.initialize().await?;
        let capabilities: BgrCapabilitiesReadResponse =
            rpc(&mut mcp, "bgr/capabilities/read", json!({})).await?;
        assert!(capabilities.general_messaging && capabilities.message_callbacks);
        let worker: ThreadStartResponse = rpc(&mut mcp, "thread/start", json!({"dynamicTools": [capabilities.report_tool, capabilities.send_tool, capabilities.reply_tool, {"type": "function", "name": "hold_work", "description": "Hold until host release", "inputSchema": {"type": "object"}}]})).await?;
        let brain = identity("external-claude-brain", BgrRole::Brain);
        assert!(brain.task_id.is_none() && brain.attempt_id.is_none());
        let worker_identity = identity("host-worker-session", BgrRole::Grunt);
        rpc::<BgrThreadStateResponse>(
            &mut mcp,
            "bgr/thread/bind",
            json!({"threadId": worker.thread.id, "identity": worker_identity}),
        )
        .await?;
        rpc::<BgrThreadStateResponse>(
            &mut mcp,
            "bgr/thread/peers",
            json!({"threadId": worker.thread.id, "identity": worker_identity, "peers": [brain]}),
        )
        .await?;
        let task = BgrEnvelope {
            version: BGR_VERSION,
            message_id: "assignment".into(),
            sender_agent_id: brain.agent_id.clone(),
            recipient_agent_id: worker_identity.agent_id.clone(),
            identity: brain.clone(),
            recipient_identity: Some(worker_identity.clone()),
            kind: BgrMessageKind::Task,
            correlation_id: "assignment-correlation".into(),
            in_reply_to: None,
            payload: json!({"text": "Perform the assignment, ask a factual question, then hold_work; report result after the answer."}),
            head_sha: Some("a".repeat(40)),
        };
        let delivery = json!({"threadId": worker.thread.id, "sender": BgrSender::from(&brain), "envelope": task});
        let receipt: BgrMessageSendResponse =
            rpc(&mut mcp, "bgr/message/deliver", delivery.clone()).await?;
        assert!(receipt.accepted && !receipt.duplicate);
        let question: BgrMessageNotification =
            timeout(TIMEOUT, mcp.read_notification("bgr/message")).await??;
        assert_eq!(question.envelope.kind, BgrMessageKind::Question);
        assert_eq!(question.envelope.identity, worker_identity);
        assert_eq!(question.envelope.recipient_identity, Some(brain.clone()));
        assert_eq!(question.envelope.in_reply_to.as_deref(), Some("assignment"));
        let ServerRequest::DynamicToolCall { request_id, params } =
            timeout(TIMEOUT, mcp.read_stream_until_request_message()).await??
        else {
            panic!("expected held native tool")
        };
        assert_eq!(params.tool, "hold_work");
        let receipt: BgrMessageSendResponse =
            rpc(&mut mcp, "bgr/message/deliver", delivery).await?;
        assert!(receipt.duplicate);
        let answer = BgrEnvelope {
            message_id: "brain-answer".into(),
            kind: BgrMessageKind::Answer,
            in_reply_to: Some(question.envelope.message_id.clone()),
            payload: json!({"text": "Preserve the existing public behavior."}),
            ..task.clone()
        };
        for field in ["source", "target", "correlation", "anchor", "reply"] {
            let mut invalid = answer.clone();
            match field {
                "source" => invalid.identity.session_id = "spoofed-brain-session".into(),
                "target" => {
                    invalid.recipient_identity.as_mut().unwrap().attempt_id = Some("stale".into())
                }
                "correlation" => invalid.correlation_id = "unrelated".into(),
                "anchor" => invalid.head_sha = Some("b".repeat(40)),
                _ => invalid.in_reply_to = Some("unknown-message".into()),
            }
            let id = mcp.send_raw_request("bgr/message/deliver", Some(json!({"threadId": worker.thread.id, "sender": BgrSender::from(&brain), "envelope": invalid}))).await?;
            timeout(
                TIMEOUT,
                mcp.read_stream_until_error_message(RequestId::Integer(id)),
            )
            .await??;
        }
        rpc::<BgrMessageSendResponse>(&mut mcp, "bgr/message/deliver", json!({"threadId": worker.thread.id, "sender": BgrSender::from(&brain), "envelope": answer})).await?;
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "mail interrupted a long-running tool"
        );
        if cancel {
            let cancellation = BgrEnvelope {
                message_id: "cancel-assignment".into(),
                kind: BgrMessageKind::Cancel,
                payload: json!({"reason": "host cancellation"}),
                ..task
            };
            let params = json!({"threadId": worker.thread.id, "sender": BgrSender::from(&brain), "envelope": cancellation});
            let receipt: BgrMessageSendResponse =
                rpc(&mut mcp, "bgr/message/deliver", params.clone()).await?;
            assert!(!receipt.duplicate);
            let receipt: BgrMessageSendResponse =
                rpc(&mut mcp, "bgr/message/deliver", params).await?;
            assert!(receipt.duplicate);
        } else {
            mcp.send_response(
                request_id,
                serde_json::to_value(DynamicToolCallResponse {
                    success: true,
                    content_items: vec![DynamicToolCallOutputContentItem::InputText {
                        text: "work resumed".into(),
                    }],
                })?,
            )
            .await?;
            let result: BgrMessageNotification =
                timeout(TIMEOUT, mcp.read_notification("bgr/message")).await??;
            assert_eq!(result.envelope.kind, BgrMessageKind::Result);
            assert_eq!(result.envelope.identity, worker_identity);
            assert_eq!(result.envelope.recipient_identity, Some(brain));
            assert_eq!(result.envelope.correlation_id, "assignment-correlation");
            assert_eq!(result.envelope.head_sha, Some("a".repeat(40)));
        }
        let completed: TurnCompletedNotification =
            timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.id, params.turn_id);
        assert_eq!(
            completed.turn.status,
            if cancel {
                TurnStatus::Interrupted
            } else {
                TurnStatus::Completed
            }
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), if cancel { 2 } else { 4 });
        for request in &requests {
            let body: Value = request.body_json()?;
            let tools = body["tools"].to_string();
            for forbidden in [
                "spawn_agent",
                "send_input",
                "wait_agent",
                "resume_agent",
                "close_agent",
                "send_message",
                "followup_task",
                "interrupt_agent",
                "list_agents",
            ] {
                assert!(
                    !tools.contains(&format!("\"name\":\"{forbidden}\"")),
                    "BGR leaked native delegation tool {forbidden}"
                );
            }
        }
        if !cancel {
            assert!(
                requests[2].body_json::<Value>()?["input"]
                    .to_string()
                    .contains("brain-answer")
            );
        }
        eprintln!(
            "BGR_GENERAL_TRACE: {}",
            json!({"cancelled": cancel, "providerRequests": requests.len(), "question": question, "completed": completed})
        );
    }
    Ok(())
}

#[tokio::test]
async fn bgr_two_grunts_reply_out_of_order_and_reject_unfenced_general_messages() -> Result<()> {
    let query_ids = ["10".repeat(16), "20".repeat(16)];
    let correlations = ["30".repeat(16), "40".repeat(16)];
    let anchor = "a".repeat(40);
    let mut simulated = vec![
        responses::sse(vec![
            responses::ev_response_created("hold-one"),
            responses::ev_function_call("hold-one", "hold_work", "{}"),
            responses::ev_completed("hold-one"),
        ]),
        responses::sse(vec![
            responses::ev_response_created("hold-two"),
            responses::ev_function_call("hold-two", "hold_work", "{}"),
            responses::ev_completed("hold-two"),
        ]),
    ];
    for index in (0..2).rev() {
        let report = json!({"messageId": format!("reply-{index}"), "inReplyTo": query_ids[index],
            "payload": {"currentAction": format!("grunt-{index} continuing"),
                "completed": ["held work"], "pending": ["assignment"], "blockers": [],
                "headSha": "b".repeat(40), "dirty": false, "coverageLimits": ["simulated provider"]}});
        simulated.push(responses::sse(vec![
            responses::ev_response_created("report"),
            responses::ev_function_call("report", BGR_REPORT_TOOL, &report.to_string()),
            responses::ev_completed("report"),
        ]));
        simulated.push(create_final_assistant_message_sse_response(
            "Continuing assignment.",
        )?);
    }
    let server = create_mock_responses_server_sequence_unchecked(simulated).await;
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
    assert!(capabilities.reviewer_sandbox);
    assert!(capabilities.reviewer_start);
    assert!(capabilities.reviewer_snapshot_no_lazy_fetch);
    assert!(capabilities.task_dispatch);
    let mut trace = json!({"capabilities": capabilities, "bindings": [], "rejections": [], "queries": [], "notifications": []});
    let brain: ThreadStartResponse = rpc(&mut mcp, "thread/start", json!({})).await?;
    let brain_identity = identity(&"1".repeat(32), BgrRole::Brain);
    rpc::<BgrThreadStateResponse>(
        &mut mcp,
        "bgr/thread/bind",
        json!({"threadId": brain.thread.id, "identity": brain_identity}),
    )
    .await?;
    let mut workers = Vec::new();
    for index in 0..2 {
        let worker: ThreadStartResponse = rpc(&mut mcp, "thread/start", json!({"dynamicTools": [capabilities.report_tool, {
            "type": "function", "name": "hold_work", "description": "Hold work until released by the host", "inputSchema": {"type": "object"}
        }]})).await?;
        let mut worker_identity = identity(&format!("{:032x}", index + 2), BgrRole::Grunt);
        worker_identity.agent_id = if index == 0 { "b" } else { "c" }.repeat(32);
        worker_identity.task_id = Some(format!("{:032x}", index + 4));
        worker_identity.attempt_id = Some(format!("{:032x}", index + 6));
        let bound: BgrThreadStateResponse = rpc(
            &mut mcp,
            "bgr/thread/bind",
            json!({"threadId": worker.thread.id, "identity": worker_identity}),
        )
        .await?;
        trace["bindings"]
            .as_array_mut()
            .unwrap()
            .push(json!({"threadId": worker.thread.id, "result": bound}));
        let turn: TurnStartResponse = rpc(&mut mcp, "turn/start", json!({"threadId": worker.thread.id,
            "input": [{"type": "text", "text": "Perform assigned work; call hold_work, report status when asked, then continue."}]})).await?;
        let ServerRequest::DynamicToolCall { request_id, params } =
            timeout(TIMEOUT, mcp.read_stream_until_request_message()).await??
        else {
            panic!("expected held worker tool")
        };
        assert_eq!(params.tool, "hold_work");
        assert_eq!(params.thread_id, worker.thread.id);
        assert_eq!(params.turn_id, turn.turn.id);
        workers.push((worker, worker_identity, turn, request_id));
    }
    let mut participants = vec![
        (brain.thread.id.clone(), brain_identity.clone()),
        (workers[0].0.thread.id.clone(), workers[0].1.clone()),
    ];
    for index in 0..2 {
        let reviewer: ThreadStartResponse = rpc(
            &mut mcp,
            "thread/start",
            json!({"dynamicTools": [capabilities.report_tool]}),
        )
        .await?;
        let mut reviewer_identity = identity(&format!("{:032x}", index + 8), BgrRole::Reviewer);
        reviewer_identity.agent_id = format!("{:032x}", index + 10);
        let params = json!({"threadId": reviewer.thread.id, "identity": reviewer_identity});
        let id = mcp
            .send_raw_request("bgr/thread/bind", Some(params.clone()))
            .await?;
        let rejected = timeout(
            TIMEOUT,
            mcp.read_stream_until_error_message(RequestId::Integer(id)),
        )
        .await??;
        assert!(rejected.error.message.contains("reviewer"));
        trace["rejections"]
            .as_array_mut()
            .unwrap()
            .push(json!({"method": "bgr/thread/bind", "params": params, "error": rejected.error}));
        participants.push((reviewer.thread.id, reviewer_identity));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    for (source, target) in [(0, 1), (1, 0), (0, 2), (2, 0), (0, 3), (3, 0)] {
        for kind in ["task", "question", "answer", "result", "command"] {
            let envelope = json!({"version": 1, "messageId": "unsupported", "senderAgentId": participants[source].1.agent_id,
                "recipientAgentId": participants[target].1.agent_id, "identity": participants[target].1,
                "kind": kind, "correlationId": "unsupported-correlation", "headSha": anchor,
                "payload": {"instruction": "Review this immutable snapshot", "headSha": anchor}});
            let (method, params) = if source == 1 {
                (
                    "bgr/message/send",
                    json!({"sourceThreadId": participants[source].0, "threadId": participants[target].0, "envelope": envelope}),
                )
            } else {
                (
                    "bgr/message/deliver",
                    json!({"threadId": participants[target].0, "sender": BgrSender::from(&participants[source].1), "envelope": envelope}),
                )
            };
            let before = server.received_requests().await.unwrap().len();
            let id = mcp.send_raw_request(method, Some(params.clone())).await?;
            let rejected = timeout(
                TIMEOUT,
                mcp.read_stream_until_error_message(RequestId::Integer(id)),
            )
            .await??;
            let after = server.received_requests().await.unwrap().len();
            assert_eq!(after, before, "rejected {kind} invoked provider");
            trace["rejections"].as_array_mut().unwrap().push(
                json!({"method": method, "params": params, "error": rejected.error,
                "providerRequestsBefore": before, "providerRequestsAfter": after}),
            );
        }
    }
    for (index, (worker, worker_identity, _, _)) in workers.iter().enumerate() {
        let envelope = json!({"version": 1, "messageId": query_ids[index], "senderAgentId": brain_identity.agent_id,
            "recipientAgentId": worker_identity.agent_id, "identity": worker_identity, "kind": "status_query",
            "correlationId": correlations[index], "headSha": anchor, "payload": {"scope": "now"}});
        let (method, params) = if index == 0 {
            (
                "bgr/message/deliver",
                json!({"threadId": worker.thread.id, "sender": BgrSender::from(&brain_identity), "envelope": envelope}),
            )
        } else {
            (
                "bgr/message/send",
                json!({"sourceThreadId": brain.thread.id, "threadId": worker.thread.id, "envelope": envelope}),
            )
        };
        let receipt: BgrMessageSendResponse = rpc(&mut mcp, method, params.clone()).await?;
        assert!(receipt.accepted);
        assert!(!receipt.duplicate);
        trace["queries"]
            .as_array_mut()
            .unwrap()
            .push(json!({"method": method, "params": params, "result": receipt}));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
    for index in (0..2).rev() {
        let (worker, worker_identity, turn, request_id) = &workers[index];
        mcp.send_response(
            request_id.clone(),
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
        assert_eq!(reply.thread_id, worker.thread.id);
        assert_eq!(reply.envelope.identity, *worker_identity);
        assert_eq!(reply.envelope.sender_agent_id, worker_identity.agent_id);
        assert_eq!(reply.envelope.recipient_agent_id, brain_identity.agent_id);
        assert_eq!(
            reply.envelope.in_reply_to.as_deref(),
            Some(query_ids[index].as_str())
        );
        assert_eq!(reply.envelope.correlation_id, correlations[index]);
        assert_eq!(reply.envelope.head_sha.as_deref(), Some(anchor.as_str()));
        let completed: TurnCompletedNotification =
            timeout(TIMEOUT, mcp.read_notification("turn/completed")).await??;
        assert_eq!(completed.turn.id, turn.turn.id);
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        let snapshot: BgrThreadStateResponse = rpc(
            &mut mcp,
            "bgr/thread/inspect",
            json!({"threadId": worker.thread.id}),
        )
        .await?;
        assert!(snapshot.snapshot.pending.is_empty());
        assert_eq!(snapshot.snapshot.replies, vec![reply.envelope.clone()]);
        trace["notifications"]
            .as_array_mut()
            .unwrap()
            .push(json!({"method": "bgr/message", "params": reply, "completed": completed}));
    }
    let bodies = server.received_requests().await.unwrap();
    assert_eq!(bodies.len(), 6);
    for (request_index, worker_index) in [(2, 1), (4, 0)] {
        let body: Value = bodies[request_index].body_json()?;
        let input = body["input"].to_string();
        assert!(input.contains(&query_ids[worker_index]));
        assert!(!input.contains(&query_ids[1 - worker_index]));
    }
    trace["providerRequests"] = bodies.iter().map(|request| {
        let body: Value = request.body_json().unwrap();
        json!({"model": body["model"], "input": body["input"].as_array().unwrap().iter().filter(|item| item["role"] != "developer").collect::<Vec<_>>()})
    }).collect();
    eprintln!("BGR_VALIDATION_TRACE: {trace}");
    Ok(())
}

async fn rpc<T: DeserializeOwned>(
    mcp: &mut TestAppServer,
    method: &str,
    params: Value,
) -> Result<T> {
    let id = mcp.send_raw_request(method, Some(params)).await?;
    timeout(TIMEOUT, mcp.read_response(id)).await?
}

fn identity(thread: &str, role: BgrRole) -> BgrIdentity {
    BgrIdentity {
        team_id: "0".repeat(32),
        agent_id: if role == BgrRole::Brain { "a" } else { "b" }.repeat(32),
        session_id: thread.into(),
        task_id: (role != BgrRole::Brain).then(|| "3".repeat(32)),
        attempt_id: (role != BgrRole::Brain).then(|| "4".repeat(32)),
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
    assert_eq!(
        capabilities.status_scopes,
        vec!["now", "since_time", "since_commit"]
    );
    assert!(capabilities.reviewer_sandbox);
    assert!(capabilities.reviewer_start);
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
        recipient_identity: None,
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
        stale_identity.attempt_id = Some("stale-attempt".into());
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
                json!({"scope": "since_time", "since_time": "not-a-time"}),
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
        let bodies = server
            .received_requests()
            .await
            .expect("captured native Responses requests");
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
    let bodies = server
        .received_requests()
        .await
        .expect("captured native Responses requests");
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
    let mut reviewer = target.clone();
    reviewer.role = BgrRole::Reviewer;
    for (method, params) in [
        ("bgr/capabilities/read", json!({})),
        (
            "bgr/message/record",
            json!({"threadId":"unloaded-native-thread","identity":target,"envelope":{"version":1,"messageId":"record","senderAgentId":sender.agent_id,"recipientAgentId":target.agent_id,"identity":target,"kind":"status_query","correlationId":"correlation","payload":{"scope":"now"}}}),
        ),
        (
            "bgr/reviewer/start",
            json!({"identity":reviewer,"peers":[],"review":{"repository":".","baseSha":"a".repeat(40),"headSha":"b".repeat(40)}}),
        ),
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
