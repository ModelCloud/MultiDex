use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelMessages;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::openai_models::TruncationMode;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;
use codex_protocol::openai_models::default_input_modalities;
use serde::Deserialize;

use crate::config::ModelsManagerConfig;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use std::path::PathBuf;
use tracing::warn;

pub const BASE_INSTRUCTIONS: &str = include_str!("../prompt.md");
/// The DeepSeek Harness system prompt, ported to MultiDex's tool surface
/// (exec_command/write_stdin/apply_patch/update_plan/web_search). DS v4.1
/// Flash is served with this prompt instead of the Codex one.
pub const DEEPSEEK_INSTRUCTIONS: &str = include_str!("../deepseek_prompt.md");
const PERSONALITY_SECTION_HEADER: &str = "# Personality";
const MULTIDEX_DSV41_FLASH: &str = "DS v4.1 Flash 1017";
/// The same DS v4.1 Flash model routed through OpenRouter's aggregate API.
const OPENROUTER_DSV41_FLASH: &str = "deepseek/deepseek-v4.1-flash-1017";
const WAFER_DSV41_FLASH: &str = "DeepSeek-V4.1-Flash-1017";
const LITHOS_DSV41_FLASH: [&str; 3] = [
    "deepseek-ai/DeepSeek-V4.1-Flash",
    "deepseek-ai/DeepSeek-V4.1-Flash-fast",
    "deepseek-ai/DeepSeek-V4.1-Flash-ultra",
];
const MULTIDEX_DSV41_FLASH_CONTEXT_WINDOW: i64 = 262_144;
const MULTIDEX_DSV41_FLASH_COMPACTION_HEADROOM: i64 = 4_196;
const MULTIDEX_DSV41_FLASH_AUTO_COMPACT_TOKEN_LIMIT: i64 =
    MULTIDEX_DSV41_FLASH_CONTEXT_WINDOW - MULTIDEX_DSV41_FLASH_COMPACTION_HEADROOM;
const MULTIDEX_RUNTIME_CAPABILITIES_FILE: &str = "multidex-runtime-capabilities.json";

#[derive(Deserialize)]
struct MultiDexRuntimeCapabilities {
    model: String,
    context_window: i64,
    #[serde(default)]
    max_prompt_tokens: Option<i64>,
}

pub fn with_config_overrides(mut model: ModelInfo, config: &ModelsManagerConfig) -> ModelInfo {
    if let Some(context_window) = config.model_context_window {
        model.context_window = Some(
            model
                .max_context_window
                .map_or(context_window, |max_context_window| {
                    context_window.min(max_context_window)
                }),
        );
    }
    if let Some(auto_compact_token_limit) = config.model_auto_compact_token_limit {
        model.auto_compact_token_limit = Some(auto_compact_token_limit);
    }
    if let Some(token_limit) = config.tool_output_token_limit {
        model.truncation_policy = match model.truncation_policy.mode {
            TruncationMode::Bytes => {
                let byte_limit =
                    i64::try_from(approx_bytes_for_tokens(token_limit)).unwrap_or(i64::MAX);
                TruncationPolicyConfig::bytes(byte_limit)
            }
            TruncationMode::Tokens => {
                let limit = i64::try_from(token_limit).unwrap_or(i64::MAX);
                TruncationPolicyConfig::tokens(limit)
            }
        };
    }

    if let Some(base_instructions) = &config.base_instructions {
        let model_messages = model.model_messages.get_or_insert_default();
        model_messages.instructions_template = Some(base_instructions.clone());
    } else if config.personality == Some(Personality::None)
        && let Some(instructions_template) = model
            .model_messages
            .as_mut()
            .and_then(|messages| messages.instructions_template.as_mut())
    {
        *instructions_template = strip_personality_section(std::mem::take(instructions_template));
    }

    apply_multidex_runtime_capabilities(model)
}

/// Use the local endpoint's live budget for every DeepSeek provider variant.
/// The snapshot comes from the local `/v1/models/{id}` endpoint. Keep the
/// bundled local budget when discovery fails; never inherit a hosted
/// provider's larger published window during a provider switch.
fn apply_multidex_runtime_capabilities(model: ModelInfo) -> ModelInfo {
    if !is_dsv41_flash_model(&model.slug) {
        return model;
    }
    let Some(codex_home) = std::env::var_os("CODEX_HOME") else {
        return model;
    };
    let path = PathBuf::from(codex_home).join(MULTIDEX_RUNTIME_CAPABILITIES_FILE);
    let Ok(contents) = std::fs::read_to_string(path) else {
        return model;
    };
    let Some((context_window, max_prompt_tokens)) = multidex_runtime_context_budget(&contents)
    else {
        return model;
    };
    apply_local_context_budget(model, context_window, max_prompt_tokens)
}

