//! Version one of the opt-in host-managed worker status bridge.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

pub const BGR_VERSION: u32 = 1;
pub const BGR_REPORT_TOOL: &str = "bgr_report_status";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum BgrRole {
    Brain,
    Grunt,
    Reviewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrIdentity {
    pub team_id: String,
    pub agent_id: String,
    /// Host session identity, not the native thread UUID used by RPC threadId.
    pub session_id: String,
    pub task_id: String,
    pub attempt_id: String,
    #[ts(type = "number")]
    pub epoch: u64,
    pub role: BgrRole,
}

/// Controller-attested sender on the trusted host bridge, not a worker assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrSender {
    pub team_id: String,
    pub agent_id: String,
    pub session_id: String,
    #[ts(type = "number")]
    pub epoch: u64,
    pub role: BgrRole,
}

impl From<&BgrIdentity> for BgrSender {
    fn from(identity: &BgrIdentity) -> Self {
        Self {
            team_id: identity.team_id.clone(),
            agent_id: identity.agent_id.clone(),
            session_id: identity.session_id.clone(),
            epoch: identity.epoch,
            role: identity.role,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub enum BgrStatusQuery {
    Now {},
    SinceTime { since_time: String },
    SinceCommit { since_commit: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum BgrMessageKind {
    Task,
    Question,
    Answer,
    Progress,
    StatusQuery,
    StatusReply,
    Result,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrEnvelope {
    pub version: u32,
    pub message_id: String,
    pub sender_agent_id: String,
    pub recipient_agent_id: String,
    /// The target assignment for queries; the reporting assignment for replies.
    pub identity: BgrIdentity,
    pub kind: BgrMessageKind,
    pub correlation_id: String,
    pub in_reply_to: Option<String>,
    pub payload: serde_json::Value,
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrStatusReply {
    pub current_action: String,
    pub completed: Vec<String>,
    pub pending: Vec<String>,
    pub blockers: Vec<String>,
    /// Worker-supplied evidence, not a backend attestation of repository state.
    pub head_sha: Option<String>,
    pub dirty: Option<bool>,
    /// Must describe missing evidence; scoped history is not synthesized by the backend.
    pub coverage_limits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrReportStatusArgs {
    pub message_id: String,
    pub in_reply_to: String,
    pub payload: BgrStatusReply,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BgrSnapshot {
    pub identity: BgrIdentity,
    pub cancelled: bool,
    /// Process-local event watermark, not a transcript position or a durable lease.
    pub sequence: u32,
    pub pending: Vec<BgrEnvelope>,
    pub replies: Vec<BgrEnvelope>,
}

pub fn report_tool() -> crate::dynamic_tools::DynamicToolSpec {
    crate::dynamic_tools::DynamicToolSpec::Function(crate::dynamic_tools::DynamicToolFunctionSpec {
        name: BGR_REPORT_TOOL.into(),
        description: "Answer a received BGR status_query at a safe boundary, then continue the same assignment. Use its messageId as inReplyTo. This is not task completion. Report only observed evidence and explicitly list coverage limits.".into(),
        input_schema: serde_json::json!(schemars::schema_for!(BgrReportStatusArgs)),
        defer_loading: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn bgr_reserved_scopes_preserve_host_payload_field_names() {
        for (query, payload) in [
            (
                BgrStatusQuery::SinceTime {
                    since_time: "2026-11-03T00:00:00Z".into(),
                },
                json!({"scope": "since_time", "since_time": "2026-11-03T00:00:00Z"}),
            ),
            (
                BgrStatusQuery::SinceCommit {
                    since_commit: "anchor-a".into(),
                },
                json!({"scope": "since_commit", "since_commit": "anchor-a"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&query).unwrap(), payload);
            assert_eq!(
                serde_json::from_value::<BgrStatusQuery>(payload).unwrap(),
                query
            );
        }
        for legacy in [
            json!({"scope": "since_time", "since": "2026-11-03T00:00:00Z"}),
            json!({"scope": "since_commit", "commit": "anchor-a"}),
        ] {
            assert!(serde_json::from_value::<BgrStatusQuery>(legacy).is_err());
        }
    }
}
