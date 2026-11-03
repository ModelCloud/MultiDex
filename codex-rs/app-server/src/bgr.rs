use crate::error_code::invalid_request;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use codex_app_server_protocol::BGR_VERSION;
use codex_app_server_protocol::BgrCapabilitiesReadResponse;
use codex_app_server_protocol::BgrMessageKind;
use codex_app_server_protocol::BgrMessageNotification;
use codex_app_server_protocol::BgrMessageSendParams;
use codex_app_server_protocol::BgrMessageSendResponse;
use codex_app_server_protocol::BgrReportStatusArgs;
use codex_app_server_protocol::BgrRole;
use codex_app_server_protocol::BgrThreadBindParams;
use codex_app_server_protocol::BgrThreadInspectParams;
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
            roles: vec![BgrRole::Brain, BgrRole::Grunt],
            status_scopes: vec!["now".into()],
            message_kinds: vec![BgrMessageKind::StatusQuery, BgrMessageKind::StatusReply],
            report_tool: report_tool(),
            live_reattach: true,
            durable_mailbox: false,
            restart_recovery: false,
            reviewer_sandbox: false,
            cross_host_delivery: false,
            task_dispatch: false,
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
        let message_id = params.envelope.message_id.clone();
        let delivered = self
            .thread(&params.thread_id)
            .await?
            .deliver_bgr(&sender.identity, params.envelope)
            .await
            .map_err(|e| invalid_request(e.to_string()))?;
        Ok(BgrMessageSendResponse {
            message_id,
            duplicate: !delivered,
            accepted: true,
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
    let result = match serde_json::from_value::<BgrReportStatusArgs>(params.arguments) {
        Ok(report) => thread.report_bgr_status(report).await,
        Err(error) => Err(codex_protocol::error::CodexErr::InvalidRequest(
            error.to_string(),
        )),
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
                "Status reply accepted by the process-local bridge. Continue the same assignment."
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
