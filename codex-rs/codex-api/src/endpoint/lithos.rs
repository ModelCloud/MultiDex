//! Lithos wire adapter. Upstream owns the logical Responses request and history.
//! Only the final HTTP payload and incoming Chat Completions events are translated.

use super::session::EndpointSession;
use crate::common::{ResponseEvent, ResponseStream, ResponseStreamReceiver, ResponsesApiRequest};
use crate::error::ApiError;
use codex_client::{EncodedJsonBody, HttpTransport};
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, Method};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::InvalidRequest {
        message: message.into(),
    }
}

fn text(content: &Value) -> Result<String, ApiError> {
    if let Some(s) = content.as_str() {
        return Ok(s.to_owned());
    }
    if content.is_null() {
        return Ok(String::new());
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| invalid("Lithos message content must be text"))?;
    let mut result = String::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text" | "input_text" | "output_text" | "reasoning_text" | "summary_text") => {
                result.push_str(
                    block["text"]
                        .as_str()
                        .ok_or_else(|| invalid("Lithos text block has no text"))?,
                );
            }
            _ => return Err(invalid("Lithos DeepSeek supports text content only")),
        }
    }
    Ok(result)
}

fn flush(messages: &mut Vec<Value>, assistant: &mut Option<Value>) {
    if let Some(message) = assistant.take() {
        messages.push(message);
    }
}

fn assistant(value: &mut Option<Value>) -> &mut Value {
    value.get_or_insert_with(|| json!({"role":"assistant","content":""}))
}

fn append(value: &mut Value, field: &str, delta: &str) {
    let mut current = value[field].as_str().unwrap_or_default().to_owned();
    current.push_str(delta);
    value[field] = Value::String(current);
}

