//! Host-managed assignments reuse native queued agent communication, never steering input.

use crate::CodexThread;
use codex_features::Feature;
use codex_protocol::AgentPath;
use codex_protocol::bgr::BGR_VERSION;
use codex_protocol::bgr::BgrEnvelope;
use codex_protocol::bgr::BgrIdentity;
use codex_protocol::bgr::BgrMessageKind;
use codex_protocol::bgr::BgrReplyMessageArgs;
use codex_protocol::bgr::BgrReportStatusArgs;
use codex_protocol::bgr::BgrRole;
use codex_protocol::bgr::BgrSendMessageArgs;
use codex_protocol::bgr::BgrSender;
use codex_protocol::bgr::BgrSnapshot;
use codex_protocol::bgr::BgrStatusQuery;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

const MAX_MESSAGES: usize = 128;
pub(crate) struct BgrBinding;

pub(crate) fn is_delegation_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "spawn_agents_on_csv"
            | "send_input"
            | "wait"
            | "wait_agent"
            | "resume_agent"
            | "close_agent"
            | "send_message"
            | "followup_task"
            | "interrupt_agent"
            | "list_agents"
    )
}
const MAX_BYTES: usize = 32 * 1024;

pub(crate) struct BgrRuntime {
    identity: BgrIdentity,
    cancelled: bool,
    mail_cancellation: CancellationToken,
    sequence: u32,
    queries: BTreeMap<String, BgrEnvelope>,
    replies: BTreeMap<String, BgrEnvelope>,
    peers: BTreeMap<String, BgrIdentity>,
    outgoing: BTreeMap<String, BgrEnvelope>,
}

