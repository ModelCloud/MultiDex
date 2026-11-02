use super::analytics::ToolCallAnalytics;
use super::message_tool::SendMessageArgs;
use super::message_tool::handle_message_string_tool;
use super::*;
use crate::agent::types::MessageDeliveryMode;
use crate::tools::handlers::multi_agents_spec::create_send_message_tool;
use codex_tools::ToolSpec;

pub(crate) struct Handler {
    expose_model_overrides: bool,
}

impl Handler {
    pub(crate) fn new(expose_model_overrides: bool) -> Self {
        Self {
            expose_model_overrides,
        }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("send_message")
    }

    fn spec(&self) -> ToolSpec {
        create_send_message_tool(self.expose_model_overrides)
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

impl Handler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
        analytics: &mut ToolCallAnalytics,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let arguments = function_arguments(invocation.payload.clone())?;
        let args: SendMessageArgs = parse_arguments(&arguments)?;
        handle_message_string_tool(
            invocation,
            MessageDeliveryMode::QueueOnly,
            args.target,
            args.message,
            Some(super::message_tool::AgentSettingsOverrides {
                provider: args.provider,
                model: args.model,
                reasoning_effort: args.reasoning_effort,
            }),
            analytics,
        )
        .await
        .map(boxed_tool_output)
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}
