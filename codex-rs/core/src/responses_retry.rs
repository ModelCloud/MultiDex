//! Shared retry and transport fallback decisions for Responses requests.
//! Content-filter guidance is recorded for sampling requests before retry decisions.
//! Server advice controls timing without extending configured retry limits.

use std::time::Duration;

use crate::client::ModelClientSession;
use crate::client::is_unsupported_persisted_item_context;
use crate::context::ContentFilterGuidance;
use crate::context::ContextualUserFragment;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_client::RetryOperation;
use codex_features::Feature;
use codex_http_client::RetryAfter;
use codex_model_provider_info::DEEPINFRA_DSV41_FLASH_AUTO_MODEL_ID;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;
use http::StatusCode;
use tokio::time::Instant;
use tracing::warn;

const INITIAL_CONNECTION_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_CONNECTION_RETRY_DELAY: Duration = Duration::from_secs(60);
const DEEPINFRA_FLEX_MAX_RETRIES: u64 = 3;
const DEEPINFRA_FLEX_RETRY_DELAY: Duration = Duration::from_secs(5);

pub(crate) enum DeepInfraAutoRetry {
    Flex { retries: u64 },
    Priority,
}

impl DeepInfraAutoRetry {
    pub(crate) fn for_request(provider: &ModelProviderInfo, model: &str) -> Option<Self> {
        (provider.is_deepinfra() && model == DEEPINFRA_DSV41_FLASH_AUTO_MODEL_ID)
            .then_some(Self::Flex { retries: 0 })
    }

    pub(crate) fn use_priority(&self) -> bool {
        matches!(self, Self::Priority)
    }

    fn next(&mut self, error: &CodexErr) -> Option<(Duration, String)> {
        let Self::Flex { retries } = self else {
            return None;
        };
        let transient = match error.details() {
            CodexErrorDetails::RetryLimit(_) | CodexErrorDetails::UnexpectedStatus(_) => {
                matches!(
                    error.http_status_code_value(),
                    Some(408 | 429 | 504 | 524 | 529)
                )
            }
            CodexErrorDetails::RequestTimeout
            | CodexErrorDetails::ConnectionFailed(_)
            | CodexErrorDetails::FlexUnavailable
            | CodexErrorDetails::RateLimitExceeded(_)
            | CodexErrorDetails::ServerOverloaded => true,
            // The Responses SSE decoder reports idle timeouts, premature EOF and body
            // transport errors as Stream(String). Do not retry arbitrary provider messages.
            CodexErrorDetails::Stream(message) => {
                matches!(
                    message.as_str(),
                    "idle timeout waiting for SSE" | "stream closed before response.completed"
                ) || message.starts_with("Transport error: ")
            }
            _ => false,
        };
        if !transient {
            return None;
        }
        if *retries == DEEPINFRA_FLEX_MAX_RETRIES {
            *self = Self::Priority;
            return Some((
                Duration::ZERO,
                "DeepInfra Auto: switching to Priority".to_string(),
            ));
        }
        *retries += 1;
        Some((
            DEEPINFRA_FLEX_RETRY_DELAY,
            format!("DeepInfra Auto: retrying Flex ({retries}/{DEEPINFRA_FLEX_MAX_RETRIES}) in 5s"),
        ))
    }

