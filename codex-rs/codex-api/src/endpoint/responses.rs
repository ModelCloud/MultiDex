use crate::auth::SharedAuthProvider;
use crate::common::CompletedToolsEcho;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::common::with_tool_results;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::Compression;
use crate::requests::headers::build_session_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::sse::responses::spawn_response_stream_with_tools_echo;
use crate::telemetry::SseTelemetry;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestCompression;
use codex_client::RequestTelemetry;
use codex_client::TransportError;
use codex_protocol::models::ToolResultBody;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use serde_json::Value;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::instrument;
use url::Url;

pub struct ResponsesClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
    chat_completions: bool,
}

#[derive(Default)]
pub struct ResponsesOptions {
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_source: Option<SessionSource>,
    pub extra_headers: HeaderMap,
    pub compression: Compression,
    pub tool_results: Vec<(usize, usize, Arc<ToolResultBody>)>,
    pub turn_state: Option<Arc<OnceLock<String>>>,
    /// Receives a digest of the `tools` echoed by `response.completed`.
    pub completed_tools: Option<CompletedToolsEcho>,
}

impl<T: HttpTransport> ResponsesClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
            chat_completions: false,
        }
    }

    /// Opt into the Lithos wire adapter without changing logical Responses history.
    pub fn with_chat_completions(mut self, enabled: bool) -> Self {
        self.chat_completions = enabled;
        self
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
            chat_completions: self.chat_completions,
        }
    }

    /// Checks whether a stored response is still available for continuation.
    /// Only the status is needed, so leave the potentially large response body unread.
    pub async fn retrieve_response(
        &self,
        response_id: &str,
        extra_headers: HeaderMap,
    ) -> Result<(), ApiError> {
        let encoded_id: String =
            url::form_urlencoded::byte_serialize(response_id.as_bytes()).collect();
        self.session
            .stream_encoded_json_with(
                Method::GET,
                &format!("/responses/{encoded_id}"),
                extra_headers,
                /*body*/ None,
                |_| {},
            )
            .await?;
        Ok(())
    }

    /// Checks the health route alongside a provider's `/v1` API root.
    /// A successful check only establishes reachability, not response-ID validity.
    pub async fn check_health(&self, extra_headers: HeaderMap) -> Result<(), ApiError> {
        let mut url = Url::parse(&self.session.provider().base_url).map_err(|_| {
            ApiError::InvalidRequest {
                message: "invalid Responses provider URL".to_string(),
            }
        })?;
        let base_path = url.path().trim_end_matches('/');
        let root_path = base_path.strip_suffix("/v1").unwrap_or(base_path);
        url.set_path(&format!("{root_path}/health"));
        url.set_query(None);
        let health_url = url.to_string();
        self.session
            .execute_with(
                Method::GET,
                "/health",
                extra_headers,
                /*body*/ None,
                |request| {
                    request.url.clone_from(&health_url);
                    request.timeout = Some(Duration::from_secs(2));
                    request.response_body_limit_bytes = Some(4096);
                },
            )
            .await?;
        Ok(())
    }

    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        self.stream_request_with_observer(request, options, |_| {})
            .await
    }

    /// Observes the encoded request before private connector results are attached.
    #[instrument(
        name = "responses.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "responses_http",
            http.method = "POST",
            api.path = "/responses"
        )
    )]
    pub async fn stream_request_with_observer(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
        on_encoded: impl FnOnce(&bytes::Bytes),
    ) -> Result<ResponseStream, ApiError> {
        let ResponsesOptions {
            session_id,
            thread_id,
            session_source,
            extra_headers,
            compression,
            tool_results,
            turn_state,
            completed_tools,
        } = options;
        let body = EncodedJsonBody::encode(&request)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;

        on_encoded(body.shared_bytes());
        let mut headers = extra_headers;
        if let Some(ref thread_id) = thread_id {
            insert_header(&mut headers, "x-client-request-id", thread_id);
        }
        headers.extend(build_session_headers(session_id, thread_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        if self.chat_completions {
            return crate::endpoint::chat_completions::stream(&self.session, &request, headers)
                .await;
        }
        if !tool_results.is_empty()
            && let Ok(json) = std::str::from_utf8(body.as_bytes())
            && let Some(json) = with_tool_results(json, &tool_results)
        {
            let stream = self
                .stream_encoded(
                    body.with_private_json(json),
                    headers.clone(),
                    compression,
                    turn_state.clone(),
                    completed_tools.clone(),
                )
                .await;
            // A redirect that preserves the request body must start over without private results.
            if !matches!(
                stream,
                Err(ApiError::Transport(TransportError::Http {
                    status: StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT,
                    headers: Some(ref headers),
                    ..
                })) if headers.get(http::header::LOCATION).is_some_and(|location| !location.is_empty())
            ) {
                return stream;
            }
        }
        self.stream_encoded(body, headers, compression, turn_state, completed_tools)
            .await
    }

    #[instrument(
        name = "responses.stream",
        level = "info",
        skip_all,
        fields(
            transport = "responses_http",
            http.method = "POST",
            api.path = "/responses",
            turn.has_state = turn_state.is_some()
        )
    )]
    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let body = EncodedJsonBody::encode(&body)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;
        self.stream_encoded(
            body,
            extra_headers,
            compression,
            turn_state,
            /*completed_tools*/ None,
        )
        .await
    }

    async fn stream_encoded(
        &self,
        body: EncodedJsonBody,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
        completed_tools: Option<CompletedToolsEcho>,
    ) -> Result<ResponseStream, ApiError> {
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        let stream_response = self
            .session
            .stream_encoded_json_with(
                Method::POST,
                "/responses",
                extra_headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    req.compression = request_compression;
                },
            )
            .await?;

        Ok(spawn_response_stream_with_tools_echo(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
            completed_tools,
        ))
    }
}
