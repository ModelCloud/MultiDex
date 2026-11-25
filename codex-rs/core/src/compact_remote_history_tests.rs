use super::*;
use codex_history::CodexHarnessMetadata;

#[test]
fn rewritten_output_preserves_harness_metadata() {
    let envelope = ResponseItemEnvelope {
        item: ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("call-1".to_string()),
            name: None,
            namespace: None,
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("large output".repeat(100)),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        },
        metadata: Some(CodexHarnessMetadata::default()),
    };

    let rewritten = rewritten_output_for_context_window(&envelope)
        .expect("function output should be rewritten");

    assert_eq!(rewritten.metadata, envelope.metadata);
    assert_ne!(rewritten.item, envelope.item);
}

#[test]
fn compacted_function_call_keeps_identity_and_bounds_json_arguments() {
    let arguments = serde_json::json!({ "command": format!("head{}tail", "界".repeat(100_000)) });
    let call = serde_json::json!({
        "type": "function_call",
        "name": "shell",
        "call_id": "call-oversized",
        "arguments": arguments.to_string(),
    });
    let envelope = ResponseItemEnvelope {
        item: serde_json::from_value(call.clone()).unwrap(),
        metadata: Some(CodexHarnessMetadata::default()),
    };
    let rewritten = rewritten_call_for_compaction(&envelope).expect("oversized call");
    let wire = serde_json::to_value(&rewritten.item).unwrap();
    assert_eq!(wire["call_id"], call["call_id"]);
    assert_eq!(wire["name"], call["name"]);
    assert_eq!(rewritten.metadata, envelope.metadata);
    let arguments = wire["arguments"].as_str().unwrap();
    assert!(arguments.len() < 4 * 1024);
    let projected: serde_json::Value = serde_json::from_str(arguments).unwrap();
    let preview = projected["truncated_arguments_preview"].as_str().unwrap();
    assert!(preview.contains("head") && preview.contains("tail"));
    assert_eq!(
        serde_json::to_value(&envelope.item).unwrap()["arguments"],
        call["arguments"]
    );
}
