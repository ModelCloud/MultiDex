//! Translate namespaces only at provider boundaries that cannot carry Responses
//! namespace tools. Logical requests, tool dispatch, and persisted history
//! continue to use upstream tool identities.
use codex_api::ResponsesApiRequest;
use codex_extension_api::ModelResponseInterceptor;
use codex_extension_api::ModelResponseStream;
use codex_extension_api::ResponseEvent;
use codex_protocol::ToolName;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::models::ResponseItem;
use futures::StreamExt;
use serde_json::Value;
use serde_json::value::RawValue;
use sha1::Digest;
use sha1::Sha1;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct WireRequest {
    pub(crate) tools: Option<Arc<RawValue>>,
    pub(crate) input: Vec<ResponseItem>,
}

/// Inspect the *full* logical request, even when only a bounded continuation delta
/// is going on the wire. Never replace the caller's continuation baseline.
pub(crate) fn prepare(
    flatten_namespaces: bool,
    full_request: &ResponsesApiRequest,
    input: &[ResponseItem],
) -> Result<Option<(WireRequest, Box<dyn ModelResponseInterceptor>)>> {
    if !flatten_namespaces {
        return Ok(None);
    }
    let mut names = Names::default();
    let tools = full_request
        .tools
        .as_ref()
        .map(|tools| -> Result<Arc<RawValue>> {
            let tools = serde_json::from_value(serde_json::to_value(tools)?)?;
            let tools = names.flatten_tools(tools)?;
            Ok(Arc::from(serde_json::value::to_raw_value(&tools)?))
        })
        .transpose()?;
    // Calls can precede or follow their output items; collect their identities first.
    for item in &full_request.input {
        if let ResponseItem::FunctionCall {
            name,
            namespace,
            call_id,
            ..
        }
        | ResponseItem::CustomToolCall {
            name,
            namespace,
            call_id,
            ..
        } = item
        {
            names.calls.insert(
                call_id.clone(),
                ToolName::new(namespace.clone(), name.clone()),
            );
        }
    }
    for item in &full_request.input {
        // Register definitions and names in history that continuation/bounding may omit.
        if matches!(
            item,
            ResponseItem::FunctionCall { .. }
                | ResponseItem::CustomToolCall { .. }
                | ResponseItem::FunctionCallOutput { name: Some(_), .. }
                | ResponseItem::CustomToolCallOutput { name: Some(_), .. }
                | ResponseItem::AdditionalTools { .. }
                | ResponseItem::ToolSearchOutput { .. }
        ) {
            names.outbound_item(&mut item.clone())?;
        }
    }
    let mut input = input.to_vec();
    for item in &mut input {
        names.outbound_item(item)?;
    }
    Ok(Some((WireRequest { tools, input }, Box::new(names))))
}

#[derive(Default)]
struct Names {
    original: BTreeMap<String, ToolName>,
    calls: BTreeMap<String, ToolName>,
}

/// Aliases depend only on identity, not inventory order, length, or a retry counter.
/// Length framing distinguishes even names containing delimiters or Unicode.
fn wire_name(tool: &ToolName) -> String {
    if tool.is_default_namespace() {
        return tool.name.clone();
    }
    let mut hash = Sha1::new();
    for part in [tool.namespace.as_deref().unwrap_or_default(), &tool.name] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let hint: String = tool
        .name
        .chars()
        .take(19)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("ldx_{hint}_{:x}", hash.finalize())
}

impl Names {
    fn register(&mut self, tool: ToolName) -> Result<String> {
        let alias = wire_name(&tool);
        if let Some(existing) = self.original.get(&alias) {
            let same_namespace = existing.namespace == tool.namespace
                || (existing.is_default_namespace() && tool.is_default_namespace());
            if existing.name != tool.name || !same_namespace {
                // Reassigning would invalidate names in stored server responses. Fail
                // explicitly, including when a plain tool reserves a generated alias.
                return Err(CodexErr::InvalidRequest(format!(
                    "MultiDex tool namespace alias collision: {alias}"
                )));
            }
        } else {
            self.original.insert(alias.clone(), tool);
        }
        Ok(alias)
    }

