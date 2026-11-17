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

    let advertised = contract.advertise(MODEL, WINDOW, &current);

    assert!(Arc::ptr_eq(&advertised, &current));
}

#[test]
fn description_revisions_keep_the_first_declaration() {
    let mut contract = ToolContract::default();
    let first = session_tools("The new token can be used immediately.", true);
    contract.advertise(MODEL, WINDOW, &first);

    // A load-balanced server answering from another revision must not flip the prefix.
    let revised = session_tools("If the new token is rejected, retry it shortly.", true);
    assert_eq!(contract.advertise(MODEL, WINDOW, &revised), first);
    assert_eq!(contract.advertise(MODEL, WINDOW, &first), first);
}

#[test]
fn reconnecting_server_keeps_its_tools_declared() {
    let mut contract = ToolContract::default();
    let connected = session_tools("Token.", true);
    contract.advertise(MODEL, WINDOW, &connected);

    let reconnecting = session_tools("Token.", false);
    assert_eq!(contract.advertise(MODEL, WINDOW, &reconnecting), connected);
    assert_eq!(contract.advertise(MODEL, WINDOW, &connected), connected);
}

#[test]
fn late_server_extends_the_contract_once_after_declared_tools() {
    let mut contract = ToolContract::default();
    let starting = session_tools("Token.", false);
    contract.advertise(MODEL, WINDOW, &starting);

    let ready = session_tools("Token.", true);
    let extended = contract.advertise(MODEL, WINDOW, &ready);
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
    assert_eq!(contract.advertise(MODEL, WINDOW, &ready), extended);
    assert_eq!(contract.advertise(MODEL, WINDOW, &starting), extended);
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
    contract.advertise(MODEL, WINDOW, &first);

    let current = tools(vec![namespace(
        "mcp__codex_apps__",
        &[
            ("_trash_page", "Trash a page."),
            ("_edit_page", "Edit or append to a page."),
        ],
    )]);
    let advertised = contract.advertise(MODEL, WINDOW, &current);

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
    contract.advertise(MODEL, WINDOW, &session_tools("Token.", true));

    let current = session_tools("Revised token.", false);
    let advertised = contract.advertise("gpt-6.1-sol", WINDOW, &current);

    assert!(Arc::ptr_eq(&advertised, &current));
}

#[test]
fn new_context_window_drops_definitions_no_longer_served() {
    let mut contract = ToolContract::default();
    contract.advertise(MODEL, WINDOW, &session_tools("Token.", true));

    // Compaction rebuilds the prompt anyway; refresh the contract with it.
    let current = session_tools("Revised token.", false);
    let advertised = contract.advertise(MODEL, "thread:4", &current);

    assert!(Arc::ptr_eq(&advertised, &current));
}
