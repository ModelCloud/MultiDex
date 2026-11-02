//! Shared argument parsing and dispatch for the v2 agent messaging tools.
//!
//! `send_message` and `followup_task` share the same submission path and differ only in whether the
//! resulting `InterAgentCommunication` should wake the target immediately.

use super::analytics::ToolCallAnalytics;
use super::*;
use crate::TurnStartOptions;
use crate::agent::api::AgentInput;
use crate::agent::api::AgentTarget;
use crate::agent::api::SendRequest;
use crate::agent::child_config::build_agent_resume_config;
use crate::agent::types::MessageDeliveryMode;
use crate::tools::context::FunctionToolOutput;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Input for the MultiAgentV2 `send_message` tool.
pub(crate) struct SendMessageArgs {
    pub(crate) target: String,
    pub(crate) message: String,
    /// Provider id override applied to the target's subsequent turns.
    pub(crate) provider: Option<String>,
    /// Model override applied to the target's subsequent turns.
    pub(crate) model: Option<String>,
    /// Reasoning effort override applied to the target's subsequent turns.
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

/// Optional model/provider/effort overrides a messaging tool applies to the target agent.
#[derive(Debug, Default)]
pub(crate) struct AgentSettingsOverrides {
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

impl AgentSettingsOverrides {
    pub(crate) fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none() && self.reasoning_effort.is_none()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Input for the MultiAgentV2 `followup_task` tool.
pub(crate) struct FollowupTaskArgs {
    pub(crate) target: String,
    pub(crate) message: String,
    /// Provider id override applied to the target's subsequent turns.
    pub(crate) provider: Option<String>,
    /// Model override applied to the target's subsequent turns.
    pub(crate) model: Option<String>,
    /// Reasoning effort override applied to the target's subsequent turns.
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

pub(super) fn message_content(message: String) -> Result<String, FunctionCallError> {
    if message.trim().is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "Empty message can't be sent to an agent".to_string(),
        ));
    }
    Ok(message)
}

/// Handles the shared MultiAgentV2 message flow for both `send_message` and `followup_task`.
pub(super) async fn handle_message_string_tool(
    invocation: ToolInvocation,
    mode: MessageDeliveryMode,
    target: String,
    message: String,
    settings: Option<AgentSettingsOverrides>,
    analytics: &mut ToolCallAnalytics,
) -> Result<FunctionToolOutput, FunctionCallError> {
    let message = message_content(message)?;
    let ToolInvocation {
        session,
        turn,
        call_id,
        source,
        ..
    } = invocation;
    let receiver_thread_id = resolve_agent_target(&session, &turn, &target).await?;
    analytics.set_receiver(receiver_thread_id);
    let mut resume_config =
        build_agent_resume_config(&turn).map_err(FunctionCallError::RespondToModel)?;
    if let Some(overrides) = settings {
        apply_agent_settings_overrides(
            &session,
            &turn,
            receiver_thread_id,
            overrides,
            &mut resume_config,
        )
        .await?;
    }
    let receipt = session
        .services
        .agent_control
        .send(SendRequest {
            caller: session.thread_id,
            target: AgentTarget::Id(receiver_thread_id),
            resume_config,
            input: AgentInput::Message {
                message: agent_message_from_tool(message, &source),
                mode,
            },
            start_options: TurnStartOptions {
                parent_turn_id: (mode == MessageDeliveryMode::TriggerTurn)
                    .then(|| turn.sub_id.clone()),
                root_turn_id: turn.turn_metadata_state.root_turn_id(),
                turn_trigger: turn.turn_metadata_state.current_turn_trigger(),
                cyber_access_program: turn.cyber_access_program,
                ..Default::default()
            },
        })
        .await
        .map_err(|err| collab_v2_agent_error(receiver_thread_id, err))?;
    let receiver_agent_path = receipt.metadata.agent_path.ok_or_else(|| {
        FunctionCallError::RespondToModel("target agent is missing an agent_path".to_string())
    })?;
    emit_sub_agent_activity(
        &session,
        &turn,
        SubAgentActivityItem {
            model: None,
            reasoning_effort: None,
            id: call_id,
            agent_thread_id: receiver_thread_id,
            agent_path: receiver_agent_path,
            kind: SubAgentActivityKind::Interacted,
        },
    )
    .await;

    Ok(FunctionToolOutput::from_text(String::new(), Some(true)))
}

/// Apply requested model/provider/effort overrides to the target agent.
///
/// Loaded targets get a real `thread/settings` update that affects subsequent turns.
/// Unloaded targets cannot receive ops, so the overrides ride the resume config that
/// `send` uses to restore them instead. Returns `true` when the update was applied live.
pub(super) async fn apply_agent_settings_overrides(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    receiver_thread_id: codex_protocol::ThreadId,
    overrides: AgentSettingsOverrides,
    resume_config: &mut crate::config::Config,
) -> Result<bool, FunctionCallError> {
    if overrides.is_empty() {
        return Ok(true);
    }
    let thread_settings = codex_protocol::protocol::ThreadSettingsOverrides {
        model: overrides.model.clone(),
        model_provider: overrides.provider.clone(),
        effort: overrides.reasoning_effort.clone().map(Some),
        ..Default::default()
    };
    match session
        .services
        .agent_control
        .update_thread_settings(
            session.thread_id,
            AgentTarget::Id(receiver_thread_id),
            thread_settings,
        )
        .await
    {
        Ok(()) => Ok(true),
        Err(err) => {
            if matches!(
                err.details(),
                codex_protocol::error::CodexErrorDetails::ThreadNotFound(_)
            ) {
                if let Some(provider_id) = overrides.provider.as_deref() {
                    let provider_info = turn
                        .config
                        .model_providers
                        .get(provider_id)
                        .cloned()
                        .ok_or_else(|| {
                            FunctionCallError::RespondToModel(format!(
                                "Unknown provider `{provider_id}`. Available providers: {}",
                                turn.config
                                    .model_providers
                                    .keys()
                                    .map(String::as_str)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                        })?;
                    resume_config.model_provider = provider_info;
                    resume_config.model_provider_id = provider_id.to_string();
                }
                if let Some(model) = overrides.model {
                    resume_config.model = Some(model);
                }
                if let Some(effort) = overrides.reasoning_effort {
                    resume_config.model_reasoning_effort = Some(effort);
                }
                return Ok(false);
            }
            Err(collab_v2_agent_error(receiver_thread_id, err))
        }
    }
}