    fn flatten_tools(&mut self, tools: Vec<Value>) -> Result<Vec<Value>> {
        let mut flat = Vec::new();
        for mut tool in tools {
            if tool.get("type").and_then(Value::as_str) == Some("namespace") {
                let namespace = tool.get("name").and_then(Value::as_str).ok_or_else(|| {
                    CodexErr::InvalidRequest("MultiDex tool namespace has no name".into())
                })?;
                let children = tool.get("tools").and_then(Value::as_array).ok_or_else(|| {
                    CodexErr::InvalidRequest("MultiDex tool namespace has no tools array".into())
                })?;
                let guidance = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                for mut child in children.clone() {
                    // Only inspect definition fields, never parameters, formats, or arguments.
                    if !matches!(
                        child.get("type").and_then(Value::as_str),
                        Some("function" | "custom")
                    ) {
                        return Err(CodexErr::InvalidRequest(
                            "MultiDex tool namespace contains an unsupported tool definition"
                                .into(),
                        ));
                    }
                    self.rename_definition(&mut child, Some(namespace))?;
                    if !guidance.is_empty() {
                        let description = child
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        child["description"] = Value::String(if description.is_empty() {
                            guidance.to_string()
                        } else {
                            format!("{guidance}\n\n{description}")
                        });
                    }
                    flat.push(child);
                }
            } else {
                if matches!(
                    tool.get("type").and_then(Value::as_str),
                    Some("function" | "custom")
                ) {
                    self.rename_definition(&mut tool, None)?;
                }
                flat.push(tool);
            }
        }
        Ok(flat)
    }

    fn rename_definition(&mut self, tool: &mut Value, namespace: Option<&str>) -> Result<()> {
        let name = tool.get("name").and_then(Value::as_str).ok_or_else(|| {
            CodexErr::InvalidRequest("MultiDex tool definition has no name".into())
        })?;
        let alias = self.register(ToolName::new(namespace.map(str::to_string), name))?;
        tool["name"] = Value::String(alias);
        Ok(())
    }

    fn outbound_item(&mut self, item: &mut ResponseItem) -> Result<()> {
        match item {
            ResponseItem::FunctionCall {
                name, namespace, ..
            }
            | ResponseItem::CustomToolCall {
                name, namespace, ..
            } => {
                *name = self.register(ToolName::new(namespace.take(), name.clone()))?;
            }
            ResponseItem::FunctionCallOutput {
                name: Some(name),
                namespace,
                ..
            } => {
                *name = self.register(ToolName::new(namespace.take(), name.clone()))?;
            }
            ResponseItem::CustomToolCallOutput {
                name: Some(name),
                call_id,
                ..
            } => {
                let tool = self
                    .calls
                    .get(call_id)
                    .cloned()
                    .unwrap_or_else(|| ToolName::plain(name.clone()));
                *name = self.register(tool)?;
            }
            ResponseItem::AdditionalTools { tools, .. } => {
                *tools = self.flatten_tools(tools.to_vec())?.into();
            }
            ResponseItem::ToolSearchOutput { tools, .. } => {
                *tools = self.flatten_tools(std::mem::take(tools))?;
            }
            _ => {}
        }
        Ok(())
    }

    fn restore(&self, name: &mut String, namespace: &mut Option<String>) {
        if ToolName::new(namespace.clone(), name.clone()).is_default_namespace()
            && let Some(original) = self.original.get(name)
        {
            *name = original.name.clone();
            *namespace = original.namespace.clone();
        }
    }

    fn inbound_item(&self, item: &mut ResponseItem) {
        match item {
            ResponseItem::FunctionCall {
                name, namespace, ..
            }
            | ResponseItem::CustomToolCall {
                name, namespace, ..
            }
            | ResponseItem::FunctionCallOutput {
                name: Some(name),
                namespace,
                ..
            } => {
                self.restore(name, namespace);
            }
            ResponseItem::CustomToolCallOutput {
                name: Some(name), ..
            } => {
                if let Some(original) = self.original.get(name) {
                    *name = original.name.clone();
                }
            }
            _ => {}
        }
    }
}

impl ModelResponseInterceptor for Names {
    fn intercept(self: Box<Self>, stream: ModelResponseStream) -> ModelResponseStream {
        Box::pin(stream.map(move |event| {
            event.map(|mut event| {
                if let ResponseEvent::OutputItemAdded(item) | ResponseEvent::OutputItemDone(item) =
                    &mut event
                {
                    self.inbound_item(item);
                }
                event
            })
        }))
    }
}

#[cfg(test)]
#[path = "multidex_tool_namespace_tests.rs"]
mod tests;