/// Return the provider wire payload and identities of custom (free-text) tools.
/// Do not mutate or replace the caller's logical request/continuation baseline.
pub(crate) fn request_body(request: &Value) -> Result<(Value, HashSet<String>), ApiError> {
    if request
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return Err(invalid(
            "Lithos Chat Completions cannot extend a stored Responses ID",
        ));
    }
    let input = request["input"]
        .as_array()
        .ok_or_else(|| invalid("Lithos input must be an array"))?;
    // Input additional_tools come first so incremental tool updates resolve by
    // recency; the request's own tool list is applied last and stays
    // authoritative when a definition also survives in replayed history (e.g.
    // after switching providers mid-thread).
    let mut definitions = Vec::new();
    for item in input {
        if item["type"] == "additional_tools" {
            definitions.extend(item["tools"].as_array().cloned().unwrap_or_default());
        }
    }
    definitions.extend(request["tools"].as_array().cloned().unwrap_or_default());
    let mut custom = HashSet::new();
    let mut tools = BTreeMap::new();
    for mut tool in definitions {
        let kind = tool["type"].as_str().unwrap_or_default();
        let name = tool["name"]
            .as_str()
            .ok_or_else(|| invalid("Lithos tool has no name"))?
            .to_owned();
        match kind {
            "function" => {
                custom.remove(&name);
                let mut function = json!({"name":name,"parameters":tool["parameters"]});
                for key in ["description", "strict"] {
                    if let Some(value) = tool.get(key) {
                        function[key] = value.clone();
                    }
                }
                tool = function;
            }
            "custom" => {
                custom.insert(name.clone());
                let mut description = tool["description"].as_str().unwrap_or_default().to_owned();
                if let Some(format) = tool.get("format").filter(|v| !v.is_null()) {
                    description.push_str("\nInput format: ");
                    description.push_str(&format.to_string());
                }
                tool = json!({"name":name,"description":description,"parameters":{"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false}});
            }
            _ => {
                return Err(invalid(format!(
                    "Lithos cannot translate tool type {kind:?}"
                )));
            }
        }
        let tool = json!({"type":"function","function":tool});
        // The last definition wins: tool updates and provider switches
        // legitimately redeclare a name with a newer shape.
        tools.insert(name, tool);
    }
    let mut messages = Vec::new();
    if let Some(instructions) = request["instructions"].as_str().filter(|s| !s.is_empty()) {
        messages.push(json!({"role":"system","content":instructions}));
    }
    let mut pending = None;
    for item in input {
        match item["type"].as_str().unwrap_or("message") {
            "additional_tools" => {}
            "compaction" | "context_compaction" => {
                // A compaction item belongs to the provider that minted it. Only
                // the readable form can cross providers; an opaque encrypted
                // token (no whitespace) is undecodable here. Dropping the item
                // keeps the rest of the conversation usable, whereas failing the
                // request would make a mid-session provider switch impossible.
                let Some(summary) = item["encrypted_content"]
                    .as_str()
                    .filter(|s| s.chars().any(char::is_whitespace))
                else {
                    tracing::warn!(
                        "Lithos dropped a compaction summary it cannot decode; \
                         the conversation continues without that summary"
                    );
                    continue;
                };
                flush(&mut messages, &mut pending);
                messages.push(json!({"role":"user","content":summary}));
            }
            "reasoning" => {
                let content = item
                    .get("content")
                    .filter(|v| !v.is_null())
                    .unwrap_or(&item["summary"]);
                let reasoning = text(content)?;
                if !reasoning.is_empty() {
                    append(assistant(&mut pending), "reasoning_content", &reasoning);
                } else if item.get("encrypted_content").is_some_and(|v| !v.is_null()) {
                    // Same rule as compaction: an encrypted reasoning payload
                    // has no plaintext for Lithos to consume. Reasoning is not
                    // required for correctness, so skip it instead of failing.
                    tracing::debug!("Lithos dropped an encrypted reasoning item it cannot decode");
                }
            }
            "message" => {
                let role = item["role"]
                    .as_str()
                    .ok_or_else(|| invalid("Lithos message has no role"))?;
                let content = text(&item["content"])?;
                if role == "assistant" {
                    append(assistant(&mut pending), "content", &content);
                } else {
                    flush(&mut messages, &mut pending);
                    messages.push(json!({"role":if role=="developer" {"system"} else {role},"content":content}));
                }
            }
            "function_call" | "custom_tool_call" => {
                let is_custom = item["type"] == "custom_tool_call";
                let arguments = if is_custom {
                    json!({"input":item["input"]}).to_string()
                } else {
                    item["arguments"]
                        .as_str()
                        .ok_or_else(|| invalid("Function arguments must be a JSON string"))?
                        .to_owned()
                };
                let message = assistant(&mut pending);
                if message.get("tool_calls").is_none() {
                    message["tool_calls"] = json!([]);
                }
                message["tool_calls"].as_array_mut().ok_or_else(|| invalid("Invalid assistant tools"))?.push(json!({"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":arguments}}));
            }
            "function_call_output" | "custom_tool_call_output" => {
                flush(&mut messages, &mut pending);
                messages.push(json!({"role":"tool","tool_call_id":item["call_id"],"content":text(&item["output"])?}));
            }
            kind => {
                return Err(invalid(format!(
                    "Lithos cannot translate input item {kind:?}"
                )));
            }
        }
    }
    flush(&mut messages, &mut pending);
    let mut body = json!({"model":request["model"],"messages":messages,"stream":true,"stream_options":{"include_usage":true}});
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.into_values().collect());
    }
    if body.get("tools").is_some() {
        if let Some(choice) = request.get("tool_choice") {
            body["tool_choice"] = choice.clone();
        }
        if let Some(parallel) = request.get("parallel_tool_calls") {
            body["parallel_tool_calls"] = parallel.clone();
        }
    }
    if let Some(effort) = request["reasoning"]["effort"].as_str() {
        body["reasoning_effort"] = json!(if effort == "off" { "none" } else { effort });
    }
    if let Some(max) = request.get("max_output_tokens") {
        body["max_tokens"] = max.clone();
    }
    if let Some(format) = request["text"].get("format") {
        body["response_format"] = if format["type"] == "json_schema" {
            let mut schema = format.clone();
            schema
                .as_object_mut()
                .ok_or_else(|| invalid("Invalid response format"))?
                .remove("type");
            json!({"type":"json_schema","json_schema":schema})
        } else {
            format.clone()
        };
    }
    Ok((body, custom))
}

#[derive(Default)]
struct ChatState {
    id: String,
    content: String,
    reasoning: String,
    calls: BTreeMap<u64, Value>,
    usage: Option<TokenUsage>,
    finish: Option<String>,
    created: bool,
}

fn item(value: Value) -> Result<ResponseItem, ApiError> {
    serde_json::from_value(value)
        .map_err(|e| ApiError::Stream(format!("Invalid Lithos output: {e}")))
}

