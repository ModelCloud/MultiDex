//! Host-managed assignments reuse native queued agent communication, never steering input.

use crate::CodexThread;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::bgr::BGR_VERSION;
use codex_protocol::bgr::BgrEnvelope;
use codex_protocol::bgr::BgrIdentity;
use codex_protocol::bgr::BgrMessageKind;
use codex_protocol::bgr::BgrReportStatusArgs;
use codex_protocol::bgr::BgrRole;
use codex_protocol::bgr::BgrSnapshot;
use codex_protocol::bgr::BgrStatusQuery;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use std::collections::BTreeMap;

const MAX_MESSAGES: usize = 128;
const MAX_BYTES: usize = 32 * 1024;

pub(crate) struct BgrRuntime {
    identity: BgrIdentity,
    cancelled: bool,
    sequence: u32,
    queries: BTreeMap<String, BgrEnvelope>,
    replies: BTreeMap<String, BgrEnvelope>,
}

fn invalid(message: impl Into<String>) -> CodexErr {
    CodexErr::InvalidRequest(message.into())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
}

impl BgrRuntime {
    fn new(identity: BgrIdentity) -> Result<Self> {
        if [
            &identity.team_id,
            &identity.agent_id,
            &identity.session_id,
            &identity.task_id,
            &identity.attempt_id,
        ]
        .into_iter()
        .any(|id| !valid_id(id))
            || identity.epoch == 0
        {
            return Err(invalid("invalid BGR identity or zero epoch"));
        }
        if identity.role == BgrRole::Reviewer {
            return Err(invalid(
                "reviewer sandbox enforcement is not supported by BGR v1",
            ));
        }
        Ok(Self {
            identity,
            cancelled: false,
            sequence: 0,
            queries: BTreeMap::new(),
            replies: BTreeMap::new(),
        })
    }

    fn check_identity(&self, identity: &BgrIdentity) -> Result<()> {
        if &self.identity != identity {
            return Err(invalid(
                "BGR assignment identity/epoch mismatch; inspect the live attempt",
            ));
        }
        if self.cancelled {
            return Err(invalid("BGR assignment is cancelled"));
        }
        Ok(())
    }

    fn validate_query(&self, sender: &BgrIdentity, envelope: &BgrEnvelope) -> Result<bool> {
        self.check_identity(&envelope.identity)?;
        if envelope.version != BGR_VERSION || envelope.kind != BgrMessageKind::StatusQuery {
            return Err(invalid("only BGR v1 status_query delivery is supported"));
        }
        if sender.role != BgrRole::Brain
            || self.identity.role != BgrRole::Grunt
            || sender.team_id != self.identity.team_id
            || sender.epoch != self.identity.epoch
            || envelope.sender_agent_id != sender.agent_id
            || envelope.recipient_agent_id != self.identity.agent_id
            || envelope.in_reply_to.is_some()
            || !valid_id(&envelope.message_id)
            || !valid_id(&envelope.correlation_id)
        {
            return Err(invalid(
                "BGR sender, recipient, role or correlation mismatch",
            ));
        }
        let query: BgrStatusQuery = serde_json::from_value(envelope.payload.clone())
            .map_err(|error| invalid(format!("invalid BGR status query: {error}")))?;
        if !matches!(query, BgrStatusQuery::Now {}) {
            return Err(invalid(
                "scoped BGR history is not supported; only scope=now is available",
            ));
        }
        if let Some(previous) = self.queries.get(&envelope.message_id) {
            return if previous == envelope {
                Ok(false)
            } else {
                Err(invalid("BGR message ID reused with different content"))
            };
        }
        if self
            .replies
            .values()
            .any(|reply| reply.message_id == envelope.message_id)
        {
            return Err(invalid("BGR query message ID is already used by a reply"));
        }
        if self.queries.len() >= MAX_MESSAGES {
            return Err(invalid(
                "BGR process-local query limit reached; host must checkpoint and reconcile",
            ));
        }
        Ok(true)
    }

