use super::*;
use codex_api::ResponseCreateWsRequest;
use codex_api::ResponsesWsRequest;
use codex_extension_api::ModelResponseError;
use pretty_assertions::assert_eq;
use serde_json::json;

fn request(tools: Vec<Value>, input: Vec<ResponseItem>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        provider: None,
        model: "zen5-cpu-0".into(),
        max_output_tokens: None,
        input,
        tools: Some(Arc::<RawValue>::from(serde_json::value::to_raw_value(&tools).unwrap()).into()),
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: true,
        previous_response_id: None,
        stream: true,
        stream_options: None,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        prompt_cache_options: None,
        text: None,
        client_metadata: None,
        access_programs: None,
    }
}

fn function(name: &str) -> Value {
    json!({
        "type": "function", "name": name, "description": "Tool guidance",
        "strict": false, "defer_loading": true,
        "parameters": {"type": "object", "properties": {
            "nested": {"const": {"type": "namespace", "name": "keep", "tools": []}}
        }},
        "output_schema": {"type": "string"}, "extra": {"name": "keep"}
    })
}

fn namespace(name: &str, tools: Vec<Value>) -> Value {
    json!({"type": "namespace", "name": name, "description": "Namespace guidance", "tools": tools})
}

fn call(namespace: Option<&str>, name: &str, custom: bool) -> ResponseItem {
    let mut value = json!({
        "type": "function_call", "id": "fc_probe", "call_id": format!("call_{name}"),
        "name": name, "namespace": namespace,
        "arguments": " {\"name\":\"keep\",\"namespace\":\"keep\",\"tools\":[1]} "
    });
    if custom {
        value["type"] = json!("custom_tool_call");
        value["input"] = value.as_object_mut().unwrap().remove("arguments").unwrap();
    }
    serde_json::from_value(value).unwrap()
}

fn tools(wire: &WireRequest) -> Vec<Value> {
    serde_json::from_str(wire.tools.as_ref().unwrap().get()).unwrap()
}

#[tokio::test]
async fn function_and_custom_roundtrip_through_added_and_done() {
    for response_namespace in [None, Some("functions")] {
        let custom = json!({
            "type": "custom", "name": "patch", "description": "Patch guidance",
            "format": {"type": "grammar", "syntax": "lark", "definition": "start: /.+/"}
        });
        let original = vec![
            call(Some("app"), "run", false),
            call(Some("app"), "patch", true),
        ];
        let full = request(
            vec![namespace("app", vec![function("run"), custom.clone()])],
            original.clone(),
        );
        let saved = full.clone();
        let (wire, interceptor) = prepare(true, &full, &full.input).unwrap().unwrap();
        let definitions = tools(&wire);
        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0]["type"], "function");
        assert_eq!(definitions[1]["type"], "custom");
        assert_eq!(definitions[1]["format"], custom["format"]);
        assert_eq!(
            definitions[0]["description"],
            "Namespace guidance\n\nTool guidance"
        );
        for field in [
            "parameters",
            "output_schema",
            "strict",
            "defer_loading",
            "extra",
        ] {
            assert_eq!(definitions[0][field], function("run")[field]);
        }
        let mut events = Vec::new();
        for (index, item) in wire.input.iter().enumerate() {
            let mut encoded = serde_json::to_value(item).unwrap();
            assert_eq!(encoded["name"], definitions[index]["name"]);
            assert!(encoded.get("namespace").is_none());
            // Simulate the provider returning the actual flat wire identity.
            encoded["namespace"] = json!(response_namespace);
            let returned: ResponseItem = serde_json::from_value(encoded).unwrap();
            events.push(Ok(ResponseEvent::OutputItemAdded(returned.clone())));
            events.push(Ok(ResponseEvent::OutputItemDone(returned)));
        }
        let mut stream = crate::model_request::intercept_stream(
            Box::pin(futures::stream::iter(events)),
            vec![interceptor],
        );
        for expected in &original {
            let ResponseEvent::OutputItemAdded(actual) = stream.next().await.unwrap().unwrap()
            else {
                panic!("expected added")
            };
            assert_eq!(&actual, expected);
            let ResponseEvent::OutputItemDone(actual) = stream.next().await.unwrap().unwrap()
            else {
                panic!("expected done")
            };
            assert_eq!(&actual, expected);
        }
        assert!(stream.next().await.is_none());
        assert_eq!(full, saved);
    }
}