    pub(crate) async fn retry(
        &mut self,
        sess: &Session,
        turn: &TurnContext,
        error: CodexErr,
    ) -> Result<(), CodexErr> {
        let Some((delay, message)) = self.next(&error) else {
            return Err(error);
        };
        let retry_at = Instant::now() + delay;
        warn!(turn_id = %turn.sub_id, %error, "{message}");
        sess.notify_stream_error(turn, message, error).await;
        if !delay.is_zero() {
            tokio::time::sleep_until(retry_at).await;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ResponsesStreamRequest {
    Sampling,
    RemoteCompactionV2,
}

/// A new generation cannot repair a terminal provider response or settle the
/// previous attempt's bill. Preserve these outcomes across transport retries,
/// same-turn steering, and compaction's fallback to another model.
pub(crate) fn is_terminal_sampling_outcome(error: &CodexErr) -> bool {
    matches!(
        error.details(),
        CodexErrorDetails::ResponseIncomplete(_)
            | CodexErrorDetails::UsageAccountingFailed(_)
            | CodexErrorDetails::SessionBudgetExceeded
            | CodexErrorDetails::InferenceHookDenied { .. }
    )
}

pub(crate) struct ResponsesStreamRetryState {
    deepinfra_auto: Option<DeepInfraAutoRetry>,
    retries: u64,
    connection_retries: u64,
    connection_retry_delay: Duration,
    /// Capacity (`server_is_overloaded`) retries so far; they never consume
    /// `retries` because overloads are out of the caller's control.
    overloaded_retries: u64,
}

impl ResponsesStreamRetryState {
    pub(crate) fn for_sampling(provider: &ModelProviderInfo, model: &str) -> Self {
        Self {
            deepinfra_auto: DeepInfraAutoRetry::for_request(provider, model),
            ..Self::default()
        }
    }

    pub(crate) fn use_deepinfra_priority(&self) -> bool {
        self.deepinfra_auto
            .as_ref()
            .is_some_and(DeepInfraAutoRetry::use_priority)
    }
}

impl Default for ResponsesStreamRetryState {
    fn default() -> Self {
        Self {
            deepinfra_auto: None,
            retries: 0,
            connection_retries: 0,
            connection_retry_delay: INITIAL_CONNECTION_RETRY_DELAY,
            overloaded_retries: 0,
        }
    }
}

/// Server retry advice retained after stream retries are exhausted. The turn ID
/// prevents a reused Guardian session from applying advice from an earlier review.
pub(crate) struct ExhaustedResponseRetry {
    pub(crate) turn_id: String,
    pub(crate) retry_at: Option<tokio::time::Instant>,
}

/// Returns `Ok(())` when the caller should retry the request loop, or the original error when
/// it is terminal or the retry budget is exhausted.
#[tracing::instrument(level = "trace", skip_all, fields(request = ?request))]
pub(crate) async fn handle_response_stream_error(
    retry_state: &mut ResponsesStreamRetryState,
    max_retries: u64,
    err: CodexErr,
    client_session: &mut ModelClientSession,
    sess: &Session,
    step_context: &StepContext,
    request: ResponsesStreamRequest,
) -> Result<(), CodexErr> {
    let turn_context = &step_context.turn;
    if matches!(request, ResponsesStreamRequest::Sampling)
        && matches!(err.details(), CodexErrorDetails::ContentFilter)
    {
        let model_info = &step_context.settings.model_info;
        let guidance = ContentFilterGuidance {
            text: codex_prompts::ResolvedModelMessages::from_model(model_info)
                .content_filter_guidance()
                .to_string(),
        };
        sess.record_conversation_items(
            turn_context,
            model_info,
            &[ContextualUserFragment::into(guidance)],
        )
        .await;
    }
    let unsupported_persisted_context = match err.details() {
        CodexErrorDetails::InvalidRequest(body) => {
            is_unsupported_persisted_item_context(StatusCode::BAD_REQUEST, Some(body))
        }
        CodexErrorDetails::UnexpectedStatus(response) => {
            is_unsupported_persisted_item_context(response.status, Some(&response.body))
        }
        _ => false,
    };
    if matches!(request, ResponsesStreamRequest::Sampling)
        && unsupported_persisted_context
        && client_session.disable_response_continuation()
    {
        warn!("Responses provider cannot replay stored context; retrying with full history");
        return Ok(());
    }
    if let Some(auto) = &mut retry_state.deepinfra_auto {
        return auto.retry(sess, turn_context, err).await;
    }
    let operation = match request {
        ResponsesStreamRequest::Sampling => RetryOperation::Sampling,
        ResponsesStreamRequest::RemoteCompactionV2 => RetryOperation::RemoteCompactionV2,
    };
    let retry_after = err.retry_after();

    // A capacity rejection is transient by definition: the provider is still
    // up, the model is just saturated. Retry it forever at a fixed delay
    // without consuming `stream_max_retries` so capacity bursts can never
    // fail the turn.
    if matches!(err.details(), CodexErrorDetails::ServerOverloaded) {
        retry_state.overloaded_retries = retry_state.overloaded_retries.saturating_add(1);
        let attempt = retry_state.overloaded_retries;
        // Honor server retry advice over the configured delay.
        let delay = turn_context.provider.info().server_overloaded_retry_delay();
        let now = Instant::now();
        let retry_at = retry_after.map(RetryAfter::deadline).unwrap_or(now + delay);
        let sleep_for = retry_at.saturating_duration_since(now);
        warn!(
            turn_id = %turn_context.sub_id,
            attempt,
            error = %err,
            ?sleep_for,
            "model at capacity; waiting to retry"
        );
        sess.notify_stream_error(
            turn_context,
            format!("{err} Retrying in {:.1}s...", sleep_for.as_secs_f64()),
            err,
        )
        .await;
        codex_client::record_retry!(attempt, sleep_for, operation);
        tokio::time::sleep_until(retry_at).await;
        return Ok(());
    }

    let retry_count = retry_state.retries.saturating_add(1);
    let Some(delay) = err.retry_delay(retry_count) else {
        return Err(err);
    };

    if turn_context
        .config
        .features
        .enabled(Feature::UnboundedConnectionRetries)
        && matches!(request, ResponsesStreamRequest::Sampling)
        && matches!(err.details(), CodexErrorDetails::ConnectionFailed(_))
        && !turn_context.session_source.is_internal()
        && !turn_context.provider.info().is_amazon_bedrock()
    {
        let retry_delay = retry_state.connection_retry_delay;
        warn!(
            turn_id = %turn_context.sub_id,
            error = %err,
            ?retry_delay,
            "stream connection failed; waiting to retry"
        );
        sess.notify_stream_error(turn_context, "Reconnecting... waiting for network", err)
            .await;
        retry_state.connection_retries = retry_state.connection_retries.saturating_add(1);
        codex_client::record_retry!(retry_state.connection_retries, retry_delay, operation);
        tokio::time::sleep(retry_delay).await;
        retry_state.connection_retry_delay = retry_delay
            .saturating_mul(2)
            .min(MAX_CONNECTION_RETRY_DELAY);
        return Ok(());
    }

    let requires_http_fallback = err.requires_http_fallback();
    let switched = (requires_http_fallback || retry_state.retries >= max_retries)
        && client_session.try_switch_fallback_transport(
            &turn_context.session_telemetry,
            turn_context.model_info(),
        );
    if switched {
        // Changing transport must not bypass the server's retry deadline.
        if let Some(retry_after) = retry_after {
            tokio::time::sleep_until(retry_after.deadline()).await;
        }
        sess.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: format!("Falling back from WebSockets to HTTPS transport. {err:#}"),
            }),
        )
        .await;
    }
    // Another request can already have switched this session to HTTP.
    if switched || requires_http_fallback {
        retry_state.retries = 0;
        return Ok(());
    }