fn apply_local_context_budget(
    mut model: ModelInfo,
    context_window: i64,
    max_prompt_tokens: i64,
) -> ModelInfo {
    model.context_window = Some(context_window);
    model.max_context_window = None;
    model.auto_compact_token_limit =
        Some(max_prompt_tokens.saturating_sub(MULTIDEX_DSV41_FLASH_COMPACTION_HEADROOM));
    model
}

fn is_dsv41_flash_model(slug: &str) -> bool {
    slug == MULTIDEX_DSV41_FLASH
        || slug == OPENROUTER_DSV41_FLASH
        || slug == WAFER_DSV41_FLASH
        || LITHOS_DSV41_FLASH.contains(&slug)
}

fn multidex_runtime_context_budget(contents: &str) -> Option<(i64, i64)> {
    let capabilities = serde_json::from_str::<MultiDexRuntimeCapabilities>(contents).ok()?;
    if capabilities.model != MULTIDEX_DSV41_FLASH || capabilities.context_window <= 0 {
        return None;
    }
    let max_prompt_tokens = capabilities
        .max_prompt_tokens
        .filter(|tokens| *tokens > 0)
        .unwrap_or(capabilities.context_window)
        .min(capabilities.context_window);
    Some((capabilities.context_window, max_prompt_tokens))
}

fn strip_personality_section(mut instructions: String) -> String {
    let mut section_start = None;
    let mut section_end = None;
    let mut offset = 0;

    for line_with_ending in instructions.split_inclusive('\n') {
        let line = match line_with_ending.strip_suffix('\n') {
            Some(line) => line.strip_suffix('\r').unwrap_or(line),
            None => line_with_ending,
        };
        if section_start.is_some() {
            if is_h1_heading(line) {
                section_end = Some(offset);
                break;
            }
        } else if line == PERSONALITY_SECTION_HEADER {
            section_start = Some(offset);
        }
        offset += line_with_ending.len();
    }

    if let Some(section_start) = section_start {
        let section_end = section_end.unwrap_or(instructions.len());
        instructions.replace_range(section_start..section_end, "");
    }

    instructions
}

fn is_h1_heading(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('#') else {
        return false;
    };
    rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')
}

/// Build a minimal fallback model descriptor for missing/unknown slugs.
pub fn model_info_from_slug(slug: &str) -> ModelInfo {
    if slug == MULTIDEX_DSV41_FLASH {
        return apply_multidex_runtime_capabilities(multidex_dsv41_flash_model_info());
    }
    if slug == OPENROUTER_DSV41_FLASH {
        return apply_multidex_runtime_capabilities(openrouter_dsv41_flash_model_info());
    }
    if slug == WAFER_DSV41_FLASH {
        return apply_multidex_runtime_capabilities(wafer_dsv41_flash_model_info());
    }
    if LITHOS_DSV41_FLASH.contains(&slug) {
        return apply_multidex_runtime_capabilities(lithos_dsv41_flash_model_info(slug));
    }
    warn!("Unknown model {slug} is used. This will use fallback model metadata.");
    ModelInfo {
        slug: slug.to_string(),
        display_name: slug.to_string(),
        description: None,
        default_reasoning_level: None,
        supported_reasoning_levels: Vec::new(),
        shell_type: ConfigShellToolType::UnifiedExec,
        visibility: ModelVisibility::None,
        supported_in_api: true,
        priority: 99,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        available_access_programs: None,
        availability_nux: None,
        upgrade: None,
        model_messages: Some(local_model_messages()),
        include_skills_usage_instructions: false,
        include_plugin_usage_instructions: false,
        include_apps_usage_instructions: false,
        supports_reasoning_summary_parameter: true,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        web_search_provider: None,
        truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
        supports_image_detail_original: false,
        context_window: Some(272_000),
        max_context_window: Some(272_000),
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: default_input_modalities(),
        supports_search_tool: false,
        supports_experimental_context: false,
        use_responses_lite: false,
        supports_reasoning_effort_updates: false,
        guardian: None,
        node_repl_auto_review_required: false,
        node_repl_disabled: false,
        auto_review_model_override: None,
        model_specialty: None,
        tool_mode: None,
        multi_agent_version: None,
        multi_agent_reasoning_effort: None,
    }
}