fn invalid(message: impl Into<String>) -> CodexErr {
    CodexErr::InvalidRequest(format!("BGR: {}", message.into()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
}

fn validate_identity(identity: &BgrIdentity) -> Result<()> {
    if [&identity.team_id, &identity.agent_id, &identity.session_id]
        .into_iter()
        .any(|id| !valid_id(id))
        || identity.epoch == 0
        || identity.task_id.as_ref().is_some_and(|id| !valid_id(id))
        || identity.attempt_id.as_ref().is_some_and(|id| !valid_id(id))
        || (identity.role != BgrRole::Brain
            && (identity.task_id.is_none() || identity.attempt_id.is_none()))
        || identity.task_id.is_some() != identity.attempt_id.is_some()
    {
        return Err(invalid("invalid BGR identity, assignment or zero epoch"));
    }
    Ok(())
}

fn validate_envelope(envelope: &BgrEnvelope) -> Result<()> {
    if envelope.version != BGR_VERSION
        || !valid_id(&envelope.message_id)
        || !valid_id(&envelope.correlation_id)
        || envelope
            .in_reply_to
            .as_ref()
            .is_some_and(|id| !valid_id(id))
        || !envelope.payload.is_object()
        || envelope
            .head_sha
            .as_ref()
            .is_some_and(|sha| sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || serde_json::to_vec(envelope)
            .map_err(|error| invalid(error.to_string()))?
            .len()
            > MAX_BYTES
    {
        return Err(invalid("invalid or oversized BGR envelope"));
    }
    if envelope.kind == BgrMessageKind::StatusQuery {
        let query: BgrStatusQuery = serde_json::from_value(envelope.payload.clone())
            .map_err(|error| invalid(format!("invalid BGR status query: {error}")))?;
        match query {
            BgrStatusQuery::Now {} => {}
            BgrStatusQuery::SinceTime { since_time } => {
                chrono::DateTime::parse_from_rfc3339(&since_time)
                    .map_err(|_| invalid("since_time requires an RFC3339 timestamp"))?;
            }
            BgrStatusQuery::SinceCommit { since_commit } => {
                if since_commit.len() != 40
                    || !since_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(invalid("since_commit requires a full 40-hex commit"));
                }
            }
        }
    }
    if envelope.kind == BgrMessageKind::StatusReply {
        let reply: codex_protocol::bgr::BgrStatusReply =
            serde_json::from_value(envelope.payload.clone())
                .map_err(|error| invalid(format!("invalid BGR status reply: {error}")))?;
        if reply.current_action.trim().is_empty()
            || !reply
                .coverage_limits
                .iter()
                .any(|limit| !limit.trim().is_empty())
        {
            return Err(invalid(
                "BGR status requires current action and explicit evidence limits",
            ));
        }
    }
    Ok(())
}

impl BgrRuntime {
    fn new(identity: BgrIdentity) -> Result<Self> {
        validate_identity(&identity)?;
        if identity.role == BgrRole::Reviewer {
            return Err(invalid(
                "reviewer sandbox enforcement is not supported by BGR v1",
            ));
        }
        Ok(Self {
            identity,
            cancelled: false,
            mail_cancellation: CancellationToken::new(),
            sequence: 0,
            queries: BTreeMap::new(),
            replies: BTreeMap::new(),
            peers: BTreeMap::new(),
            outgoing: BTreeMap::new(),
        })
    }

    fn set_peers(&mut self, identity: &BgrIdentity, peers: Vec<BgrIdentity>) -> Result<()> {
        self.check_identity(identity)?;
        if peers.len() > MAX_MESSAGES {
            return Err(invalid("BGR peer limit exceeded"));
        }
        let mut authorized = BTreeMap::new();
        for peer in peers {
            validate_identity(&peer)?;
            if peer.team_id != identity.team_id
                || peer.epoch != identity.epoch
                || peer.agent_id == identity.agent_id
                || (peer.role == BgrRole::Brain) == (identity.role == BgrRole::Brain)
                || authorized.insert(peer.agent_id.clone(), peer).is_some()
            {
                return Err(invalid("BGR peer identity, role, team or epoch mismatch"));
            }
        }
        self.peers = authorized;
        Ok(())
    }

    fn validate_message(&self, sender: &BgrSender, envelope: &BgrEnvelope) -> Result<bool> {
        let Some(target) = &envelope.recipient_identity else {
            return self.validate_query(sender, envelope);
        };
        validate_envelope(envelope)?;
        validate_identity(&envelope.identity)?;
        if target != &self.identity
            || BgrSender::from(&envelope.identity) != *sender
            || envelope.sender_agent_id != sender.agent_id
            || envelope.recipient_agent_id != target.agent_id
            || self.peers.get(&sender.agent_id) != Some(&envelope.identity)
            || (envelope.kind == BgrMessageKind::Cancel && sender.role != BgrRole::Brain)
        {
            return Err(invalid(
                "BGR source, recipient, role or assignment fence mismatch",
            ));
        }
        if self.cancelled && envelope.kind != BgrMessageKind::Cancel {
            return Err(invalid("BGR assignment is cancelled"));
        }
        if let Some(previous) = self.queries.get(&envelope.message_id) {
            return if previous == envelope {
                Ok(false)
            } else {
                Err(invalid("BGR message ID reused with different content"))
            };
        }
        self.check_identity(target)?;
        self.check_message_id(&envelope.message_id)?;
        if self.queries.len() >= MAX_MESSAGES {
            return Err(invalid("BGR incoming message limit reached"));
        }
        self.validate_correlation(envelope, &self.outgoing, true)?;
        Ok(true)
    }

    fn check_message_id(&self, id: &str) -> Result<()> {
        if self.queries.contains_key(id)
            || self.outgoing.contains_key(id)
            || self.replies.values().any(|reply| reply.message_id == id)
        {
            return Err(invalid("BGR message ID is already in use"));
        }
        Ok(())
    }

    fn validate_correlation(
        &self,
        envelope: &BgrEnvelope,
        prior: &BTreeMap<String, BgrEnvelope>,
        incoming: bool,
    ) -> Result<()> {
        if let Some(id) = &envelope.in_reply_to {
            let request = prior
                .get(id)
                .ok_or_else(|| invalid("unknown BGR inReplyTo for this attempt"))?;
            let peer = if incoming {
                &request.recipient_agent_id
            } else {
                &request.sender_agent_id
            };
            let actual = if incoming {
                &envelope.sender_agent_id
            } else {
                &envelope.recipient_agent_id
            };
            if peer != actual
                || request.correlation_id != envelope.correlation_id
                || request.head_sha != envelope.head_sha
            {
                return Err(invalid(
                    "BGR reply peer, correlation or head anchor mismatch",
                ));
            }
            if (envelope.kind == BgrMessageKind::Answer && request.kind != BgrMessageKind::Question)
                || (envelope.kind == BgrMessageKind::StatusReply
                    && request.kind != BgrMessageKind::StatusQuery)
            {
                return Err(invalid("BGR reply kind does not match the request"));
            }
        } else if !matches!(
            envelope.kind,
            BgrMessageKind::Task
                | BgrMessageKind::Question
                | BgrMessageKind::StatusQuery
                | BgrMessageKind::Cancel
        ) {
            return Err(invalid("BGR answer/progress/result requires inReplyTo"));
        }
        Ok(())
    }

    fn emit(&mut self, envelope: BgrEnvelope) -> Result<(BgrEnvelope, bool)> {
        self.check_identity(&self.identity)?;
        validate_envelope(&envelope)?;
        if envelope.identity != self.identity
            || envelope.sender_agent_id != self.identity.agent_id
            || envelope.recipient_identity.as_ref() != self.peers.get(&envelope.recipient_agent_id)
            || envelope.recipient_identity.is_none()
            || (envelope.kind == BgrMessageKind::Cancel && self.identity.role != BgrRole::Brain)
        {
            return Err(invalid("BGR outgoing peer or source identity mismatch"));
        }
        if let Some(previous) = self.outgoing.get(&envelope.message_id) {
            return if previous == &envelope {
                Ok((envelope, false))
            } else {
                Err(invalid(
                    "BGR outgoing message ID reused with different content",
                ))
            };
        }
        self.check_message_id(&envelope.message_id)?;
        if self.outgoing.len() >= MAX_MESSAGES {
            return Err(invalid("BGR outgoing message limit reached"));
        }
        self.validate_correlation(&envelope, &self.queries, false)?;
        self.outgoing
            .insert(envelope.message_id.clone(), envelope.clone());
        self.sequence += 1;
        Ok((envelope, true))
    }

    fn send_message(&mut self, args: BgrSendMessageArgs) -> Result<(BgrEnvelope, bool)> {
        let recipient = self
            .peers
            .get(&args.recipient_agent_id)
            .ok_or_else(|| invalid("BGR recipient is not host-authorized"))?
            .clone();
        self.emit(BgrEnvelope {
            version: BGR_VERSION,
            message_id: args.message_id,
            sender_agent_id: self.identity.agent_id.clone(),
            recipient_agent_id: recipient.agent_id.clone(),
            identity: self.identity.clone(),
            recipient_identity: Some(recipient),
            kind: args.kind,
            correlation_id: args.correlation_id,
            in_reply_to: args.in_reply_to,
            payload: args.payload,
            head_sha: args.head_sha,
        })
    }

    fn reply_message(&mut self, args: BgrReplyMessageArgs) -> Result<(BgrEnvelope, bool)> {
        let incoming = self
            .queries
            .get(&args.in_reply_to)
            .ok_or_else(|| invalid("unknown BGR incoming message"))?;
        self.send_message(BgrSendMessageArgs {
            message_id: args.message_id,
            recipient_agent_id: incoming.sender_agent_id.clone(),
            kind: args.kind,
            correlation_id: incoming.correlation_id.clone(),
            in_reply_to: Some(args.in_reply_to),
            payload: args.payload,
            head_sha: incoming.head_sha.clone(),
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

    fn validate_query(&self, sender: &BgrSender, envelope: &BgrEnvelope) -> Result<bool> {
        self.check_identity(&envelope.identity)?;
        if [&sender.team_id, &sender.agent_id, &sender.session_id]
            .into_iter()
            .any(|id| !valid_id(id))
            || sender.epoch == 0
        {
            return Err(invalid("invalid BGR sender identity or zero epoch"));
        }
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
            validate_envelope(envelope)?;
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
            || self.outgoing.contains_key(&envelope.message_id)
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
        if query.kind != BgrMessageKind::StatusQuery {
            return Err(invalid("status reports require a status_query"));
        }
        if query.payload["scope"] != "now"
            && !report
                .payload
                .coverage_limits
                .iter()
                .any(|limit| !limit.trim().is_empty())
        {
            return Err(invalid(
                "scoped self-summary requires explicit missing-evidence limits",
            ));
        }
        let envelope = BgrEnvelope {
            version: BGR_VERSION,
            message_id: report.message_id,
            sender_agent_id: self.identity.agent_id.clone(),
            recipient_agent_id: query.sender_agent_id.clone(),
            identity: self.identity.clone(),
            recipient_identity: query
                .recipient_identity
                .as_ref()
                .map(|_| query.identity.clone()),
            kind: BgrMessageKind::StatusReply,
            correlation_id: query.correlation_id.clone(),
            in_reply_to: Some(query.message_id.clone()),
            head_sha: query.head_sha.clone(),
            payload: serde_json::to_value(report.payload).map_err(|e| invalid(e.to_string()))?,
        };
        if serde_json::to_vec(&envelope)
            .map_err(|e| invalid(e.to_string()))?
            .len()
            > MAX_BYTES
        {
            return Err(invalid("oversized BGR status reply envelope"));
        }
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
            || self.outgoing.contains_key(&envelope.message_id)
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
                .filter(|(id, _)| {
                    !self.replies.contains_key(*id)
                        && !self
                            .outgoing
                            .values()
                            .any(|reply| reply.in_reply_to.as_ref() == Some(*id))
                })
                .map(|(_, query)| query.clone())
                .collect(),
            replies: self
                .replies
                .values()
                .chain(self.outgoing.values())
                .cloned()
                .collect(),
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
        self.session
            .services
            .thread_extension_data
            .insert(BgrBinding);
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

    pub async fn set_bgr_peers(
        &self,
        identity: &BgrIdentity,
        peers: Vec<BgrIdentity>,
    ) -> Result<BgrSnapshot> {
        if !self.session.has_bgr_message_tools().await {
            return Err(invalid(
                "general BGR requires advertised sendTool and replyTool at thread/start",
            ));
        }
        let mut binding = self.bgr.lock().await;
        let runtime = binding
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?;
        runtime.set_peers(identity, peers)?;
        Ok(runtime.snapshot())
    }

    pub async fn send_bgr_message(&self, args: BgrSendMessageArgs) -> Result<(BgrEnvelope, bool)> {
        self.bgr
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?
            .send_message(args)
    }

    pub async fn reply_bgr_message(
        &self,
        args: BgrReplyMessageArgs,
    ) -> Result<(BgrEnvelope, bool)> {
        self.bgr
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?
            .reply_message(args)
    }

    pub async fn record_bgr_message(&self, envelope: BgrEnvelope) -> Result<(BgrEnvelope, bool)> {
        self.bgr
            .lock()
            .await
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?
            .emit(envelope)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "queue submission and query registration must be atomic with replies and cancellation; never waits for worker progress"
    )]
    pub async fn deliver_bgr(&self, sender: &BgrSender, envelope: BgrEnvelope) -> Result<bool> {
        let mut binding = self.bgr.lock().await;
        let runtime = binding
            .as_mut()
            .ok_or_else(|| invalid("thread has no live BGR binding"))?;
        if !runtime.validate_message(sender, &envelope)? {
            return Ok(false);
        }
        let content = serde_json::to_string(&envelope).map_err(|e| invalid(e.to_string()))?;
        if content.len() > MAX_BYTES {
            return Err(invalid("BGR envelope is too large"));
        }
        if envelope.kind == BgrMessageKind::Cancel {
            runtime.mail_cancellation.cancel();
            runtime.cancelled = true;
            self.submit(Op::Interrupt).await?;
            runtime
                .queries
                .insert(envelope.message_id.clone(), envelope);
            runtime.sequence += 1;
            return Ok(true);
        }
        let communication = InterAgentCommunication::new(
            AgentPath::root(),
            AgentPath::root(),
            Vec::new(),
            content,
            true,
        );
        self.io
            .submit_cancellable_mail(communication, runtime.mail_cancellation.clone())
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
            runtime.mail_cancellation.cancel();
            runtime.cancelled = true;
            runtime.sequence += 1;
            self.submit(Op::Interrupt).await?;
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
            task_id: (role != BgrRole::Brain).then(|| "task".into()),
            attempt_id: (role != BgrRole::Brain).then(|| "attempt".into()),
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
            recipient_identity: None,
            kind: BgrMessageKind::StatusQuery,
            correlation_id: format!("corr-{id}"),
            in_reply_to: None,
            payload: json!({"scope": "now"}),
            head_sha: None,
        }
    }

    fn sender() -> BgrSender {
        BgrSender::from(&identity(BgrRole::Brain))
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
        let sender = sender();
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
    fn bgr_general_message_identity_replay_correlation_and_size_fences() {
        let brain_id = identity(BgrRole::Brain);
        let worker_id = identity(BgrRole::Grunt);
        assert!(brain_id.task_id.is_none() && brain_id.attempt_id.is_none());
        let mut brain = BgrRuntime::new(brain_id.clone()).unwrap();
        let mut worker = BgrRuntime::new(worker_id.clone()).unwrap();
        brain.set_peers(&brain_id, vec![worker_id.clone()]).unwrap();
        worker
            .set_peers(&worker_id, vec![brain_id.clone()])
            .unwrap();
        let args = BgrSendMessageArgs {
            message_id: "task".into(),
            recipient_agent_id: worker_id.agent_id.clone(),
            kind: BgrMessageKind::Task,
            correlation_id: "correlation".into(),
            in_reply_to: None,
            payload: json!({"text": "work"}),
            head_sha: Some("a".repeat(40)),
        };
        let (task, fresh) = brain.send_message(args.clone()).unwrap();
        assert!(fresh);
        assert!(!brain.send_message(args.clone()).unwrap().1);
        assert!(worker.validate_message(&sender(), &task).unwrap());
        worker.queries.insert(task.message_id.clone(), task.clone());
        assert!(!worker.validate_message(&sender(), &task).unwrap());
        let (question, _) = worker
            .reply_message(BgrReplyMessageArgs {
                message_id: "question".into(),
                in_reply_to: "task".into(),
                kind: BgrMessageKind::Question,
                payload: json!({"text": "clarify"}),
            })
            .unwrap();
        assert!(
            brain
                .validate_message(&BgrSender::from(&worker_id), &question)
                .unwrap()
        );
        brain
            .queries
            .insert(question.message_id.clone(), question.clone());
        let (answer, _) = brain
            .reply_message(BgrReplyMessageArgs {
                message_id: "answer".into(),
                in_reply_to: "question".into(),
                kind: BgrMessageKind::Answer,
                payload: json!({"text": "clarification"}),
            })
            .unwrap();
        assert!(worker.validate_message(&sender(), &answer).unwrap());
        for field in [
            "correlation",
            "anchor",
            "reply",
            "source",
            "target",
            "version",
            "size",
            "kind",
        ] {
            let mut invalid = answer.clone();
            match field {
                "correlation" => invalid.correlation_id = "wrong".into(),
                "anchor" => invalid.head_sha = Some("b".repeat(40)),
                "reply" => invalid.in_reply_to = Some("unknown".into()),
                "source" => invalid.identity.session_id = "spoofed".into(),
                "target" => {
                    invalid.recipient_identity.as_mut().unwrap().attempt_id = Some("stale".into())
                }
                "version" => invalid.version = 2,
                "size" => invalid.payload = json!({"text": "界".repeat(MAX_BYTES)}),
                _ => invalid.kind = BgrMessageKind::StatusReply,
            }
            assert!(
                worker.validate_message(&sender(), &invalid).is_err(),
                "{field}"
            );
        }
        let mut conflict = args;
        conflict.payload = json!({"text": "changed"});
        assert!(brain.send_message(conflict).is_err());
        assert!(
            worker
                .set_peers(&worker_id, vec![worker_id.clone()])
                .is_err()
        );
        worker.set_peers(&worker_id, vec![]).unwrap();
        assert!(worker.validate_message(&sender(), &answer).is_err());
        worker.set_peers(&worker_id, vec![brain_id]).unwrap();
        worker.cancelled = true;
        assert!(worker.validate_message(&sender(), &answer).is_err());
        assert!(
            worker
                .reply_message(BgrReplyMessageArgs {
                    message_id: "late".into(),
                    in_reply_to: "task".into(),
                    kind: BgrMessageKind::Result,
                    payload: json!({"text": "late result"})
                })
                .is_err()
        );
    }

    #[test]
    fn bgr_scoped_status_is_explicitly_incomplete_self_summary() {
        for payload in [
            json!({"scope": "since_time", "since_time": "2026-11-03T00:00:00Z"}),
            json!({"scope": "since_commit", "since_commit": "a".repeat(40)}),
        ] {
            let mut worker = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
            let mut message = query("scope");
            message.payload = payload;
            assert!(worker.validate_query(&sender(), &message).unwrap());
            worker.queries.insert(message.message_id.clone(), message);
            let mut incomplete = report("scope");
            incomplete.payload.coverage_limits.clear();
            assert!(worker.report(incomplete.clone()).is_err());
            incomplete.payload.coverage_limits = vec![
                "No complete historical evidence; summary is limited to this fresh model context."
                    .into(),
            ];
            assert!(worker.report(incomplete).unwrap().1);
        }
    }

    #[test]
    fn bgr_status_reply_envelope_byte_limit_preserves_valid_retry() {
        for character in ['a', '界'] {
            for overflow in [0, 1] {
                let mut state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
                state.queries.insert("one".into(), query("one"));
                let mut status = report("one");
                let (base, _) = state.report(status.clone()).unwrap();
                let base_size = serde_json::to_vec(&base).unwrap().len();
                state.replies.clear();
                state.sequence = 0;
                let padding_bytes = MAX_BYTES - base_size + overflow;
                status.payload.current_action.push_str(
                    &character
                        .to_string()
                        .repeat(padding_bytes / character.len_utf8()),
                );
                status
                    .payload
                    .current_action
                    .push_str(&"x".repeat(padding_bytes % character.len_utf8()));
                assert!(serde_json::to_vec(&status).unwrap().len() < MAX_BYTES);
                let mut expected = base;
                expected.payload = serde_json::to_value(&status.payload).unwrap();
                let envelope_bytes = serde_json::to_vec(&expected).unwrap().len();
                assert_eq!(envelope_bytes, MAX_BYTES + overflow);
                eprintln!(
                    "BGR_REPLY_SIZE_TRACE: character={character} args={} envelope={envelope_bytes}",
                    serde_json::to_vec(&status).unwrap().len()
                );
                let before = state.snapshot();
                let result = state.report(status.clone());
                if overflow == 0 {
                    let (envelope, fresh) = result.unwrap();
                    assert!(fresh);
                    assert_eq!(serde_json::to_vec(&envelope).unwrap().len(), MAX_BYTES);
                    assert_eq!(state.report(status).unwrap(), (envelope, false));
                } else {
                    assert!(
                        result.is_err(),
                        "report args fit but final envelope exceeds MAX_BYTES"
                    );
                    assert_eq!(state.snapshot(), before);
                    assert!(state.report(report("one")).unwrap().1);
                    assert!(state.snapshot().pending.is_empty());
                }
            }
        }
    }

    #[test]
    fn bgr_replies_preserve_query_anchor_and_observed_head() {
        for anchor in [Some("anchor-a"), None] {
            let mut state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
            let mut message = query("one");
            message.head_sha = anchor.map(str::to_owned);
            state.queries.insert(message.message_id.clone(), message);
            let mut status = report("one");
            status.payload.head_sha = Some("observed-b".into());

            let (reply, fresh) = state.report(status.clone()).unwrap();
            assert!(fresh);
            assert_eq!(reply.head_sha.as_deref(), anchor);
            assert_eq!(reply.payload["headSha"], "observed-b");
            assert_eq!(reply.in_reply_to.as_deref(), Some("one"));
            assert_eq!(reply.correlation_id, "corr-one");
            assert_eq!(state.report(status).unwrap(), (reply, false));
        }
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
        let sender = sender();
        for field in ["epoch", "attempt", "session", "task", "team", "agent"] {
            let mut message = query("one");
            match field {
                "epoch" => message.identity.epoch += 1,
                "attempt" => message.identity.attempt_id = Some("stale-attempt".into()),
                "session" => message.identity.session_id.push('x'),
                "task" => message.identity.task_id = Some("stale-task".into()),
                "team" => message.identity.team_id.push('x'),
                _ => message.identity.agent_id.push('x'),
            }
            assert!(state.validate_query(&sender, &message).is_err());
        }
        for field in ["epoch", "team", "role", "agent", "session"] {
            let mut spoofed = sender.clone();
            match field {
                "epoch" => spoofed.epoch += 1,
                "team" => spoofed.team_id.push('x'),
                "role" => spoofed.role = BgrRole::Grunt,
                "session" => spoofed.session_id.clear(),
                _ => spoofed.agent_id.push('x'),
            }
            assert!(state.validate_query(&spoofed, &query("one")).is_err());
        }
    }

    #[test]
    fn bgr_cancelled_attempt_and_unsupported_scope_are_rejected() {
        let mut state = BgrRuntime::new(identity(BgrRole::Grunt)).unwrap();
        for payload in [
            json!({"scope": "since_time", "since_time": "not-a-time"}),
            json!({"scope": "since_commit", "since_commit": "abc"}),
            json!({"scope": "now", "instructions": "untyped work"}),
        ] {
            let mut message = query("one");
            message.payload = payload;
            assert!(state.validate_query(&sender(), &message).is_err());
        }
        state.cancelled = true;
        assert!(state.validate_query(&sender(), &query("one")).is_err());
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
                .validate_query(&sender(), &query("reply-one"))
                .is_err()
        );
        let mut huge = report("two");
        huge.payload.current_action = "x".repeat(MAX_BYTES);
        assert!(state.report(huge).is_err());
        for i in state.queries.len()..MAX_MESSAGES {
            let id = format!("q{i}");
            state.queries.insert(id.clone(), query(&id));
        }
        assert!(state.validate_query(&sender(), &query("extra")).is_err());
    }
}