impl ChatState {
    fn chunk(&mut self, chunk: Value) -> Result<Vec<ResponseEvent>, ApiError> {
        if let Some(error) = chunk.get("error").filter(|value| !value.is_null()) {
            return Err(ApiError::Stream(format!("Lithos stream error: {error}")));
        }
        let mut events = Vec::new();
        if let Some(id) = chunk["id"].as_str() {
            if !self.id.is_empty() && self.id != id {
                return Err(ApiError::Stream(
                    "Lithos changed response id within a stream".into(),
                ));
            }
            self.id = id.to_owned();
        }
        if !self.created {
            if self.id.is_empty() {
                return Err(ApiError::Stream("Lithos stream has no response id".into()));
            }
            events.push(ResponseEvent::Created {
                response_id: Some(self.id.clone()),
            });
            self.created = true;
        }
        if let Some(usage) = chunk.get("usage").filter(|v| !v.is_null()) {
            let input = usage["prompt_tokens"].as_i64().unwrap_or_default();
            let output = usage["completion_tokens"].as_i64().unwrap_or_default();
            self.usage = Some(TokenUsage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: usage["total_tokens"].as_i64().unwrap_or(input + output),
                cached_input_tokens: usage["prompt_tokens_details"]["cached_tokens"]
                    .as_i64()
                    .unwrap_or_default(),
                reasoning_output_tokens: usage["completion_tokens_details"]["reasoning_tokens"]
                    .as_i64()
                    .unwrap_or_default(),
                ..Default::default()
            });
        }
        for choice in chunk["choices"].as_array().into_iter().flatten() {
            if choice["index"].as_u64().unwrap_or_default() != 0 {
                return Err(ApiError::Stream(
                    "Lithos returned more than one choice".into(),
                ));
            }
            if let Some(finish) = choice["finish_reason"].as_str() {
                self.finish = Some(finish.to_owned());
            }
            let delta = &choice["delta"];
            if let Some(s) = delta["reasoning_content"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                if self.reasoning.is_empty() {
                    events.push(ResponseEvent::OutputItemAdded(item(json!({"type":"reasoning","id":format!("{}_reasoning",self.id),"summary":[],"content":[],"encrypted_content":null}))?));
                }
                self.reasoning.push_str(s);
                events.push(ResponseEvent::ReasoningContentDelta {
                    delta: s.to_owned(),
                    content_index: 0,
                });
            }
            if let Some(s) = delta["content"].as_str().filter(|s| !s.is_empty()) {
                if self.content.is_empty() {
                    events.push(ResponseEvent::OutputItemAdded(item(json!({"type":"message","id":format!("{}_message",self.id),"role":"assistant","content":[]}))?));
                }
                self.content.push_str(s);
                events.push(ResponseEvent::OutputTextDelta(s.to_owned()));
            }
            for call in delta["tool_calls"].as_array().into_iter().flatten() {
                let index = call["index"]
                    .as_u64()
                    .ok_or_else(|| ApiError::Stream("Lithos tool delta has no index".into()))?;
                let entry = self
                    .calls
                    .entry(index)
                    .or_insert_with(|| json!({"id":"","name":"","arguments":""}));
                if let Some(s) = call["id"].as_str() {
                    if !entry["id"].as_str().unwrap_or_default().is_empty() && entry["id"] != s {
                        return Err(ApiError::Stream("Lithos changed a tool call id".into()));
                    }
                    entry["id"] = json!(s);
                }
                if let Some(s) = call["function"]["name"].as_str() {
                    append(entry, "name", s);
                }
                if let Some(s) = call["function"]["arguments"].as_str() {
                    append(entry, "arguments", s);
                }
            }
        }
        Ok(events)
    }

    fn complete(self, custom: &HashSet<String>) -> Result<Vec<ResponseEvent>, ApiError> {
        if !matches!(self.finish.as_deref(), Some("stop" | "tool_calls")) {
            return Err(ApiError::Stream(format!(
                "Lithos generation did not complete: {:?}",
                self.finish
            )));
        }
        let mut events = Vec::new();
        if !self.reasoning.is_empty() {
            events.push(ResponseEvent::OutputItemDone(item(json!({"type":"reasoning","id":format!("{}_reasoning",self.id),"summary":[],"content":[{"type":"reasoning_text","text":self.reasoning}],"encrypted_content":null}))?));
        }
        if !self.content.is_empty() {
            events.push(ResponseEvent::OutputItemDone(item(json!({"type":"message","id":format!("{}_message",self.id),"role":"assistant","content":[{"type":"output_text","text":self.content}]}))?));
        }
        for call in self.calls.into_values() {
            let name = call["name"]
                .as_str()
                .ok_or_else(|| ApiError::Stream("Lithos tool has no name".into()))?;
            if call["id"].as_str().is_none_or(str::is_empty) || name.is_empty() {
                return Err(ApiError::Stream("Incomplete Lithos tool identity".into()));
            }
            let value = if custom.contains(name) {
                let arguments: Value = serde_json::from_str(
                    call["arguments"].as_str().unwrap_or_default(),
                )
                .map_err(|e| ApiError::Stream(format!("Invalid custom tool arguments: {e}")))?;
                let input = arguments["input"]
                    .as_str()
                    .ok_or_else(|| ApiError::Stream("Custom tool input must be a string".into()))?;
                json!({"type":"custom_tool_call","id":call["id"],"call_id":call["id"],"name":name,"input":input})
            } else {
                serde_json::from_str::<Value>(call["arguments"].as_str().unwrap_or_default())
                    .map_err(|e| ApiError::Stream(format!("Invalid tool arguments: {e}")))?;
                json!({"type":"function_call","id":call["id"],"call_id":call["id"],"name":name,"arguments":call["arguments"]})
            };
            events.push(ResponseEvent::OutputItemAdded(item(value.clone())?));
            events.push(ResponseEvent::OutputItemDone(item(value)?));
        }
        events.push(ResponseEvent::Completed {
            response_id: self.id,
            token_usage: self.usage,
            usage_metadata: None,
            reasoning_removals: Default::default(),
            end_turn: Some(self.finish.as_deref() == Some("stop")),
        });
        Ok(events)
    }
}

