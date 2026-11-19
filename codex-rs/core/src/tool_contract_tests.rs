use super::TOOL_CONTRACT_EXTENSION_MAX_CONVERSATION_TOKENS;
use super::ToolContract;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use std::sync::Arc;

const MODEL: &str = "DS v4.1 Flash 1017";
const WINDOW: &str = "thread:3";
/// A thread a few turns in, where the tool block is followed by a short conversation.
const SHORT: i64 = 4_000;
/// A long-running thread, as in the Lite trace that resent 344K tokens after a catalog change.
const LONG: i64 = 340_000;

fn function(name: &str, description: &str) -> ResponsesApiTool {
    ResponsesApiTool {
        name: name.to_string(),
        description: description.to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::default(),
        output_schema: None,
    }
}

fn namespace(name: &str, tools: &[(&str, &str)]) -> ToolSpec {
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: name.to_string(),
        description: format!("Tools in the {name} namespace."),
        tools: tools
            .iter()
            .map(|(name, description)| {
                ResponsesApiNamespaceTool::Function(function(name, description))
            })
            .collect(),
    })
}

fn tools(specs: Vec<ToolSpec>) -> Arc<[ToolSpec]> {
    specs.into()
}

fn names(specs: &[ToolSpec]) -> Vec<String> {
    specs
        .iter()
        .flat_map(|spec| match spec {
            ToolSpec::Namespace(namespace) => namespace
                .tools
                .iter()
                .map(|tool| match tool {
                    ResponsesApiNamespaceTool::Function(tool) => {
                        format!("{}{}", namespace.name, tool.name)
                    }
                    ResponsesApiNamespaceTool::Custom(tool) => {
                        format!("{}{}", namespace.name, tool.name)
                    }
                })
                .collect::<Vec<_>>(),
            spec => vec![spec.name().to_string()],
        })
        .collect()
}

fn session_tools(apps_description: &str, omnigent: bool) -> Arc<[ToolSpec]> {
    let mut specs = vec![
        ToolSpec::Function(function("exec_command", "Runs a command.")),
        namespace(
            "mcp__codex_apps__",
            &[
                ("_generate_siwc_bypass_token", apps_description),
                ("_edit_page", "Edit a page."),
            ],
        ),
    ];
    if omnigent {
        specs.push(namespace(
            "mcp__omnigent__",
            &[
                ("sys_os_read", "Read a file."),
                ("sys_session_send", "Send to a session."),
            ],
        ));
    }
    specs.push(ToolSpec::Function(function(
        "update_plan",
        "Update the plan.",
    )));
    tools(specs)
}

#[test]
fn first_capture_is_advertised_as_is() {
    let mut contract = ToolContract::default();
    let current = session_tools("The new token can be used immediately.", true);

    let advertised = contract.advertise(MODEL, WINDOW, &current, || SHORT);

    assert!(Arc::ptr_eq(&advertised, &current));
}

#[test]
fn description_revisions_keep_the_first_declaration() {
    let mut contract = ToolContract::default();
    let first = session_tools("The new token can be used immediately.", true);
    contract.advertise(MODEL, WINDOW, &first, || SHORT);

    // A load-balanced server answering from another revision must not flip the prefix.
    let revised = session_tools("If the new token is rejected, retry it shortly.", true);
    assert_eq!(contract.advertise(MODEL, WINDOW, &revised, || SHORT), first);
    assert_eq!(contract.advertise(MODEL, WINDOW, &first, || SHORT), first);
}

#[test]
fn reconnecting_server_keeps_its_tools_declared() {
    let mut contract = ToolContract::default();
    let connected = session_tools("Token.", true);
    contract.advertise(MODEL, WINDOW, &connected, || SHORT);

    let reconnecting = session_tools("Token.", false);
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &reconnecting, || SHORT),
        connected
    );
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &connected, || SHORT),
        connected
    );
}

#[test]
fn late_server_joins_while_the_conversation_is_short() {
    let mut contract = ToolContract::default();
    let starting = session_tools("Token.", false);
    contract.advertise(MODEL, WINDOW, &starting, || SHORT);

    let ready = session_tools("Token.", true);
    let extended = contract.advertise(MODEL, WINDOW, &ready, || SHORT);
    assert_eq!(
        names(&extended),
        vec![
            "exec_command",
            "mcp__codex_apps___generate_siwc_bypass_token",
            "mcp__codex_apps___edit_page",
            "update_plan",
            "mcp__omnigent__sys_os_read",
            "mcp__omnigent__sys_session_send",
        ]
    );
    // Later captures in either shape keep the extended declarations.
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &ready, || SHORT),
        extended
    );
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &starting, || SHORT),
        extended
    );
}

#[test]
fn namespace_changes_merge_per_tool() {
    let mut contract = ToolContract::default();
    let first = tools(vec![namespace(
        "mcp__codex_apps__",
        &[
            ("_edit_page", "Edit a page."),
            ("_find_pages", "Find pages."),
        ],
    )]);
    contract.advertise(MODEL, WINDOW, &first, || SHORT);

    let current = tools(vec![namespace(
        "mcp__codex_apps__",
        &[
            ("_trash_page", "Trash a page."),
            ("_edit_page", "Edit or append to a page."),
        ],
    )]);
    let advertised = contract.advertise(MODEL, WINDOW, &current, || SHORT);

    assert_eq!(
        advertised,
        tools(vec![namespace(
            "mcp__codex_apps__",
            &[
                ("_edit_page", "Edit a page."),
                ("_find_pages", "Find pages."),
                ("_trash_page", "Trash a page."),
            ],
        )])
    );
}

