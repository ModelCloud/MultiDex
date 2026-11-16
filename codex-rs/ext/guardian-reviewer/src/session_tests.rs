#![cfg(test)]

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn guardian_prompt_cache_key_is_scoped_to_parent_and_review_thread() {
    let session_source =
        SessionSource::SubAgent(SubAgentSource::Other(GUARDIAN_REVIEWER_NAME.to_string()));
    let parent_thread_id = ThreadId::new();
    let review_thread_id = ThreadId::new();
    let key = prompt_cache_key_override_for_review_session(
        &session_source,
        Some(parent_thread_id),
        Some(review_thread_id),
    )
    .expect("guardian prompt cache key");

    let review_tag = review_thread_id.to_string();
    let review_tag = &review_tag[review_tag.len() - 8..];
    assert_eq!(key, format!("guardian:{parent_thread_id}:{review_tag}"));
    assert!(
        key.len() <= 64,
        "guardian prompt cache key should fit the Responses API limit"
    );
    assert_eq!(
        key,
        prompt_cache_key_override_for_review_session(
            &session_source,
            Some(parent_thread_id),
            Some(review_thread_id),
        )
        .expect("same guardian prompt cache key")
    );
    // Interleaved reviews of the same parent must not share one continuation
    // record: that overwrote the persistent reviewer's cached prefix.
    assert_ne!(
        key,
        prompt_cache_key_override_for_review_session(
            &session_source,
            Some(parent_thread_id),
            Some(ThreadId::new()),
        )
        .expect("different review thread guardian prompt cache key")
    );
    assert_ne!(
        key,
        prompt_cache_key_override_for_review_session(
            &session_source,
            Some(ThreadId::new()),
            Some(review_thread_id),
        )
        .expect("different parent guardian prompt cache key")
    );
    assert_eq!(
        Some(format!("guardian:{parent_thread_id}")),
        prompt_cache_key_override_for_review_session(
            &session_source,
            Some(parent_thread_id),
            /*review_thread_id*/ None,
        )
    );
    assert_eq!(
        None,
        prompt_cache_key_override_for_review_session(
            &SessionSource::Cli,
            Some(parent_thread_id),
            Some(review_thread_id),
        )
    );
    assert_eq!(
        None,
        prompt_cache_key_override_for_review_session(
            &session_source,
            /*parent_thread_id*/ None,
            Some(review_thread_id),
        )
    );
}