pub(crate) async fn stream<T: HttpTransport>(
    session: &EndpointSession<T>,
    request: &ResponsesApiRequest,
    headers: HeaderMap,
) -> Result<ResponseStream, ApiError> {
    let logical = serde_json::to_value(request).map_err(|e| invalid(e.to_string()))?;
    let (mut body, custom) = request_body(&logical)?;
    // Routing must not wait for a potentially megabyte-sized message history.
    #[derive(serde::Serialize)]
    struct RoutingFirstBody<'a> {
        model: Value,
        #[serde(flatten)]
        fields: &'a serde_json::Map<String, Value>,
    }
    let fields = body
        .as_object_mut()
        .ok_or_else(|| invalid("Invalid Lithos request"))?;
    let model = fields
        .remove("model")
        .ok_or_else(|| invalid("Lithos request has no model"))?;
    let encoded = EncodedJsonBody::encode(&RoutingFirstBody { model, fields })
        .map_err(|e| invalid(e.to_string()))?;
    let response = session
        .stream_encoded_json_with(
            Method::POST,
            "/chat/completions",
            headers,
            Some(encoded),
            |req| {
                req.headers.insert(
                    http::header::ACCEPT,
                    HeaderValue::from_static("text/event-stream"),
                );
            },
        )
        .await?;
    Ok(response_stream(
        response,
        session.provider().stream_idle_timeout,
        custom,
    ))
}