/// Return the upstream catalog plus the bundled MultiDex model.
///
/// A MultiDex provider must expose both its configured local model and the
/// ordinary Codex catalog from one `/model` picker. `model_catalog_json` is an
/// authoritative replacement, so callers use this only when the user has not
/// configured one explicitly.
pub fn multidex_model_catalog() -> ModelsResponse {
    let mut catalog = crate::bundled_models_response().unwrap_or_default();
    catalog.models.retain(|model| {
        model.slug != MULTIDEX_DSV41_FLASH
            && model.slug != OPENROUTER_DSV41_FLASH
            && model.slug != WAFER_DSV41_FLASH
            && !LITHOS_DSV41_FLASH.contains(&model.slug.as_str())
    });
    catalog.models.push(multidex_dsv41_flash_model_info());
    catalog.models.push(wafer_dsv41_flash_model_info());
    catalog.models.push(openrouter_dsv41_flash_model_info());
    catalog
        .models
        .extend(LITHOS_DSV41_FLASH.map(lithos_dsv41_flash_model_info));
    for model in &mut catalog.models {
        if is_dsv41_flash_model(&model.slug) {
            *model = apply_multidex_runtime_capabilities(model.clone());
        }
    }
    catalog
}

pub fn is_bundled_openai_model(model: &str) -> bool {
    crate::bundled_models_response()
        .is_ok_and(|catalog| catalog.models.iter().any(|info| info.slug == model))
}

/// Metadata for the MultiDex provider's bundled DeepSeek endpoint.
///
/// This is intentionally part of MultiDex rather than a `model_catalog_json`
/// overlay: Codex treats that setting as a replacement for its full catalog,
/// which conflicts with the upstream ChatGPT model catalog and configured
/// provider routing. The inference server advertises matching capabilities
/// through its OpenAI-compatible model-discovery endpoint.
fn multidex_dsv41_flash_model_info() -> ModelInfo {
    dsv41_flash_model_info(
        MULTIDEX_DSV41_FLASH,
        "DeepSeek v4.1 Flash (Local)",
        "DeepSeek v4.1 Flash 1017 served by the local endpoint.",
        /*default_reasoning_level*/ ReasoningEffort::High,
        /*supports_reasoning_summary_parameter*/ false,
        /*default_reasoning_summary*/ ReasoningSummary::None,
        /*priority*/ 0,
    )
}

/// The same model routed through the built-in OpenRouter provider.
fn openrouter_dsv41_flash_model_info() -> ModelInfo {
    dsv41_flash_model_info(
        OPENROUTER_DSV41_FLASH,
        "DeepSeek v4.1 Flash (OpenRouter)",
        "DS v4.1 Flash 1017 coding model served through OpenRouter.",
        // Matches the MultiDex Lite entry's default reasoning effort.
        /*default_reasoning_level*/
        ReasoningEffort::Max,
        // OpenRouter normalizes reasoning into a distinct stream rather than
        // mirroring raw thinking text, so the summary parameter is safe.
        /*supports_reasoning_summary_parameter*/
        true,
        /*default_reasoning_summary*/ ReasoningSummary::Auto,
        /*priority*/ 2,
    )
}

fn wafer_dsv41_flash_model_info() -> ModelInfo {
    dsv41_flash_model_info(
        WAFER_DSV41_FLASH,
        "DeepSeek v4.1 Flash (Wafer)",
        "DeepSeek v4.1 Flash 1017 standard model served by Wafer US.",
        ReasoningEffort::High,
        false,
        ReasoningSummary::None,
        1,
    )
}

fn lithos_dsv41_flash_model_info(slug: &str) -> ModelInfo {
    let variant = match slug {
        "deepseek-ai/DeepSeek-V4.1-Flash-fast" => "Fast",
        "deepseek-ai/DeepSeek-V4.1-Flash-ultra" => "Ultra",
        _ => "Regular",
    };
    dsv41_flash_model_info(
        slug,
        &format!("DeepSeek v4.1 Flash (Lithos {variant})"),
        "DeepSeek v4.1 Flash 1017 served by Lithos.",
        ReasoningEffort::Max,
        false,
        ReasoningSummary::None,
        3,
    )
}

