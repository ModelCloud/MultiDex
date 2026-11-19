//! Keeps the tool declarations sent to prefix-cached providers stable for a thread.
//!
//! MCP catalogs are captured again for every sampling step. A server that is still starting,
//! reconnecting, or serving a newer revision of its catalog changes that capture. Tools render
//! ahead of the conversation, so any change discards the provider's cached prefix and breaks
//! HTTP continuation (`previous_response_id`), which compares the full tool list. Appending
//! does not help: whatever follows the tools, including the whole conversation, is recomputed.
//!
//! The first declaration of each tool is kept while the thread stays in one context window
//! with one model:
//! - a later description or schema revision is ignored, while calls still reach the live tool;
//! - a tool that disappears stays declared, and a call to it is rejected by the step's router
//!   as an unsupported call, which the model receives as the tool output;
//! - a tool that has never been declared is withheld once the conversation has grown, and
//!   becomes visible with the next context window or thread. While the conversation is still
//!   short (an MCP server finishing startup after the first step), it is appended instead,
//!   because recomputing a short prompt costs little.
//!
//! A deliberate catalog change, such as an explicit MCP reconnect, renews the contract once
//! from the live catalog. A new context window or model already rebuilds the prompt, so the
//! contract restarts there from the current catalog and drops definitions no longer served.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::Arc;

use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolSpec;
use tracing::debug;
use tracing::info;

/// Conversation size, in estimated tokens, up to which newly available tools may still join
/// the contract. Recomputing a prompt this short after the tool block is cheap, and it lets a
/// slow MCP server that finishes startup during the first steps of a thread stay usable.
pub(crate) const TOOL_CONTRACT_EXTENSION_MAX_CONVERSATION_TOKENS: i64 = 16_384;

/// The tool list one thread has advertised to one model in one context window.
#[derive(Debug, Default)]
pub(crate) struct ToolContract {
    model: String,
    window_id: String,
    tools: Option<Arc<[ToolSpec]>>,
    /// Newly available tools already reported as withheld, so drift is logged once.
    withheld: BTreeSet<String>,
    /// Why the next catalog change should renew the contract instead of being held back.
    renewal: Option<&'static str>,
}

#[derive(Default)]
struct Drift {
    added: Vec<String>,
    revised: usize,
    unavailable: usize,
}

impl ToolContract {
    /// Returns the declarations to advertise for `current`. A different model or context
    /// window starts a new contract. Within one, the declared tools are kept, and tools never
    /// declared are added only while `conversation_tokens` reports a short conversation, or
    /// once after [`ToolContract::renew`].
    pub(crate) fn advertise(
        &mut self,
        model: &str,
        window_id: &str,
        current: &Arc<[ToolSpec]>,
        conversation_tokens: impl FnOnce() -> i64,
    ) -> Arc<[ToolSpec]> {
        let pinned = match &self.tools {
            Some(pinned) if self.model == model && self.window_id == window_id => {
                Arc::clone(pinned)
            }
            _ => {
                *self = Self {
                    model: model.to_string(),
                    window_id: window_id.to_string(),
                    tools: Some(Arc::clone(current)),
                    ..Self::default()
                };
                return Arc::clone(current);
            }
        };
        if pinned == *current {
            return pinned;
        }
        if let Some(reason) = self.renewal.take() {
            info!(
                model,
                window_id,
                reason,
                declared = pinned.len(),
                live = current.len(),
                "renewing the thread's tool contract from the live catalog; the provider \
                 recomputes the prompt after the tool block once"
            );
            self.withheld.clear();
            self.tools = Some(Arc::clone(current));
            return Arc::clone(current);
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
        let conversation_tokens = conversation_tokens();
        if conversation_tokens > TOOL_CONTRACT_EXTENSION_MAX_CONVERSATION_TOKENS {
            let unreported: Vec<&str> = drift
                .added
                .iter()
                .filter(|name| !self.withheld.contains(name.as_str()))
                .map(String::as_str)
                .collect();
            if !unreported.is_empty() {
                info!(
                    model,
                    window_id,
                    conversation_tokens,
                    withheld = drift.added.len(),
                    tools = %unreported.join(", "),
                    "withholding newly available tools until the next context window; adding \
                     them now would make the provider recompute the whole conversation \
                     (an explicit MCP refresh renews the tool contract)"
                );
                self.withheld.extend(drift.added);
            }
            return pinned;
        }
        info!(
            model,
            window_id,
            conversation_tokens,
            added = drift.added.len(),
            tools = %drift.added.join(", "),
            "extending the thread's tool contract with newly available tools"
        );
        let merged: Arc<[ToolSpec]> = merged.into();
        self.tools = Some(Arc::clone(&merged));
        merged
    }

    /// Lets the next catalog change replace the contract once. Used for deliberate changes,
    /// such as an explicit MCP reconnect, where the new tools are wanted in this thread now.
    pub(crate) fn renew(&mut self, reason: &'static str) {
        if self.tools.is_some() {
            self.renewal = Some(reason);
        }
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
