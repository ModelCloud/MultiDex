//! Keeps the tool declarations sent to prefix-cached providers stable for a thread.
//!
//! MCP catalogs are captured again for every sampling step. A server that is still starting,
//! reconnecting, or serving a newer revision of its descriptions changes that capture. Tools
//! render ahead of the conversation, so any change discards the provider's cached prefix and
//! breaks HTTP continuation (`previous_response_id`), which compares the full tool list.
//!
//! The first declaration of each tool is kept while the thread stays in one context window
//! with one model:
//! - a later description or schema revision is ignored, while calls still reach the live tool;
//! - a tool that disappears stays declared, and a call to it is rejected by the step's router
//!   as an unsupported call, which the model receives as the tool output;
//! - a tool that has never been declared is appended after the declared ones.
//!
//! A new context window or model already rebuilds the prompt, so the contract restarts there
//! from the current catalog and drops definitions that are no longer served.

use std::collections::HashMap;
use std::sync::Arc;

use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolSpec;
use tracing::debug;
use tracing::info;

/// The tool list one thread has advertised to one model in one context window.
#[derive(Debug, Default)]
pub(crate) struct ToolContract {
    model: String,
    window_id: String,
    tools: Option<Arc<[ToolSpec]>>,
}

#[derive(Default)]
struct Drift {
    added: Vec<String>,
    revised: usize,
    unavailable: usize,
}

impl ToolContract {
    /// Returns the declarations to advertise for `current`, extending the contract only with
    /// tools it has never declared. A different model or context window starts a new contract.
    pub(crate) fn advertise(
        &mut self,
        model: &str,
        window_id: &str,
        current: &Arc<[ToolSpec]>,
    ) -> Arc<[ToolSpec]> {
        let pinned = match &self.tools {
            Some(pinned) if self.model == model && self.window_id == window_id => {
                Arc::clone(pinned)
            }
            _ => {
                self.model = model.to_string();
                self.window_id = window_id.to_string();
                self.tools = Some(Arc::clone(current));
                return Arc::clone(current);
            }
        };
        if pinned == *current {
            return pinned;
        }
        let mut drift = Drift::default();
        let merged = merge(&pinned, current, &mut drift);
        if drift.revised > 0 || drift.unavailable > 0 {
            debug!(
                model,
                revised = drift.revised,
                unavailable = drift.unavailable,
                window_id,
                "keeping declared tool definitions stable for this context window"
            );
        }
        if drift.added.is_empty() {
            return pinned;
        }
        info!(
            model,
            window_id,
            added = drift.added.len(),
            tools = %drift.added.join(", "),
            "extending the thread's tool contract with newly available tools"
        );
        let merged: Arc<[ToolSpec]> = merged.into();
        self.tools = Some(Arc::clone(&merged));
        merged
    }
}

fn merge(pinned: &[ToolSpec], current: &[ToolSpec], drift: &mut Drift) -> Vec<ToolSpec> {
    let mut current_by_key = HashMap::with_capacity(current.len());
    for tool in current {
        current_by_key.entry(spec_key(tool)).or_insert(tool);
    }
    let mut merged = Vec::with_capacity(pinned.len().max(current.len()));
    for declared in pinned {
        let Some(live) = current_by_key.remove(&spec_key(declared)) else {
            drift.unavailable += 1;
            merged.push(declared.clone());
            continue;
        };
        merged.push(match (declared, live) {
            (ToolSpec::Namespace(declared), ToolSpec::Namespace(live)) => {
                ToolSpec::Namespace(ResponsesApiNamespace {
                    name: declared.name.clone(),
                    description: declared.description.clone(),
                    tools: merge_namespace(declared, live, drift),
                })
            }
            _ => {
                if declared != live {
                    drift.revised += 1;
                }
                declared.clone()
            }
        });
    }
    for tool in current {
        if current_by_key.remove(&spec_key(tool)).is_some() {
            drift.added.push(tool.name().to_string());
            merged.push(tool.clone());
        }
    }
    merged
}

fn merge_namespace(
    declared: &ResponsesApiNamespace,
    live: &ResponsesApiNamespace,
    drift: &mut Drift,
) -> Vec<ResponsesApiNamespaceTool> {
    if declared.description != live.description {
        drift.revised += 1;
    }
    let mut live_by_name = HashMap::with_capacity(live.tools.len());
    for tool in &live.tools {
        live_by_name.entry(namespace_tool_key(tool)).or_insert(tool);
    }
    let mut merged = Vec::with_capacity(declared.tools.len().max(live.tools.len()));
    for tool in &declared.tools {
        match live_by_name.remove(&namespace_tool_key(tool)) {
            Some(live) if live != tool => drift.revised += 1,
            Some(_) => {}
            None => drift.unavailable += 1,
        }
        merged.push(tool.clone());
    }
    for tool in &live.tools {
        let key = namespace_tool_key(tool);
        if live_by_name.remove(&key).is_some() {
            drift.added.push(format!("{}{}", declared.name, key.1));
            merged.push(tool.clone());
        }
    }
    merged
}

fn spec_key(tool: &ToolSpec) -> (&'static str, &str) {
    let kind = match tool {
        ToolSpec::Function(_) => "function",
        ToolSpec::Namespace(_) => "namespace",
        ToolSpec::ToolSearch { .. } => "tool_search",
        ToolSpec::WebSearch { .. } => "web_search",
        ToolSpec::Freeform(_) => "custom",
    };
    (kind, tool.name())
}

fn namespace_tool_key(tool: &ResponsesApiNamespaceTool) -> (&'static str, &str) {
    match tool {
        ResponsesApiNamespaceTool::Function(tool) => ("function", tool.name.as_str()),
        ResponsesApiNamespaceTool::Custom(tool) => ("custom", tool.name.as_str()),
    }
}

#[cfg(test)]
#[path = "tool_contract_tests.rs"]
mod tests;