    fn report(&mut self, report: BgrReportStatusArgs) -> Result<(BgrEnvelope, bool)> {
        self.check_identity(&self.identity)?;
        if !valid_id(&report.message_id)
            || report.payload.current_action.trim().is_empty()
            || serde_json::to_vec(&report)
                .map_err(|e| invalid(e.to_string()))?
                .len()
                > MAX_BYTES
        {
            return Err(invalid("invalid or oversized BGR status report"));
        }
        let query = self.queries.get(&report.in_reply_to).ok_or_else(|| {
            invalid("unknown BGR status query; cannot report another attempt's query")
        })?;
        let envelope = BgrEnvelope {
            version: BGR_VERSION,
            message_id: report.message_id,
            sender_agent_id: self.identity.agent_id.clone(),
            recipient_agent_id: query.sender_agent_id.clone(),
            identity: self.identity.clone(),
            kind: BgrMessageKind::StatusReply,
            correlation_id: query.correlation_id.clone(),
            in_reply_to: Some(query.message_id.clone()),
            head_sha: report.payload.head_sha.clone(),
            payload: serde_json::to_value(report.payload).map_err(|e| invalid(e.to_string()))?,
        };
        if let Some(previous) = self.replies.get(&query.message_id) {
            return if previous == &envelope {
                Ok((envelope, false))
            } else {
                Err(invalid("BGR query already answered with different content"))
            };
        }
        if self
            .replies
            .values()
            .any(|reply| reply.message_id == envelope.message_id)
            || self.queries.contains_key(&envelope.message_id)
        {
            return Err(invalid("BGR reply message ID is already in use"));
        }
        self.replies
            .insert(query.message_id.clone(), envelope.clone());
        self.sequence += 1;
        Ok((envelope, true))
    }

    fn snapshot(&self) -> BgrSnapshot {
        BgrSnapshot {
            identity: self.identity.clone(),
            cancelled: self.cancelled,
            sequence: self.sequence,
            pending: self
                .queries
                .iter()
                .filter(|(id, _)| !self.replies.contains_key(*id))
                .map(|(_, query)| query.clone())
                .collect(),
            replies: self.replies.values().cloned().collect(),
        }
    }
}

impl CodexThread {
    /// The trusted host binds a normal, delegation-disabled thread before assigning work.
    /// Bindings are process-local: they are not recovered by ordinary thread/resume.
    pub async fn bind_bgr(&self, identity: BgrIdentity) -> Result<BgrSnapshot> {
        {
            let binding = self.bgr.lock().await;
            if let Some(runtime) = binding.as_ref() {
                runtime.check_identity(&identity)?;
                return Ok(runtime.snapshot());
            }
        }
        let runtime = BgrRuntime::new(identity)?;
        if !matches!(
            self.session_source,
            SessionSource::Cli | SessionSource::VSCode | SessionSource::Exec | SessionSource::Mcp
        ) || self.enabled(Feature::Collab)
            || self.enabled(Feature::MultiAgentV2)
            || !self.enabled(Feature::DeferMailboxPreemption)
        {
            return Err(invalid(
                "BGR requires a normal thread with multi_agent=false, multi_agent_v2=false and defer_mailbox_preemption=true",
            ));
        }
        if !self
            .session
            .allows_bgr_binding(runtime.identity.role == BgrRole::Grunt)
            .await
        {
            return Err(invalid(
                "bind BGR before the first turn; grunt thread/start must include the advertised reportTool",
            ));
        }
        let mut binding = self.bgr.lock().await;
        if let Some(existing) = binding.as_ref() {
            existing.check_identity(&runtime.identity)?;
            return Ok(existing.snapshot());
        }
        let snapshot = runtime.snapshot();
        *binding = Some(runtime);
        Ok(snapshot)
    }