#[tokio::test]
async fn duplicate_leaf_names_dispatch_to_the_original_namespace() {
    let originals = vec![
        call(Some("app-a"), "run", false),
        call(Some("app_a"), "run", false),
        call(None, "run", false),
    ];
    let full = request(
        vec![
            namespace("app-a", vec![function("run")]),
            namespace("app_a", vec![function("run")]),
            function("run"),
        ],
        originals.clone(),
    );
    let (wire, interceptor) = prepare(true, &full, &full.input).unwrap().unwrap();
    let definitions = tools(&wire);
    assert_ne!(definitions[0]["name"], definitions[1]["name"]);
    assert_eq!(definitions[2]["name"], "run");
    let events = wire
        .input
        .into_iter()
        .map(|item| Ok(ResponseEvent::OutputItemDone(item)));
    let mut stream = crate::model_request::intercept_stream(
        Box::pin(futures::stream::iter(events)),
        vec![interceptor],
    );
    for original in originals {
        let ResponseEvent::OutputItemDone(actual) = stream.next().await.unwrap().unwrap() else {
            panic!("expected done")
        };
        assert_eq!(actual, original);
    }
}

#[tokio::test]
async fn named_outputs_follow_calls_in_trimmed_history_without_changing_output_payloads() {
    let function_output: ResponseItem = serde_json::from_value(json!({
        "type": "function_call_output", "call_id": "call_run", "name": "run", "namespace": "app",
        "output": [{"type": "input_text", "text": "{\"name\":\"keep\",\"namespace\":\"keep\"}"}]
    }))
    .unwrap();
    let custom_output: ResponseItem = serde_json::from_value(json!({
        "type": "custom_tool_call_output", "call_id": "call_patch", "name": "patch",
        "output": "keep custom result"
    }))
    .unwrap();
    let outputs = vec![function_output, custom_output];
    let full = request(
        Vec::new(),
        vec![
            outputs[0].clone(),
            outputs[1].clone(),
            call(Some("app"), "run", false),
            call(Some("app"), "patch", true),
        ],
    );
    let (full_wire, _) = prepare(true, &full, &full.input).unwrap().unwrap();
    let (wire, interceptor) = prepare(true, &full, &outputs).unwrap().unwrap();
    for (index, item) in wire.input.iter().enumerate() {
        let encoded = serde_json::to_value(item).unwrap();
        let encoded_call = serde_json::to_value(&full_wire.input[index + 2]).unwrap();
        let original = serde_json::to_value(&outputs[index]).unwrap();
        assert_eq!(encoded["name"], encoded_call["name"]);
        assert_eq!(encoded["output"], original["output"]);
        assert!(encoded.get("namespace").is_none());
    }
    let events = wire
        .input
        .into_iter()
        .map(|item| Ok(ResponseEvent::OutputItemDone(item)));
    let mut stream = interceptor.intercept(Box::pin(futures::stream::iter(events)));
    for original in outputs {
        let ResponseEvent::OutputItemDone(actual) = stream.next().await.unwrap().unwrap() else {
            panic!("expected done")
        };
        assert_eq!(actual, original);
    }
}

#[test]
fn aliases_are_distinct_bounded_and_stable_across_inventory_changes() {
    let inventories = [
        namespace("a-b", vec![function("run")]),
        namespace("a_b", vec![function("run")]),
        namespace("a", vec![function("b__run")]),
        namespace("a__b", vec![function("run")]),
        namespace(&"界".repeat(100), vec![function(&"工具?.".repeat(100))]),
    ];
    let full = request(inventories.to_vec(), Vec::new());
    let (wire, _) = prepare(true, &full, &[]).unwrap().unwrap();
    let names: Vec<_> = tools(&wire)
        .into_iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let distinct: std::collections::BTreeSet<_> = names.iter().collect();
    assert_eq!(distinct.len(), names.len());
    for (index, definition) in inventories.iter().enumerate() {
        let name = &names[index];
        assert!(name.len() <= 64);
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        );
        let changed = request(vec![function("plain"), definition.clone()], Vec::new());
        let (wire, _) = prepare(true, &changed, &[]).unwrap().unwrap();
        assert_eq!(tools(&wire)[1]["name"], *name);
    }
    let reversed = request(inventories.into_iter().rev().collect(), Vec::new());
    let (wire, _) = prepare(true, &reversed, &[]).unwrap().unwrap();
    let reversed_names: Vec<_> = tools(&wire)
        .into_iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .rev()
        .collect();
    assert_eq!(names, reversed_names);
}

