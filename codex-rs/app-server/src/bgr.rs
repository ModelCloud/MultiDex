use crate::error_code::invalid_request;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use codex_app_server_protocol::BGR_VERSION;
use codex_app_server_protocol::BgrCapabilitiesReadResponse;
use codex_app_server_protocol::BgrMessageDeliverParams;
use codex_app_server_protocol::BgrMessageKind;
use codex_app_server_protocol::BgrMessageNotification;
use codex_app_server_protocol::BgrMessageRecordParams;
use codex_app_server_protocol::BgrMessageSendParams;
use codex_app_server_protocol::BgrMessageSendResponse;
use codex_app_server_protocol::BgrReportStatusArgs;
use codex_app_server_protocol::BgrRole;
use codex_app_server_protocol::BgrSender;
use codex_app_server_protocol::BgrThreadBindParams;
use codex_app_server_protocol::BgrThreadInspectParams;
use codex_app_server_protocol::BgrThreadPeersParams;
use codex_app_server_protocol::BgrThreadStateResponse;
use codex_app_server_protocol::DynamicToolCallParams;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::report_tool;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_protocol::ThreadId;
use std::result::Result;
use std::sync::Arc;

pub(crate) struct BgrRequestProcessor {
    thread_manager: Arc<ThreadManager>,
}

impl BgrRequestProcessor {
    pub(crate) fn new(thread_manager: Arc<ThreadManager>) -> Self {
        Self { thread_manager }
    }

    pub(crate) fn capabilities(&self) -> BgrCapabilitiesReadResponse {
        BgrCapabilitiesReadResponse {
            version: BGR_VERSION,
            roles: vec![BgrRole::Brain, BgrRole::Grunt, BgrRole::Reviewer],
            status_scopes: vec!["now".into(), "since_time".into(), "since_commit".into()],
            message_kinds: vec![
                BgrMessageKind::Task,
                BgrMessageKind::Question,
                BgrMessageKind::Answer,
                BgrMessageKind::Progress,
                BgrMessageKind::Result,
                BgrMessageKind::Cancel,
                BgrMessageKind::StatusQuery,
                BgrMessageKind::StatusReply,
            ],
            report_tool: report_tool(),
            send_tool: codex_app_server_protocol::send_tool(),
            reply_tool: codex_app_server_protocol::reply_tool(),
            general_messaging: true,
            message_callbacks: true,
            consumption_receipts: false,
            status_evidence:
                "model_self_summary_with_explicit_missing_evidence; no backend history attestation"
                    .into(),
            live_reattach: true,
            durable_mailbox: false,
            restart_recovery: false,
            reviewer_sandbox: true,
            reviewer_start: true,
            reviewer_snapshot_no_lazy_fetch: true,
            review_snapshot_tool: codex_protocol::bgr::review_snapshot_tool(),
            host_delivery: true,
            host_record: true,
            cross_host_delivery: false,
            task_dispatch: true,
            max_queries_per_attempt: 128,
        }
    }

    async fn thread(&self, id: &str) -> Result<Arc<CodexThread>, JSONRPCErrorError> {
        let id = ThreadId::from_string(id).map_err(|e| invalid_request(e.to_string()))?;
        self.thread_manager
            .get_thread(id)
            .await
            .map_err(|e| invalid_request(format!("live BGR thread unavailable: {e}")))
    }

    pub(crate) async fn bind(
        &self,
        params: BgrThreadBindParams,
    ) -> Result<BgrThreadStateResponse, JSONRPCErrorError> {
        let snapshot = self
            .thread(&params.thread_id)
            .await?
            .bind_bgr(params.identity)
            .await
            .map_err(|e| invalid_request(e.to_string()))?;
        Ok(BgrThreadStateResponse { snapshot })
    }

    pub(crate) async fn inspect(
        &self,
        params: BgrThreadInspectParams,
    ) -> Result<BgrThreadStateResponse, JSONRPCErrorError> {
        let snapshot = self
            .thread(&params.thread_id)
            .await?
            .inspect_bgr()
            .await
            .map_err(|e| invalid_request(e.to_string()))?;
        Ok(BgrThreadStateResponse { snapshot })
    }

    pub(crate) async fn peers(
        &self,
        params: BgrThreadPeersParams,
    ) -> Result<BgrThreadStateResponse, JSONRPCErrorError> {
        let snapshot = self
            .thread(&params.thread_id)
            .await?
            .set_bgr_peers(&params.identity, params.peers)
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        Ok(BgrThreadStateResponse { snapshot })
    }

    pub(crate) async fn send(
        &self,
        params: BgrMessageSendParams,
    ) -> Result<BgrMessageSendResponse, JSONRPCErrorError> {
        let sender = self
            .thread(&params.source_thread_id)
            .await?
            .inspect_bgr()
            .await
            .map_err(|e| invalid_request(e.to_string()))?;
        if sender.cancelled {
            return Err(invalid_request("BGR sender is cancelled"));
        }
        if params.envelope.recipient_identity.is_some() {
            self.thread(&params.source_thread_id)
                .await?
                .record_bgr_message(params.envelope.clone())
                .await
                .map_err(|error| invalid_request(error.to_string()))?;
        }
        self.deliver(BgrMessageDeliverParams {
            thread_id: params.thread_id,
            sender: BgrSender::from(&sender.identity),
            envelope: params.envelope,
        })
        .await
    }