    pub async fn inspect_bgr(&self) -> Result<BgrSnapshot> {
        self.bgr
            .lock()
            .await
            .as_ref()
            .map(BgrRuntime::snapshot)
            .ok_or_else(|| invalid("thread has no live BGR binding"))
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "queue submission and query registration must be atomic with replies and cancellation; never waits for worker progress"
    )]
    pub async fn deliver_bgr(&self, sender: &BgrIdentity, envelope: BgrEnvelope) -> Result<bool> {
        let mut binding = self.bgr.lock().await;
        let runtime = binding
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?;
        if !runtime.validate_query(sender, &envelope)? {
            return Ok(false);
        }
        let content = serde_json::to_string(&envelope).map_err(|e| invalid(e.to_string()))?;
        if content.len() > MAX_BYTES {
            return Err(invalid("BGR envelope is too large"));
        }
        let communication = InterAgentCommunication::new(
            AgentPath::root(),
            AgentPath::root(),
            Vec::new(),
            content,
            true,
        );
        self.submit(Op::InterAgentCommunication {
            communication,
            start_options: Default::default(),
        })
        .await?;
        runtime
            .queries
            .insert(envelope.message_id.clone(), envelope);
        runtime.sequence += 1;
        Ok(true)
    }

    /// Authenticates the reporting worker from the live native tool invocation, not model IDs.
    pub async fn report_bgr_status(
        &self,
        report: BgrReportStatusArgs,
    ) -> Result<(BgrEnvelope, bool)> {
        self.bgr
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?
            .report(report)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "interrupt submission and the cancellation fence must be ordered with status delivery"
    )]
    pub async fn cancel_bgr(&self, identity: &BgrIdentity) -> Result<BgrSnapshot> {
        let mut binding = self.bgr.lock().await;
        let runtime = binding
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?;
        if &runtime.identity != identity {
            return Err(invalid("BGR cancellation identity mismatch"));
        }
        if !runtime.cancelled {
            self.submit(Op::Interrupt).await?;
            runtime.cancelled = true;
            runtime.sequence += 1;
        }
        Ok(runtime.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::bgr::BgrStatusReply;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn identity(role: BgrRole) -> BgrIdentity {
        BgrIdentity {
            team_id: "team".into(),
            agent_id: if role == BgrRole::Brain {
                "brain"
            } else {
                "grunt-one"
            }
            .into(),
            session_id: "019b9048-a066-7000-8000-000000000001".into(),
            task_id: "task".into(),
            attempt_id: "attempt".into(),
            epoch: 1,
            role,
        }
    }

    fn query(id: &str) -> BgrEnvelope {
        BgrEnvelope {
            version: BGR_VERSION,
            message_id: id.into(),
            sender_agent_id: "brain".into(),
            recipient_agent_id: "grunt-one".into(),
            identity: identity(BgrRole::Grunt),
            kind: BgrMessageKind::StatusQuery,
            correlation_id: format!("corr-{id}"),
            in_reply_to: None,
            payload: json!({"scope": "now"}),
            head_sha: None,
        }
    }

    fn report(id: &str) -> BgrReportStatusArgs {
        BgrReportStatusArgs {
            message_id: format!("reply-{id}"),
            in_reply_to: id.into(),
            payload: BgrStatusReply {
                current_action: "testing".into(),
                completed: vec![],
                pending: vec!["continue assignment".into()],
                blockers: vec![],
                head_sha: None,
                dirty: None,
                coverage_limits: vec!["worker self-report".into()],
            },
        }
    }

    #[test]
    fn bgr_duplicate_queries_and_reports_are_content_checked() {
        let mut state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
        let message = query("one");
        let sender = identity(BgrRole::Brain);
        assert!(state.validate_query(&sender, &message).unwrap());
        state
            .queries
            .insert(message.message_id.clone(), message.clone());
        assert!(!state.validate_query(&sender, &message).unwrap());
        let mut conflict = message;
        conflict.correlation_id = "changed".into();
        assert!(state.validate_query(&sender, &conflict).is_err());
        assert!(state.report(report("one")).unwrap().1);
        assert!(!state.report(report("one")).unwrap().1);
        let mut conflict = report("one");
        conflict.payload.current_action = "different".into();
        assert!(state.report(conflict).is_err());
        assert_eq!(state.snapshot().pending.len(), 0);
        assert_eq!(state.snapshot().replies.len(), 1);
    }

    #[test]
    fn bgr_out_of_order_replies_keep_target_attempt_and_correlation() {
        let mut state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
        for id in ["one", "two"] {
            let message = query(id);
            state.queries.insert(id.into(), message);
        }
        for id in ["two", "one"] {
            let (reply, fresh) = state.report(report(id)).unwrap();
            assert!(fresh);
            assert_eq!(reply.correlation_id, format!("corr-{id}"));
            assert_eq!(reply.in_reply_to, Some(id.into()));
            assert_eq!(reply.identity, identity(BgrRole::Grunt));
            assert_eq!(reply.sender_agent_id, "grunt-one");
        }
        assert!(!state.cancelled);
        assert_eq!(state.sequence, 2);
        assert_eq!(state.queries.len(), 2);
        assert!(state.report(report("unknown")).is_err());
    }

    #[test]
    fn bgr_fences_target_sender_role_epoch_and_team() {
        let state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
        let sender = identity(BgrRole::Brain);
        for field in ["epoch", "attempt", "session", "task", "team", "agent"] {
            let mut message = query("one");
            match field {
                "epoch" => message.identity.epoch += 1,
                "attempt" => message.identity.attempt_id.push('x'),
                "session" => message.identity.session_id.push('x'),
                "task" => message.identity.task_id.push('x'),
                "team" => message.identity.team_id.push('x'),
                _ => message.identity.agent_id.push('x'),
            }
            assert!(state.validate_query(&sender, &message).is_err());
        }
        for field in ["epoch", "team", "role", "agent"] {
            let mut spoofed = sender.clone();
            match field {
                "epoch" => spoofed.epoch += 1,
                "team" => spoofed.team_id.push('x'),
                "role" => spoofed.role = BgrRole::Grunt,
                _ => spoofed.agent_id.push('x'),
            }
            assert!(state.validate_query(&spoofed, &query("one")).is_err());
        }
    }

    #[test]
    fn bgr_cancelled_attempt_and_unsupported_scope_are_rejected() {
        let mut state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
        for payload in [
            json!({"scope": "since_time", "since": "2026-11-03T00:00:00Z"}),
            json!({"scope": "since_commit", "commit": "abc"}),
            json!({"scope": "now", "instructions": "untyped work"}),
        ] {
            let mut message = query("one");
            message.payload = payload;
            assert!(
                state
                    .validate_query(&identity(BgrRole::Brain), &message)
                    .is_err()
            );
        }
        state.cancelled = true;
        assert!(
            state
                .validate_query(&identity(BgrRole::Brain), &query("one"))
                .is_err()
        );
        assert!(state.report(report("one")).is_err());
    }

    #[test]
    fn bgr_reviewer_invalid_ids_and_zero_epochs_are_rejected() {
        assert!(BgrRuntime::new(identity(BgrRole::Reviewer)).is_err());
        let mut bad = identity(BgrRole::Brain);
        bad.agent_id = "invalid agent ID".into();
        assert!(BgrRuntime::new(bad).is_err());
        let mut bad = identity(BgrRole::Grunt);
        bad.epoch = 0;
        assert!(BgrRuntime::new(bad).is_err());
        let mut bad = identity(BgrRole::Grunt);
        bad.session_id.clear();
        assert!(BgrRuntime::new(bad).is_err());
    }

    #[test]
    fn bgr_bounds_queries_and_rejects_reused_reply_ids() {
        let mut state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
        for id in ["one", "two"] {
            state.queries.insert(id.into(), query(id));
        }
        state.report(report("one")).unwrap();
        let mut conflict = report("two");
        conflict.message_id = "reply-one".into();
        assert!(state.report(conflict).is_err());
        assert!(
            state
                .validate_query(&identity(BgrRole::Brain), &query("reply-one"))
                .is_err()
        );
        let mut huge = report("two");
        huge.payload.current_action = "x".repeat(MAX_BYTES);
        assert!(state.report(huge).is_err());
        for i in state.queries.len()..MAX_MESSAGES {
            let id = format!("q{i}");
            state.queries.insert(id.clone(), query(&id));
        }
        assert!(
            state
                .validate_query(&identity(BgrRole::Brain), &query("extra"))
                .is_err()
        );
    }
}