#[test]
fn reserved_alias_collision_fails_without_mutating_or_dropping_calls() {
    let declaration = namespace("app", vec![function("run")]);
    let full = request(
        vec![declaration.clone()],
        vec![call(Some("app"), "run", false)],
    );
    let (wire, _) = prepare(true, &full, &full.input).unwrap().unwrap();
    let alias = tools(&wire)[0]["name"].as_str().unwrap().to_string();
    for definitions in [
        vec![declaration.clone(), function(&alias)],
        vec![function(&alias), declaration.clone()],
    ] {
        let full = request(definitions, full.input.clone());
        let saved = full.clone();
        let error = prepare(true, &full, &full.input).err().unwrap();
        assert!(error.to_string().contains("alias collision"));
        assert!(error.to_string().contains(&alias));
        assert_eq!(full, saved);
    }
    // A reserved name that exists only in omitted history must also be detected.
    let full = request(vec![declaration], vec![call(None, &alias, false)]);
    assert!(
        prepare(true, &full, &[])
            .err()
            .unwrap()
            .to_string()
            .contains("alias collision")
    );
}

#[tokio::test]
async fn plain_and_default_functions_keep_names_and_restore_default_namespace() {
    for namespace_name in [None, Some("functions")] {
        let definitions = match namespace_name {
            Some(ns) => vec![namespace(ns, vec![function("run")])],
            None => vec![function("run")],
        };
        let original = call(namespace_name, "run", false);
        let full = request(definitions, vec![original.clone()]);
        let (wire, interceptor) = prepare(true, &full, &full.input).unwrap().unwrap();
        assert_eq!(tools(&wire)[0]["name"], "run");
        let encoded = serde_json::to_value(&wire.input[0]).unwrap();
        assert_eq!(encoded["name"], "run");
        assert!(encoded.get("namespace").is_none());
        let mut stream = interceptor.intercept(Box::pin(futures::stream::iter([Ok(
            ResponseEvent::OutputItemDone(wire.input[0].clone()),
        )])));
        let ResponseEvent::OutputItemDone(actual) = stream.next().await.unwrap().unwrap() else {
            panic!("expected done")
        };
        assert_eq!(actual, original);
    }
}

#[tokio::test]
async fn continuation_uses_full_history_mapping_and_preserves_response_id() {
    let original = call(Some("retired_app"), "run", false);
    let first = request(
        vec![namespace("retired_app", vec![function("run")])],
        vec![original.clone()],
    );
    let (first_wire, _) = prepare(true, &first, &first.input).unwrap().unwrap();
    let alias = tools(&first_wire)[0]["name"].clone();
    let output: ResponseItem = serde_json::from_value(json!({
        "type": "function_call_output", "call_id": "call_run", "output": "ok"
    }))
    .unwrap();
    // Tool is no longer declared, and continuation preparation removed its call.
    let full = request(
        vec![function("unrelated")],
        vec![original.clone(), output.clone()],
    );
    let saved = full.clone();
    let mut delta = full.clone();
    delta.previous_response_id = Some("resp_stored".into());
    delta.input = vec![output.clone()];
    let (wire, interceptor) = prepare(true, &full, &delta.input).unwrap().unwrap();
    delta.tools = wire.tools.map(Into::into);
    delta.input = wire.input;
    assert_eq!(delta.input, vec![output]);
    assert_eq!(delta.previous_response_id.as_deref(), Some("resp_stored"));
    assert_eq!(full, saved);
    let mut returned = serde_json::to_value(&first_wire.input[0]).unwrap();
    assert_eq!(returned["name"], alias);
    returned["namespace"] = json!("functions");
    let mut stream = crate::model_request::intercept_stream(
        Box::pin(futures::stream::iter([Ok(ResponseEvent::OutputItemDone(
            serde_json::from_value(returned).unwrap(),
        ))])),
        vec![interceptor],
    );
    let ResponseEvent::OutputItemDone(actual) = stream.next().await.unwrap().unwrap() else {
        panic!("expected done")
    };
    assert_eq!(actual, original);
}

#[test]
fn additional_tools_and_tool_search_definitions_are_flattened_without_touching_payloads() {
    for item_type in ["additional_tools", "tool_search_output"] {
        let definition = namespace("found_app", vec![function("run")]);
        let item: ResponseItem = serde_json::from_value(json!({
            "type": item_type, "role": "assistant", "call_id": "search", "status": "completed",
            "execution": "client", "tools": [definition]
        }))
        .unwrap();
        let full = request(
            Vec::new(),
            vec![item.clone(), call(Some("found_app"), "run", false)],
        );
        let (wire, _) = prepare(true, &full, &full.input).unwrap().unwrap();
        let encoded = serde_json::to_value(&wire.input).unwrap();
        assert_eq!(encoded[0]["tools"][0]["type"], "function");
        assert_eq!(encoded[0]["tools"][0]["name"], encoded[1]["name"]);
        assert_eq!(
            encoded[0]["tools"][0]["parameters"],
            function("run")["parameters"]
        );
        let mut expected = serde_json::to_value(item).unwrap();
        expected["tools"] = encoded[0]["tools"].clone();
        assert_eq!(encoded[0], expected);
    }
}