#[test]
fn model_change_starts_a_new_contract() {
    let mut contract = ToolContract::default();
    contract.advertise(MODEL, WINDOW, &session_tools("Token.", true), || SHORT);

    let current = session_tools("Revised token.", false);
    let advertised = contract.advertise("gpt-6.1-sol", WINDOW, &current, || SHORT);

    assert!(Arc::ptr_eq(&advertised, &current));
}

#[test]
fn new_context_window_drops_definitions_no_longer_served() {
    let mut contract = ToolContract::default();
    contract.advertise(MODEL, WINDOW, &session_tools("Token.", true), || SHORT);

    // Compaction rebuilds the prompt anyway; refresh the contract with it.
    let current = session_tools("Revised token.", false);
    let advertised = contract.advertise(MODEL, "thread:4", &current, || SHORT);

    assert!(Arc::ptr_eq(&advertised, &current));
}

/// The docs connector shape from the Lite trace: three tools published mid-thread.
fn docs_connector(published: bool) -> Arc<[ToolSpec]> {
    let mut docs = vec![
        ("_edit_page", "Edit a page."),
        ("_write_page_reference", "Write."),
    ];
    if published {
        docs.extend([
            ("_create_template", "Create a template."),
            ("_duplicate_page", "Duplicate a page."),
        ]);
    }
    tools(vec![
        ToolSpec::Function(function("exec_command", "Runs a command.")),
        namespace("mcp__codex_apps__docs", &docs),
        namespace("mcp__codex_apps__github", &[("_fetch_pr", "Fetch a PR.")]),
        ToolSpec::Function(function("get_goal", "Read the goal.")),
    ])
}

#[test]
fn tools_published_mid_thread_are_withheld_from_a_long_conversation() {
    let mut contract = ToolContract::default();
    let declared = docs_connector(false);
    contract.advertise(MODEL, WINDOW, &declared, || LONG);

    let published = docs_connector(true);
    // Any change ahead of the conversation would recompute all of it, wherever it lands.
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &published, || LONG),
        declared
    );
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &published, || LONG),
        declared
    );
}

#[test]
fn extension_stops_at_the_conversation_size_limit() {
    let mut contract = ToolContract::default();
    contract.advertise(MODEL, WINDOW, &docs_connector(false), || 0);

    let published = docs_connector(true);
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &published, || {
            TOOL_CONTRACT_EXTENSION_MAX_CONVERSATION_TOKENS + 1
        }),
        docs_connector(false)
    );

    let mut contract = ToolContract::default();
    contract.advertise(MODEL, WINDOW, &docs_connector(false), || 0);
    assert_eq!(
        names(&contract.advertise(MODEL, WINDOW, &published, || {
            TOOL_CONTRACT_EXTENSION_MAX_CONVERSATION_TOKENS
        })),
        names(&published)
    );
}

#[test]
fn conversation_size_is_only_estimated_for_new_tools() {
    let mut contract = ToolContract::default();
    let declared = docs_connector(false);
    contract.advertise(MODEL, WINDOW, &declared, || unreachable!());
    contract.advertise(MODEL, WINDOW, &declared, || unreachable!());

    let revised = tools(vec![ToolSpec::Function(function(
        "exec_command",
        "Revised.",
    ))]);
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &revised, || unreachable!()),
        declared
    );
}

#[test]
fn withheld_tools_join_with_the_next_context_window() {
    let mut contract = ToolContract::default();
    contract.advertise(MODEL, WINDOW, &docs_connector(false), || LONG);
    let published = docs_connector(true);
    contract.advertise(MODEL, WINDOW, &published, || LONG);

    // Compaction starts a new prompt, so the new window declares the live catalog.
    let advertised = contract.advertise(MODEL, "thread:4", &published, || LONG);
    assert!(Arc::ptr_eq(&advertised, &published));
}

#[test]
fn renewal_applies_one_deliberate_change() {
    let mut contract = ToolContract::default();
    let declared = docs_connector(false);
    contract.advertise(MODEL, WINDOW, &declared, || LONG);

    contract.renew("explicit MCP server reconnect");
    // An unchanged capture does not consume the renewal.
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &declared, || LONG),
        declared
    );

    let published = docs_connector(true);
    let renewed = contract.advertise(MODEL, WINDOW, &published, || LONG);
    assert!(Arc::ptr_eq(&renewed, &published));

    // Later drift is held back again.
    let reconnecting = tools(vec![ToolSpec::Function(function(
        "exec_command",
        "Runs a command.",
    ))]);
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &reconnecting, || LONG),
        published
    );
    assert_eq!(
        contract.advertise(MODEL, WINDOW, &declared, || LONG),
        published
    );
}

#[test]
fn renewal_before_the_first_capture_is_ignored() {
    let mut contract = ToolContract::default();
    contract.renew("explicit MCP server reconnect");
    let declared = docs_connector(false);
    contract.advertise(MODEL, WINDOW, &declared, || LONG);

    assert_eq!(
        contract.advertise(MODEL, WINDOW, &docs_connector(true), || LONG),
        declared
    );
}

#[test]
fn new_context_window_discards_a_pending_renewal() {
    let mut contract = ToolContract::default();
    contract.advertise(MODEL, WINDOW, &docs_connector(false), || LONG);
    contract.renew("explicit MCP server reconnect");

    let declared = docs_connector(true);
    contract.advertise(MODEL, "thread:4", &declared, || LONG);
    assert_eq!(
        contract.advertise(MODEL, "thread:4", &docs_connector(false), || LONG),
        declared
    );
}
