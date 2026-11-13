use crate::JsonSchema;
use crate::TS;
pub use codex_protocol::bgr::*;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BgrCapabilitiesReadParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BgrCapabilitiesReadResponse {
    pub version: u32,
    pub roles: Vec<BgrRole>,
    pub status_scopes: Vec<String>,
    pub message_kinds: Vec<BgrMessageKind>,
    pub report_tool: codex_protocol::dynamic_tools::DynamicToolSpec,
    pub send_tool: codex_protocol::dynamic_tools::DynamicToolSpec,
    pub reply_tool: codex_protocol::dynamic_tools::DynamicToolSpec,
    pub general_messaging: bool,
    pub message_callbacks: bool,
    pub consumption_receipts: bool,
    pub status_evidence: String,
    pub live_reattach: bool,
    pub durable_mailbox: bool,
    pub restart_recovery: bool,
    pub reviewer_sandbox: bool,
    pub reviewer_start: bool,
    pub reviewer_snapshot_no_lazy_fetch: bool,
    pub review_snapshot_tool: codex_protocol::dynamic_tools::DynamicToolSpec,
    pub host_delivery: bool,
    pub host_record: bool,
    pub cross_host_delivery: bool,
    pub task_dispatch: bool,
    pub max_queries_per_attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrThreadBindParams {
    pub thread_id: String,
    pub identity: BgrIdentity,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrThreadInspectParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrThreadPeersParams {
    pub thread_id: String,
    pub identity: BgrIdentity,
    pub peers: Vec<BgrIdentity>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrMessageSendParams {
    pub source_thread_id: String,
    pub thread_id: String,
    pub envelope: BgrEnvelope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrMessageDeliverParams {
    pub thread_id: String,
    /// The host authenticates and authorizes this sender before calling the bridge.
    pub sender: BgrSender,
    pub envelope: BgrEnvelope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct BgrMessageRecordParams {
    pub thread_id: String,
    pub identity: BgrIdentity,
    pub envelope: BgrEnvelope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BgrMessageSendResponse {
    pub message_id: String,
    pub duplicate: bool,
    /// Process-local acceptance (source metadata for record), not consumption or durability.
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BgrThreadStateResponse {
    pub snapshot: BgrSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BgrReviewerStartResponse {
    pub thread_id: String,
    pub snapshot: BgrSnapshot,
    pub review: BgrReviewManifest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct BgrMessageNotification {
    pub thread_id: String,
    pub envelope: BgrEnvelope,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientRequest;
    use crate::ClientRequestSerializationScope;
    use crate::experimental_api::ExperimentalApi;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn bgr_rpc_is_experimental_and_serialized_with_native_thread_operations() {
        let request: ClientRequest = serde_json::from_value(json!({
            "id": 1, "method": "bgr/thread/inspect", "params": {"threadId": "worker-thread"}
        }))
        .unwrap();
        assert_eq!(request.experimental_reason(), Some("bgr/thread/inspect"));
        assert_eq!(
            request.serialization_scope(),
            Some(ClientRequestSerializationScope::Thread {
                thread_id: "worker-thread".into()
            })
        );
        assert_eq!(
            serde_json::to_value(request).unwrap()["method"],
            "bgr/thread/inspect"
        );
    }

    #[test]
    fn bgr_envelopes_reject_unknown_fields_and_round_trip_correlation() {
        let mut envelope = json!({"version": 1, "messageId": "query-1", "senderAgentId": "brain", "recipientAgentId": "grunt-one",
            "identity": {"teamId": "team", "agentId": "grunt-one", "sessionId": "thread", "taskId": "task", "attemptId": "attempt", "epoch": 1, "role": "grunt"},
            "kind": "status_query", "correlationId": "correlation-1", "inReplyTo": null, "payload": {"scope": "now"}, "headSha": null});
        let parsed: BgrEnvelope = serde_json::from_value(envelope.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), envelope);
        envelope["instructions"] = json!("arbitrary untyped assignment");
        assert!(serde_json::from_value::<BgrEnvelope>(envelope).is_err());
    }

    #[test]
    fn bgr_host_delivery_is_experimental_thread_scoped_and_has_no_source_thread() {
        let mut value = json!({"id": 1, "method": "bgr/message/deliver", "params": {
            "threadId": "worker-thread",
            "sender": {"teamId": "team", "agentId": "brain", "sessionId": "claude-session", "epoch": 1, "role": "brain"},
            "envelope": {"version": 1, "messageId": "query", "senderAgentId": "brain", "recipientAgentId": "worker",
                "identity": {"teamId": "team", "agentId": "worker", "sessionId": "host-worker-session", "taskId": "task", "attemptId": "attempt", "epoch": 1, "role": "grunt"},
                "kind": "status_query", "correlationId": "correlation", "payload": {"scope": "now"}}
        }});
        let request: ClientRequest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(request.experimental_reason(), Some("bgr/message/deliver"));
        assert_eq!(
            request.serialization_scope(),
            Some(ClientRequestSerializationScope::Thread {
                thread_id: "worker-thread".into()
            })
        );
        value["params"]["sender"]["taskId"] = json!("not-an-assignment");
        assert!(serde_json::from_value::<ClientRequest>(value).is_err());
    }
}