#[test]
fn malformed_namespace_fails_and_unknown_top_level_tools_are_preserved() {
    for bad in [
        json!({"type": "namespace", "name": "broken"}),
        json!({"type": "namespace", "tools": []}),
        namespace("app", vec![json!({"type": "function"})]),
        namespace("app", vec![json!({"type": "future_tool", "name": "run"})]),
    ] {
        let full = request(vec![function("keep"), bad], Vec::new());
        assert!(prepare(true, &full, &[]).is_err());
    }
    let unknown = json!({"type": "future_tool", "config": {"type": "namespace", "tools": []}});
    let full = request(vec![unknown.clone()], Vec::new());
    let (wire, _) = prepare(true, &full, &[]).unwrap().unwrap();
    assert_eq!(tools(&wire), vec![unknown]);
}

#[test]
fn openai_path_is_a_noop_even_for_unsupported_definitions() {
    let full = request(
        vec![
            namespace("app", vec![function("run")]),
            json!({"type": "namespace"}),
        ],
        vec![call(Some("app"), "run", false)],
    );
    let saved = serde_json::to_string(&full).unwrap();
    assert!(prepare(false, &full, &full.input).unwrap().is_none());
    assert_eq!(serde_json::to_string(&full).unwrap(), saved);
}

#[test]
fn http_and_websocket_wire_shapes_match_and_keep_continuation_fields() {
    let full = request(
        vec![namespace("app", vec![function("run")])],
        vec![call(Some("app"), "run", false)],
    );
    let (http_wire, _) = prepare(true, &full, &full.input).unwrap().unwrap();
    let mut http = full.clone();
    http.previous_response_id = Some("resp_1".into());
    http.tools = http_wire.tools.map(Into::into);
    http.input = http_wire.input;
    let (ws_wire, _) = prepare(true, &full, &full.input).unwrap().unwrap();
    let ws = ResponsesWsRequest::ResponseCreate(ResponseCreateWsRequest {
        previous_response_id: Some("resp_1".into()),
        input: &ws_wire.input,
        tools: ws_wire.tools.as_deref(),
        generate: Some(false),
        ..ResponseCreateWsRequest::from(&full)
    });
    let http = serde_json::to_value(http).unwrap();
    let ws = serde_json::to_value(ws).unwrap();
    for field in ["tools", "input", "previous_response_id", "store"] {
        assert_eq!(http[field], ws[field]);
    }
    assert_eq!(ws["generate"], false);
    assert_eq!(ws["type"], "response.create");
}

#[tokio::test]
async fn interceptor_preserves_unknown_names_deltas_completion_and_errors() {
    let full = request(vec![namespace("app", vec![function("run")])], Vec::new());
    let (_, interceptor) = prepare(true, &full, &[]).unwrap().unwrap();
    let unknown = call(Some("functions"), "app__run", false);
    let mut stream = interceptor.intercept(Box::pin(futures::stream::iter([
        Ok(ResponseEvent::OutputItemDone(unknown.clone())),
        Ok(ResponseEvent::ToolCallInputDelta {
            item_id: "fc_probe".into(),
            call_id: Some("call_run".into()),
            delta: " { untouched } ".into(),
        }),
        Ok(ResponseEvent::Completed {
            response_id: "resp_1".into(),
            token_usage: None,
            usage_metadata: None,
            end_turn: Some(false),
        }),
        Err(ModelResponseError::Stream("upstream failure".into())),
    ])));
    let ResponseEvent::OutputItemDone(actual) = stream.next().await.unwrap().unwrap() else {
        panic!("expected done")
    };
    assert_eq!(actual, unknown);
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ResponseEvent::ToolCallInputDelta { delta, .. } if delta == " { untouched } ")
    );
    assert!(
        matches!(stream.next().await.unwrap().unwrap(), ResponseEvent::Completed { response_id, .. } if response_id == "resp_1")
    );
    assert!(
        matches!(stream.next().await.unwrap(), Err(ModelResponseError::Stream(message)) if message == "upstream failure")
    );
}