    if retry_state.retries < max_retries {
        retry_state.retries = retry_count;
        log_retry(request, turn_context, &err, retry_count, max_retries, delay);

        // In release builds, hide the first websocket retry notification to reduce noisy
        // transient reconnect messages. In debug builds, keep full visibility for diagnosis.
        let report_error = retry_count > 1
            || cfg!(debug_assertions)
            || !sess.services.model_client.responses_websocket_enabled();
        if report_error {
            // Surface retry information to any UI/front-end so the user understands what is
            // happening instead of staring at a seemingly frozen screen.
            sess.notify_stream_error(
                turn_context,
                format!("Reconnecting... {retry_count}/{max_retries}"),
                err,
            )
            .await;
        }
        // Use one clock sample so local backoff telemetry retains the selected delay.
        let now = Instant::now();
        let retry_at = retry_after.map(RetryAfter::deadline).unwrap_or(now + delay);
        let delay = retry_at.saturating_duration_since(now);
        codex_client::record_retry!(retry_count, delay, operation);
        tokio::time::sleep_until(retry_at).await;
        return Ok(());
    }

    sess.services
        .thread_extension_data
        .insert(ExhaustedResponseRetry {
            turn_id: turn_context.sub_id.clone(),
            retry_at: retry_after.map(RetryAfter::deadline),
        });
    Err(err)
}

fn log_retry(
    request: ResponsesStreamRequest,
    turn_context: &TurnContext,
    err: &CodexErr,
    retries: u64,
    max_retries: u64,
    delay: Duration,
) {
    match request {
        ResponsesStreamRequest::Sampling => {
            warn!(
                turn_id = %turn_context.sub_id,
                retries,
                max_retries,
                sampling_error = %err,
                "stream disconnected - retrying sampling request ({retries}/{max_retries} in {delay:?})...",
            );
        }
        ResponsesStreamRequest::RemoteCompactionV2 => {
            warn!(
                turn_id = %turn_context.sub_id,
                retries,
                max_retries,
                compact_error = %err,
                "remote compaction v2 stream failed; retrying request after delay"
            );
        }
    }
}

#[cfg(test)]
#[path = "responses_retry_tests.rs"]
mod tests;