#[allow(clippy::too_many_arguments)]
fn dsv41_flash_model_info(
    slug: &str,
    display_name: &str,
    description: &str,
    default_reasoning_level: ReasoningEffort,
    supports_reasoning_summary_parameter: bool,
    default_reasoning_summary: ReasoningSummary,
    priority: i32,
) -> ModelInfo {
    ModelInfo {
        slug: slug.to_string(),
        display_name: display_name.to_string(),
        description: Some(description.to_string()),
        // DSV4.1 accepts `off`, `low`, `high`, `max`, or a numeric budget. Its
        // OpenAI-compatible endpoint explicitly rejects Codex's generic
        // `medium` default; the local model defaults to high reasoning.
        default_reasoning_level: Some(default_reasoning_level),
        supported_reasoning_levels: vec![
            ReasoningEffortPreset {
                effort: ReasoningEffort::Custom("off".to_string()),
                description: "Disable thinking for the fastest responses".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Low,
                description: "Fast responses with lighter reasoning".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::High,
                description: "Greater reasoning depth for complex work".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffort::Max,
                description: "Maximum reasoning depth for difficult work".to_string(),
            },
        ],
        shell_type: ConfigShellToolType::UnifiedExec,
        // These are configured provider models, not hidden fallbacks. The
        // local model leads the picker: the lowest priority sorts first,
        // which also makes it the default when this catalog is in use.
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        available_access_programs: None,
        availability_nux: None,
        upgrade: None,
        model_messages: Some(dsv41_model_messages()),
        // MultiDex uses the same skill catalog and bridge as Codex. Include
        // the usage protocol so a local model knows to inspect a selected
        // SKILL.md before it acts on that skill.
        include_skills_usage_instructions: true,
        include_plugin_usage_instructions: false,
        include_apps_usage_instructions: false,
        // The local endpoint streams raw reasoning text. It does not expose a
        // distinct reasoning-summary stream, so never ask it for one there: some
        // OpenAI-compatible servers mirror a requested summary alongside the
        // raw stream, which duplicates/interleaves visible thinking.
        supports_reasoning_summary_parameter,
        default_reasoning_summary,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
        web_search_tool_type: WebSearchToolType::Text,
        web_search_provider: None,
        truncation_policy: TruncationPolicyConfig::tokens(/*limit*/ 10_000),
        supports_image_detail_original: false,
        // Conservative fallback shared by every DeepSeek provider. A live
        // local endpoint snapshot replaces the context window and sets the
        // compact threshold below its separate max_prompt_tokens ceiling.
        context_window: Some(MULTIDEX_DSV41_FLASH_CONTEXT_WINDOW),
        // A live endpoint capability snapshot may raise or lower this value;
        // do not cap that authoritative result to the bundled fallback.
        max_context_window: None,
        auto_compact_token_limit: Some(MULTIDEX_DSV41_FLASH_AUTO_COMPACT_TOKEN_LIMIT),
        comp_hash: None,
        effective_context_window_percent: 100,
        experimental_supported_tools: Vec::new(),
        input_modalities: vec![InputModality::Text],
        supports_search_tool: false,
        supports_experimental_context: false,
        use_responses_lite: false,
        // The configured OpenAI-compatible endpoint does not support changing
        // reasoning effort after a turn has started.
        supports_reasoning_effort_updates: false,
        guardian: None,
        node_repl_auto_review_required: false,
        node_repl_disabled: false,
        auto_review_model_override: None,
        model_specialty: None,
        tool_mode: Some(ToolMode::Direct),
        multi_agent_version: None,
        multi_agent_reasoning_effort: None,
    }
}

fn local_model_messages() -> ModelMessages {
    ModelMessages {
        instructions_template: Some(BASE_INSTRUCTIONS.to_string()),
        ..Default::default()
    }
}

/// DS v4.1 Flash runs with the DeepSeek Harness system prompt: the model's
/// own prompt is shorter and better tuned than the generic Codex one.
fn dsv41_model_messages() -> ModelMessages {
    ModelMessages {
        instructions_template: Some(DEEPSEEK_INSTRUCTIONS.to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "model_info_tests.rs"]
mod tests;