fn response_stream(
    response: codex_client::StreamResponse,
    idle: std::time::Duration,
    custom: HashSet<String>,
) -> ResponseStream {
    let upstream_request_id = response
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let mut source = response.bytes.eventsource();
        let mut state = ChatState::default();
        loop {
            let next = tokio::select! {
                _=task_cancellation.cancelled()=>return,
                next=tokio::time::timeout(idle,source.next())=>next,
            };
            let events = match next {
                Ok(Some(Ok(event))) if event.data.trim() == "[DONE]" => state.complete(&custom),
                Ok(Some(Ok(event))) => match serde_json::from_str(&event.data) {
                    Ok(chunk) => match state.chunk(chunk) {
                        Ok(events) => {
                            for event in events {
                                if tx.send(Ok(event)).await.is_err() {
                                    return;
                                }
                            }
                            continue;
                        }
                        Err(e) => Err(e),
                    },
                    Err(e) => Err(ApiError::Stream(format!("Invalid Lithos SSE JSON: {e}"))),
                },
                Ok(Some(Err(e))) => Err(ApiError::Stream(format!("Lithos SSE disconnected: {e}"))),
                Ok(None) => Err(ApiError::Stream("Lithos SSE ended before [DONE]".into())),
                Err(_) => Err(ApiError::Stream("Lithos SSE idle timeout".into())),
            };
            match events {
                Ok(events) => {
                    for event in events {
                        if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                }
            }
            return;
        }
    });
    ResponseStream {
        routing_affinity: None,
        rx_event: ResponseStreamReceiver::new(rx, cancellation),
        upstream_request_id,
        interrupt: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use bytes::Bytes;
    use std::time::Duration;

    #[test]
    fn lithos_preserves_reasoning_tools_and_input_without_mutating_history() -> Result<()> {
        let request = json!({"model":"deepseek-ai/DeepSeek-V4.1-Flash-fast","instructions":"system","reasoning":{"effort":"off"},"tools":[{"type":"custom","name":"patch","description":"Apply a patch","format":{"type":"grammar"}}],"input":[{"type":"reasoning","summary":[],"content":[{"type":"reasoning_text","text":"prior thought"}]},{"type":"custom_tool_call","name":"patch","call_id":"c1","input":"*** Begin Patch\n*** End Patch"},{"type":"custom_tool_call_output","call_id":"c1","output":"done"},{"type":"message","role":"user","content":[{"type":"input_text","text":"continue"}]}]});
        let before = request.clone();
        let (body, custom) = request_body(&request)?;
        assert_eq!(request, before);
        assert!(custom.contains("patch"));
        assert_eq!(body["model"], request["model"]);
        assert_eq!(body["reasoning_effort"], "none");
        assert_eq!(body["messages"][1]["reasoning_content"], "prior thought");
        let args: Value = serde_json::from_str(
            body["messages"][1]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing arguments"))?,
        )?;
        assert_eq!(args["input"], request["input"][1]["input"]);
        assert_eq!(body["messages"][2]["tool_call_id"], "c1");
        assert!(body.get("previous_response_id").is_none());
        Ok(())
    }

    #[test]
    fn lithos_duplicate_tool_names_resolve_to_the_latest_definition() -> Result<()> {
        let request = json!({
            "model": "test",
            "tools": [
                {"type":"function","name":"exec","description":"current","parameters":{"type":"object"}},
            ],
            "input": [
                {"type":"additional_tools","tools":[
                    {"type":"function","name":"exec","description":"oldest","parameters":{}},
                    {"type":"function","name":"only_in_history","description":"kept","parameters":{}}
                ]},
                {"type":"additional_tools","tools":[
                    {"type":"function","name":"exec","description":"newer","parameters":{"type":"string"}}
                ]}
            ]
        });
        let (body, _) = request_body(&request)?;
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        let exec = tools
            .iter()
            .find(|tool| tool["function"]["name"] == "exec")
            .unwrap();
        // The request's own tool list wins over replayed history.
        assert_eq!(exec["function"]["description"], "current");
        let history_only = tools
            .iter()
            .find(|tool| tool["function"]["name"] == "only_in_history")
            .unwrap();
        assert_eq!(history_only["function"]["description"], "kept");
        Ok(())
    }

    #[test]
    fn lithos_later_additional_tools_update_earlier_definitions() -> Result<()> {
        let request = json!({
            "model": "test",
            "input": [
                {"type":"additional_tools","tools":[
                    {"type":"function","name":"exec","description":"oldest","parameters":{}}
                ]},
                {"type":"additional_tools","tools":[
                    {"type":"function","name":"exec","description":"newer","parameters":{"type":"string"}}
                ]}
            ]
        });
        let (body, _) = request_body(&request)?;
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["description"], "newer");
        Ok(())
    }

    #[test]
    fn lithos_redefined_tool_kind_controls_response_decoding() -> Result<()> {
        for (old_kind, new_kind) in [("custom", "function"), ("function", "custom")] {
            let request = json!({
                "input": [{"type":"additional_tools","tools":[
                    {"type":old_kind,"name":"exec","parameters":{"type":"object"}}
                ]}],
                "tools": [{"type":new_kind,"name":"exec","parameters":{"type":"object"}}]
            });
            let (body, custom) = request_body(&request)?;
            assert_eq!(body["tools"].as_array().unwrap().len(), 1);
            assert_eq!(custom.contains("exec"), new_kind == "custom");
            let arguments = if new_kind == "custom" {
                r#"{"input":"echo hello"}"#
            } else {
                r#"{"command":"echo hello"}"#
            };
            let mut state = ChatState::default();
            state.chunk(json!({"id":"chat","choices":[{
                "index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"call_exec","function":{"name":"exec","arguments":arguments}
                }]},"finish_reason":"tool_calls"
            }]}))?;
            let events = state.complete(&custom)?;
            let expected = if new_kind == "custom" {
                "custom_tool_call"
            } else {
                "function_call"
            };
            assert!(events.iter().any(|event| {
                matches!(event, ResponseEvent::OutputItemDone(item)
                    if serde_json::to_value(item).unwrap()["type"] == expected)
            }));
        }
        Ok(())
    }

    #[test]
    fn lithos_rejects_responses_only_content_instead_of_dropping_it() {
        assert!(
            request_body(&json!({"model":"test","input":[],"previous_response_id":"resp_old"}))
                .is_err()
        );
    }

    #[test]
    fn lithos_drops_foreign_compaction_instead_of_failing_the_switch() -> Result<()> {
        // An OpenAI-issued compaction token has no whitespace and cannot be
        // decoded by Lithos. A provider switch must still succeed: the turn is
        // served without that summary rather than erroring out.
        let (body, _) = request_body(&json!({"model":"test","input":[
            {"type":"compaction","encrypted_content":"opaqueEncryptedToken"},
            {"type":"message","role":"user","content":"after the switch"}
        ]}))?;
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "after the switch");
        Ok(())
    }

    #[test]
    fn lithos_accepts_a_real_openai_switch_payload() -> Result<()> {
        // Byte shapes taken from a live OpenAI rollout on agent-gpu-0:
        // reasoning items carry encrypted_content with content=None.
        let (body, _) = request_body(&json!({"model":"test","input":[
            {"type":"reasoning","summary":[],
             "encrypted_content":"gAAAAABq4Ynrjlim6j5BqUaZAW5wDKDZo9YTONo-"},
            {"type":"compaction","encrypted_content":"gAAAAABq4Yn43WsbxUTB2OMQ_B-31Hf83EVyE5zA"},
            {"type":"message","role":"user","content":"continue after the switch"},
            {"type":"function_call","name":"exec_command","arguments":"{}","call_id":"c1"},
            {"type":"function_call_output","call_id":"c1","output":"ok"}
        ]}))?;
        let msgs = body["messages"].as_array().expect("messages");
        assert_eq!(msgs[0]["content"], "continue after the switch");
        let roles: Vec<&str> = msgs.iter().filter_map(|m| m["role"].as_str()).collect();
        assert!(roles.contains(&"tool"), "tool output preserved: {roles:?}");
        Ok(())
    }

    #[test]
    fn lithos_still_honours_a_readable_compaction_summary() -> Result<()> {
        let summary = "## Active Objective\nContinue the previous work.";
        let (body, _) = request_body(&json!({"model":"test","input":[
            {"type":"compaction","encrypted_content":summary}
        ]}))?;
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], summary);
        Ok(())
    }

    #[test]
    fn lithos_drops_encrypted_reasoning_without_failing() -> Result<()> {
        let (body, _) = request_body(&json!({"model":"test","input":[
            {"type":"reasoning","summary":[],"encrypted_content":"encrypted-reasoning"},
            {"type":"message","role":"user","content":"hello"}
        ]}))?;
        assert_eq!(body["messages"][0]["content"], "hello");
        Ok(())
    }

    #[test]
    fn lithos_text_blocks_do_not_gain_unrequested_separators() -> Result<()> {
        assert_eq!(
            text(&json!([{"type":"input_text","text":"a"},{"type":"input_text","text":"b"}]))?,
            "ab"
        );
        Ok(())
    }

    #[test]
    fn lithos_stream_keeps_tool_arguments_and_usage_structured() -> Result<()> {
        let mut state = ChatState::default();
        state.chunk(json!({"id":"chat_1","choices":[{"index":0,"delta":{"reasoning_content":"think","tool_calls":[{"index":0,"id":"c1","function":{"name":"exec_","arguments":"{\"cmd\":"}}]}}]}))?;
        state.chunk(json!({"id":"chat_1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"command","arguments":"\"pwd\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":100,"completion_tokens":12,"total_tokens":112,"prompt_tokens_details":{"cached_tokens":80},"completion_tokens_details":{"reasoning_tokens":5}}}))?;
        let events = state.complete(&HashSet::new())?;
        assert!(events.iter().any(|e|matches!(e,ResponseEvent::OutputItemDone(ResponseItem::FunctionCall{name,arguments,..}) if name=="exec_command" && arguments=="{\"cmd\":\"pwd\"}")));
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, ResponseEvent::OutputTextDelta(_)))
        );
        assert!(matches!(
            events.last(),
            Some(ResponseEvent::Completed {
                token_usage: Some(TokenUsage {
                    input_tokens: 100,
                    cached_input_tokens: 80,
                    output_tokens: 12,
                    ..
                }),
                end_turn: Some(false),
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn lithos_rejects_truncated_calls_and_streams() -> Result<()> {
        let mut state = ChatState::default();
        state.chunk(
            json!({"id":"chat_1","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}),
        )?;
        assert!(state.complete(&HashSet::new()).is_err());
        assert!(ChatState::default().complete(&HashSet::new()).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn lithos_sse_handles_fragmented_utf8_and_completes() -> Result<()> {
        let wire = "data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你好\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let chunks: Vec<_> = wire
            .as_bytes()
            .chunks(3)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        let response = codex_client::StreamResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(futures::stream::iter(chunks)),
        };
        let mut stream = response_stream(response, Duration::from_secs(1), HashSet::new());
        assert!(stream.routing_affinity.is_none());
        let mut text = String::new();
        let mut completed = false;
        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::OutputTextDelta(s) => text.push_str(&s),
                ResponseEvent::Completed { .. } => completed = true,
                _ => {}
            }
        }
        assert_eq!(text, "你好");
        assert!(completed);
        Ok(())
    }

    #[tokio::test]
    async fn lithos_sse_idle_timeout_is_an_error() {
        let response = codex_client::StreamResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(futures::stream::pending()),
        };
        let mut stream = response_stream(response, Duration::from_millis(5), HashSet::new());
        assert!(matches!(
            stream.next().await,
            Some(Err(ApiError::Stream(_)))
        ));
    }

    #[test]
    fn lithos_custom_tool_call_is_restored_as_a_custom_item() -> Result<()> {
        let mut state = ChatState::default();
        state.chunk(json!({"id":"chat_custom","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_patch","function":{"name":"patch","arguments":"{\"input\":\"a\\nb\"}"}}]},"finish_reason":"tool_calls"}]}))?;
        let events = state.complete(&HashSet::from(["patch".to_owned()]))?;
        assert!(events.iter().any(|e| matches!(e,ResponseEvent::OutputItemDone(ResponseItem::CustomToolCall{name,input,call_id,..}) if name=="patch" && input=="a\nb" && call_id=="call_patch")));
        Ok(())
    }

    #[tokio::test]
    async fn lithos_eof_does_not_fake_a_completed_response() {
        let wire=Bytes::from_static(b"data: {\"id\":\"chat_1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
        let response = codex_client::StreamResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(futures::stream::iter([Ok(wire)])),
        };
        let mut stream = response_stream(response, Duration::from_secs(1), HashSet::new());
        let mut error = false;
        while let Some(event) = stream.next().await {
            assert!(!matches!(event, Ok(ResponseEvent::Completed { .. })));
            error |= event.is_err();
        }
        assert!(error);
    }

    #[tokio::test]
    async fn lithos_dropping_the_response_releases_the_upstream_stream() -> Result<()> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct DropProbe(Arc<AtomicBool>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let probe = DropProbe(Arc::clone(&dropped));
        let bytes = futures::stream::pending().map(move |event| {
            let _ = &probe;
            event
        });
        let response = codex_client::StreamResponse {
            status: http::StatusCode::OK,
            headers: HeaderMap::new(),
            bytes: Box::pin(bytes),
        };
        let stream = response_stream(response, Duration::from_secs(900), HashSet::new());
        drop(stream);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await
            }
        })
        .await?;
        Ok(())
    }
}