    pub(crate) async fn deliver(
        &self,
        params: BgrMessageDeliverParams,
    ) -> Result<BgrMessageSendResponse, JSONRPCErrorError> {
        let message_id = params.envelope.message_id.clone();
        let delivered = self
            .thread(&params.thread_id)
            .await?
            .deliver_bgr(&params.sender, params.envelope)
            .await
            .map_err(|e| invalid_request(e.to_string()))?;
        Ok(BgrMessageSendResponse {
            message_id,
            duplicate: !delivered,
            accepted: true,
        })
    }

    pub(crate) async fn record(
        &self,
        params: BgrMessageRecordParams,
    ) -> Result<BgrMessageSendResponse, JSONRPCErrorError> {
        if params.identity != params.envelope.identity {
            return Err(invalid_request("BGR record source identity mismatch"));
        }
        let (envelope, fresh) = self
            .thread(&params.thread_id)
            .await?
            .record_bgr_message(params.envelope)
            .await
            .map_err(|error| invalid_request(error.to_string()))?;
        Ok(BgrMessageSendResponse {
            message_id: envelope.message_id,
            accepted: true,
            duplicate: !fresh,
        })
    }

    pub(crate) async fn cancel(
        &self,
        params: BgrThreadBindParams,
    ) -> Result<BgrThreadStateResponse, JSONRPCErrorError> {
        let snapshot = self
            .thread(&params.thread_id)
            .await?
            .cancel_bgr(&params.identity)
            .await
            .map_err(|e| invalid_request(e.to_string()))?;
        Ok(BgrThreadStateResponse { snapshot })
    }

    pub(crate) async fn checkpoint(
        &self,
        params: BgrThreadBindParams,
    ) -> Result<BgrThreadStateResponse, JSONRPCErrorError> {
        let thread = self.thread(&params.thread_id).await?;
        let snapshot = thread
            .inspect_bgr()
            .await
            .map_err(|e| invalid_request(e.to_string()))?;
        if snapshot.identity != params.identity {
            return Err(invalid_request("BGR checkpoint identity mismatch"));
        }
        thread
            .flush_rollout()
            .await
            .map_err(|e| invalid_request(format!("BGR native checkpoint failed: {e}")))?;
        Ok(BgrThreadStateResponse { snapshot })
    }
}

pub(crate) async fn on_status_report(
    params: DynamicToolCallParams,
    thread: Arc<CodexThread>,
    outgoing: ThreadScopedOutgoingMessageSender,
) {
    if params.tool == codex_protocol::bgr::BGR_REVIEW_SNAPSHOT_TOOL {
        let result = match serde_json::from_value(params.arguments) {
            Ok(args) => thread.read_bgr_review_snapshot(args).await,
            Err(error) => Err(codex_protocol::error::CodexErr::InvalidRequest(
                error.to_string(),
            )),
        };
        let (success, text) = match result {
            Ok(value) => (true, value.to_string()),
            Err(error) => (false, error.to_string()),
        };
        let _ = thread.submit(codex_protocol::protocol::Op::DynamicToolResponse {
            id: params.call_id,
            response: codex_protocol::dynamic_tools::DynamicToolResponse {
                success, content_items: vec![codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem::InputText { text }],
            },
        }).await;
        return;
    }
    let result = match params.tool.as_str() {
        codex_protocol::bgr::BGR_SEND_TOOL => match serde_json::from_value(params.arguments) {
            Ok(args) => thread.send_bgr_message(args).await,
            Err(error) => Err(codex_protocol::error::CodexErr::InvalidRequest(
                error.to_string(),
            )),
        },
        codex_protocol::bgr::BGR_REPLY_TOOL => match serde_json::from_value(params.arguments) {
            Ok(args) => thread.reply_bgr_message(args).await,
            Err(error) => Err(codex_protocol::error::CodexErr::InvalidRequest(
                error.to_string(),
            )),
        },
        _ => match serde_json::from_value::<BgrReportStatusArgs>(params.arguments) {
            Ok(report) => thread.report_bgr_status(report).await,
            Err(error) => Err(codex_protocol::error::CodexErr::InvalidRequest(
                error.to_string(),
            )),
        },
    };
    let (success, text) = match result {
        Ok((envelope, fresh)) => {
            if fresh {
                outgoing
                    .send_server_notification(ServerNotification::BgrMessage(
                        BgrMessageNotification {
                            thread_id: params.thread_id,
                            envelope,
                        },
                    ))
                    .await;
            }
            (
                true,
                "BGR message accepted by the process-local bridge, not acknowledged as consumed. Continue the same assignment."
                    .to_string(),
            )
        }
        Err(error) => (false, error.to_string()),
    };
    let _ = thread
        .submit(codex_protocol::protocol::Op::DynamicToolResponse {
            id: params.call_id,
            response: codex_protocol::dynamic_tools::DynamicToolResponse {
                success,
                content_items: vec![
                    codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem::InputText {
                        text,
                    },
                ],
            },
        })
        .await;
}
