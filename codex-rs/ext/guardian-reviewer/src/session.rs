//! Recognizes Guardian session sources and scopes legacy reviewer cache keys to the parent thread.

use codex_protocol::ThreadId;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;

pub const GUARDIAN_REVIEWER_NAME: &str = "guardian";

pub fn is_basic_session_source(session_source: &SessionSource) -> bool {
    match session_source {
        SessionSource::SubAgent(SubAgentSource::Other(label)) => label == GUARDIAN_REVIEWER_NAME,
        SessionSource::Internal(InternalSessionSource::Guardian) => true,
        _ => false,
    }
}

pub fn prompt_cache_key_override_for_review_session(
    session_source: &SessionSource,
    parent_thread_id: Option<ThreadId>,
    review_thread_id: Option<ThreadId>,
) -> Option<String> {
    let SessionSource::SubAgent(SubAgentSource::Other(name)) = session_source else {
        return None;
    };
    if name != GUARDIAN_REVIEWER_NAME {
        return None;
    }
    let parent_thread_id = parent_thread_id?;
    // Several review sessions can share one parent thread (a persistent review
    // session plus one-off reviews). A parent-only key made them overwrite each
    // other's continuation record, so the persistent reviewer re-prefilled its
    // whole transcript. Keep the parent link and add the review thread's random
    // tail; the Responses API caps the key at 64 bytes.
    let Some(review_thread_id) = review_thread_id else {
        return Some(format!("guardian:{parent_thread_id}"));
    };
    let review_thread_id = review_thread_id.to_string();
    let tag = &review_thread_id[review_thread_id.len().saturating_sub(8)..];
    Some(format!("guardian:{parent_thread_id}:{tag}"))
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
