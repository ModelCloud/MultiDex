use super::*;
use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::types::SpawnAgentForkMode;
use crate::codex_thread::ThreadConfigSnapshot;
use crate::thread_spawn::ThreadSpawnError;
use crate::thread_spawn::ThreadSpawnRequest;
use crate::tools::handlers::multi_agents::collab_tool_call_status;
use crate::tools::handlers::multi_agents_spec::SpawnAgentToolOptions;
use crate::tools::handlers::multi_agents_spec::create_spawn_agent_tool_v2;
use crate::tools::handlers::multi_agents_v2::message_tool::message_content;
use crate::turn_timing::now_unix_timestamp_ms;
use codex_protocol::ThreadId;
use codex_protocol::protocol::MultiAgentVersion;
use codex_tools::ToolSpec;
use std::num::NonZeroUsize;

#[derive(Default)]
pub(crate) struct Handler {
    options: SpawnAgentToolOptions,
    description_override: Option<String>,
}

impl Handler {
    pub(crate) fn new(
        options: SpawnAgentToolOptions,
        description_override: Option<String>,
    ) -> Self {
        Self {
            options,
            description_override,
        }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("spawn_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_spawn_agent_tool_v2(self.options.clone(), self.description_override.as_deref())
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let analytics = invocation.session.services.analytics_events_client.clone();
            let sender_thread_id = invocation.session.thread_id;
            let turn_id = invocation.step_context.turn.sub_id.clone();
            let call_id = invocation.call_id.clone();
            let started_at_ms = now_unix_timestamp_ms();
            let result = handle_spawn_agent(invocation).await;
            let completed_at_ms = now_unix_timestamp_ms();
            let (status, receiver_thread_ids, agents_states) = match &result {
                Ok((_, thread_id, agent_status, _)) => (
                    collab_tool_call_status(agent_status, Some(*thread_id)),
                    vec![*thread_id],
                    [(*thread_id, agent_status.clone())].into_iter().collect(),
                ),
                Err(_) => (
                    CollabAgentToolCallStatus::Failed,
                    Vec::new(),
                    Default::default(),
                ),
            };
            let agent_snapshot = result.as_ref().ok().map(|(_, _, _, snapshot)| snapshot);

            analytics.track_collab_tool_call(
                turn_id,
                CollabAgentToolCallItem {
                    id: call_id,
                    tool: CollabAgentTool::SpawnAgent,
                    status,
                    sender_thread_id,
                    receiver_thread_ids,
                    receiver_agents: Vec::new(),
                    prompt: None,
                    model: agent_snapshot.map(|snapshot| snapshot.model.clone()),
                    reasoning_effort: agent_snapshot
                        .and_then(|snapshot| snapshot.reasoning_effort.clone()),
                    agents_states,
                },
                started_at_ms,
                completed_at_ms,
            );

            result.map(|(output, _, _, _)| boxed_tool_output(output))
        })
    }
}

async fn handle_spawn_agent(
    invocation: ToolInvocation,
) -> Result<
    (
        SpawnAgentResult,
        ThreadId,
        AgentStatus,
        ThreadConfigSnapshot,
    ),
    FunctionCallError,
