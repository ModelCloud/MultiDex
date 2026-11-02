use super::analytics::ToolCallAnalytics;
use super::message_tool::AgentSettingsOverrides;
use super::*;
use crate::agent::api::AgentTarget;
use crate::tools::handlers::multi_agents_spec::create_set_agent_settings_tool;
use codex_tools::ToolSpec;
use serde::Deserialize;

pub(crate) struct Handler;

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("set_agent_settings")
    }

    fn spec(&self) -> ToolSpec {
        create_set_agent_settings_tool()
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let mut analytics = ToolCallAnalytics::new(&invocation, CollabAgentTool::SendMessage);
            let result = self.handle_call(invocation, &mut analytics).await;
            analytics.finish(&result);
            result
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
/// Input for the MultiAgentV2 `set_agent_settings` tool.
struct SetAgentSettingsArgs {
    target: String,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl Handler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
        analytics: &mut ToolCallAnalytics,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let arguments = function_arguments(invocation.payload.clone())?;
        let args: SetAgentSettingsArgs = parse_arguments(&arguments)?;
        let ToolInvocation {
            session,
            step_context,
            ..
        } = invocation;
        let turn = &step_context.turn;
        let overrides = AgentSettingsOverrides {
            provider: args.provider,
            model: args.model,
            reasoning_effort: args.reasoning_effort,
        };
        if overrides.is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "set_agent_settings requires at least one of `provider`, `model`, or `reasoning_effort`"
                    .to_string(),
            ));
        }
        let receiver_thread_id = resolve_agent_target(&session, turn, &args.target).await?;
        analytics.set_receiver(receiver_thread_id);
        // The control layer updates loaded targets live and persists overrides onto stored
        // metadata for unloaded ones so cold restore picks them up.
        session
            .services
            .agent_control
            .update_thread_settings(
                session.thread_id,
                AgentTarget::Id(receiver_thread_id),
                overrides.to_thread_settings_overrides(),
            )
            .await
            .map_err(|err| collab_v2_agent_error(receiver_thread_id, err))?;
        Ok(boxed_tool_output(
            crate::tools::context::FunctionToolOutput::from_text(String::new(), Some(true)),
        ))
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}
