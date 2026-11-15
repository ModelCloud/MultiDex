//! Implements the MultiAgentV2 collaboration tool surface.

use crate::agent::AgentStatus;
use crate::agent::agent_resolver::resolve_agent_target;
use crate::agent::types::AgentMessage;
use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::*;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_protocol::items::CollabAgentTool;
use codex_protocol::items::CollabAgentToolCallItem;
use codex_protocol::items::CollabAgentToolCallStatus;
use codex_protocol::items::SubAgentActivityItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::SubAgentActivityKind;
use codex_tools::ToolName;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

pub(crate) use followup_task::Handler as FollowupTaskHandler;
pub(crate) use interrupt_agent::Handler as InterruptAgentHandler;
pub(crate) use list_agents::Handler as ListAgentsHandler;
pub(crate) use resolve_model::Handler as ResolveModelHandler;
pub(crate) use send_message::Handler as SendMessageHandler;
pub(crate) use set_agent_settings::Handler as SetAgentSettingsHandler;
pub(crate) use spawn::Handler as SpawnAgentHandler;
pub(crate) use wait::Handler as WaitAgentHandler;

mod analytics;
mod followup_task;
mod interrupt_agent;
mod list_agents;
mod message_tool;
mod resolve_model;
mod send_message;
mod set_agent_settings;
mod spawn;
pub(crate) mod wait;

pub(crate) async fn emit_sub_agent_activity(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    item: SubAgentActivityItem,
) {
    let item = TurnItem::SubAgentActivity(item);
    session.emit_turn_item_started(turn, &item).await;
    session.emit_turn_item_completed(turn, item).await;
}

fn agent_message_from_tool(
    message: String,
    source: &crate::tools::context::ToolCallSource,
    origin_is_openai: bool,
) -> AgentMessage {
    // Only OpenAI's direct collaboration calls can carry decryptable message
    // ciphertext. Other providers send ordinary text even when the tool schema
    // marks the parameter as encrypted. Labeling that text as ciphertext makes
    // a cross-provider child fail before it can read its first task.
    if !origin_is_openai
        || matches!(
            source,
            crate::tools::context::ToolCallSource::DirectPlaintextMessage
                | crate::tools::context::ToolCallSource::CodeMode { .. }
        )
    {
        AgentMessage::Plaintext(message)
    } else {
        AgentMessage::Encrypted(message)
    }
}

#[cfg(test)]
mod agent_message_tests {
    use super::*;
    use crate::tools::context::ToolCallSource;

    #[test]
    fn non_openai_direct_messages_are_plaintext_for_cross_provider_children() {
        assert!(matches!(
            agent_message_from_tool("review this".into(), &ToolCallSource::Direct, false),
            AgentMessage::Plaintext(message) if message == "review this"
        ));
        assert!(matches!(
            agent_message_from_tool("ciphertext".into(), &ToolCallSource::Direct, true),
            AgentMessage::Encrypted(message) if message == "ciphertext"
        ));
    }
}