> {
    let ToolInvocation {
        session,
        step_context,
        payload,
        call_id,
        source,
        ..
    } = invocation;
    let turn = &step_context.turn;
    let arguments = function_arguments(payload)?;
    let args: SpawnAgentArgs = parse_arguments(&arguments)?;
    let fork_mode = args.fork_mode()?;
    let message = message_content(args.message)?;
    let role_name = args
        .agent_type
        .as_deref()
        .map(str::trim)
        .filter(|role| !role.is_empty());

    let spawned = crate::thread_spawn::spawn(
        &session,
        &step_context,
        &call_id,
        ThreadSpawnRequest {
            task_name: args.task_name,
            message: agent_message_from_tool(message, &source),
            fork_mode: fork_mode.clone(),
            agent_type: args.agent_type.clone(),
            provider: args.provider,
            model: args.model,
            reasoning_effort: args.reasoning_effort,
            review_repository: args.review_repository,
            review_ref: args.review_ref,
            review_pull_request: args.review_pull_request,
        },
    )
    .await
    .map_err(|err| match err {
        ThreadSpawnError::Preparation(message) => FunctionCallError::RespondToModel(message),
        ThreadSpawnError::Backend(err) => {
            record_collab_spawn_failure(
                &turn.session_telemetry,
                turn.config.apps_mcp_product_sku.as_deref(),
                &err,
                &call_id,
                &turn.sub_id,
                fork_mode.as_ref(),
                MultiAgentVersion::V2,
            );
            collab_spawn_error(err)
        }
    })?;
    let spawned_agent = spawned.agent;
    let agent_snapshot = spawned.config;
    let new_agent_path = spawned.task_name;
    let new_thread_id = spawned_agent.thread_id;
    let agent_status = spawned_agent.status;
    let nickname = agent_snapshot
        .session_source
        .get_nickname()
        .or(spawned_agent.metadata.agent_nickname);
    emit_sub_agent_activity(
        &session,
        turn,
        SubAgentActivityItem {
            model: Some(agent_snapshot.model.clone()),
            reasoning_effort: agent_snapshot.reasoning_effort.clone(),
            id: call_id,
            agent_thread_id: new_thread_id,
            agent_path: new_agent_path.clone(),
            kind: SubAgentActivityKind::Started,
        },
    )
    .await;
    let role_tag = role_name.unwrap_or(DEFAULT_ROLE_NAME);
    turn.session_telemetry.counter(
        "codex.multi_agent.spawn",
        /*inc*/ 1,
        &[("role", role_tag), ("version", "v2")],
    );
    let task_name = String::from(new_agent_path);

    let hide_agent_metadata = turn.config.multi_agent_v2.hide_spawn_agent_metadata;
    let output = if hide_agent_metadata {
        SpawnAgentResult::HiddenMetadata { task_name }
    } else {
        SpawnAgentResult::WithNickname {
            task_name,
            nickname,
        }
    };
    Ok((output, new_thread_id, agent_status, agent_snapshot))
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    message: String,
    task_name: String,
    agent_type: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    review_repository: Option<String>,
    review_ref: Option<String>,
    review_pull_request: Option<u64>,
    fork_turns: Option<String>,
    fork_context: Option<bool>,
}

impl SpawnAgentArgs {
    fn fork_mode(&self) -> Result<Option<SpawnAgentForkMode>, FunctionCallError> {
        if self.fork_context.is_some() {
            return Err(FunctionCallError::RespondToModel(
                "fork_context is not supported in MultiAgentV2; use fork_turns instead".to_string(),
            ));
        }

        let is_reviewer = self.agent_type.as_deref() == Some("reviewer");
        let fork_turns = self
            .fork_turns
            .as_deref()
            .map(str::trim)
            .filter(|fork_turns| !fork_turns.is_empty())
            .unwrap_or(if is_reviewer { "none" } else { "all" });

        if is_reviewer && !fork_turns.eq_ignore_ascii_case("none") {
            return Err(FunctionCallError::RespondToModel(
                "reviewer agents require fork_turns=`none` to keep every review independent"
                    .to_string(),
            ));
        }

        if fork_turns.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        // Accept legacy turn counts without limiting the inherited history.
        if fork_turns.eq_ignore_ascii_case("all") || fork_turns.parse::<NonZeroUsize>().is_ok() {
            return Ok(Some(SpawnAgentForkMode::FullHistory));
        }

        Err(FunctionCallError::RespondToModel(
            "fork_turns must be `none` or `all`".to_string(),
        ))
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn args(agent_type: Option<&str>, fork_turns: Option<&str>) -> SpawnAgentArgs {
        SpawnAgentArgs {
            message: "Review the changed files".to_string(),
            task_name: "review".to_string(),
            agent_type: agent_type.map(str::to_string),
            provider: None,
            model: None,
            reasoning_effort: None,
            review_repository: None,
            review_ref: None,
            review_pull_request: None,
            fork_turns: fork_turns.map(str::to_string),
            fork_context: None,
        }
    }

    #[test]
    fn reviewers_always_start_without_parent_history() {
        assert!(args(Some("reviewer"), None).fork_mode().unwrap().is_none());
        assert!(
            args(Some("reviewer"), Some("none"))
                .fork_mode()
                .unwrap()
                .is_none()
        );
        assert_eq!(
            args(Some("reviewer"), Some("all")).fork_mode(),
            Err(FunctionCallError::RespondToModel(
                "reviewer agents require fork_turns=`none` to keep every review independent"
                    .to_string(),
            ))
        );
        assert!(args(None, None).fork_mode().unwrap().is_some());
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum SpawnAgentResult {
    WithNickname {
        task_name: String,
        nickname: Option<String>,
    },
    HiddenMetadata {
        task_name: String,
    },
}

impl ToolOutput for SpawnAgentResult {
    fn log_output(&self) -> String {
        tool_output_json_text(self, "spawn_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "spawn_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "spawn_agent")
    }
}
