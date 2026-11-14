//! Prepares child configuration from captured step settings and requested overrides.
//!
//! Spawn and reload share live runtime policy; role and model precedence, full-history
//! inheritance, and validation messages remain the same for each multi-agent version.

use crate::agent::role::DEFAULT_ROLE_NAME;
use crate::agent::role::apply_role_to_config;
use crate::config::Config;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_features::Feature;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::models::BaseInstructions;
use codex_protocol::openai_models::ModelOrigin;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::MultiAgentVersion;

pub(crate) const MAX_SPAWN_AGENT_MODEL_OVERRIDES: usize = 5;

pub(crate) fn model_supports_multi_agent_backend(
    model: &ModelPreset,
    multi_agent_version: MultiAgentVersion,
) -> bool {
    multi_agent_version != MultiAgentVersion::V2
        || model.multi_agent_version != Some(MultiAgentVersion::Disabled)
}

/// Selects the existing spawn tool's role-inheritance rules.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnConfigVersion {
    V1,
    V2,
}

pub(crate) struct SpawnConfigOptions<'a> {
    pub(crate) version: SpawnConfigVersion,
    pub(crate) full_history_fork: bool,
    pub(crate) role_name: Option<&'a str>,
    /// Configured `[model_providers]` id for the child; `None` inherits the caller's provider.
    pub(crate) provider: Option<&'a str>,
    pub(crate) model: Option<&'a str>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

pub(crate) struct PreparedSpawnConfig {
    pub(crate) config: Config,
    pub(crate) role_name: Option<String>,
    /// Provider-specific catalog the child must keep when spawned on a different provider.
    pub(crate) models_manager: Option<SharedModelsManager>,
}

/// Resolves child settings before starting the thread, retaining the invoking tool's precedence.
pub(crate) async fn prepare_agent_spawn_config(
    session: &Session,
    step_context: &StepContext,
    options: SpawnConfigOptions<'_>,
) -> Result<PreparedSpawnConfig, String> {
    let turn = step_context.turn.as_ref();
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, step_context)?;
    if options.version == SpawnConfigVersion::V1 && options.full_history_fork {
        reject_full_fork_agent_type_override(options.role_name)?;
    }
    let models_manager = apply_requested_spawn_agent_model_overrides(
        session,
        step_context,
        &mut config,
        options.provider,
        options.model,
        options.reasoning_effort,
    )
    .await?;
    if !options.full_history_fork
        || (options.version == SpawnConfigVersion::V2 && options.role_name.is_some())
    {
        apply_spawn_agent_role(
            session,
            models_manager.as_ref(),
            &mut config,
            options.role_name,
        )
        .await?;
        if options.version == SpawnConfigVersion::V2
            && options.full_history_fork
            && config.developer_instructions.is_none()
        {
            config
                .developer_instructions
                .clone_from(&turn.developer_instructions);
        }
    }
    apply_spawn_agent_service_tier(session, models_manager.as_ref(), &mut config).await?;
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    // Remember an applied configured default so cold reload reapplies its restrictions.
    let role_name = options
        .role_name
        .or_else(|| {
            (options.version == SpawnConfigVersion::V2
                && !options.full_history_fork
                && config
                    .agent_roles
                    .get(DEFAULT_ROLE_NAME)
                    .is_some_and(|role| role.config_file.is_some()))
            .then_some(DEFAULT_ROLE_NAME)
        })
        .map(str::to_owned);
    Ok(PreparedSpawnConfig {
        config,
        role_name,
        models_manager,
    })
}

/// Builds the base config snapshot for a newly spawned sub-agent.
///
/// The returned config starts from the parent's effective config and then refreshes the
/// model selection and reasoning settings captured for the invoking step, plus the turn's
/// runtime approval policy, sandbox, and cwd. Role-specific overrides are layered
/// after this step; skipping this helper and cloning stale config state directly can send the child
/// agent out with the wrong provider or runtime policy.
pub(crate) fn build_agent_spawn_config(
    base_instructions: &BaseInstructions,
    step_context: &StepContext,
) -> Result<Config, String> {
    let mut config = build_agent_shared_config(step_context.turn.as_ref())?;
    let settings = &step_context.settings;
    config.model = Some(settings.model_info.slug.clone());
    config.model_reasoning_effort = settings.effective_reasoning_effort();
    config.model_reasoning_summary = Some(settings.reasoning_summary);
    config.base_instructions = Some(base_instructions.text.clone());
    config.base_instructions_provenance = base_instructions.provenance.clone();
    Ok(config)
}

pub(crate) fn build_agent_resume_config(turn: &TurnContext) -> Result<Config, String> {
    let mut config = build_agent_shared_config(turn)?;
    // For resume, keep base instructions sourced from rollout/session metadata.
    config.base_instructions = None;
    config.base_instructions_provenance = None;
    Ok(config)
}

fn build_agent_shared_config(turn: &TurnContext) -> Result<Config, String> {
    let base_config = turn.config.clone();
    let mut config = (*base_config).clone();
    if config
        .features
        .enabled(Feature::SubagentDefaultContextLimits)
    {
        config.model_context_window = None;
        config.model_auto_compact_token_limit = None;
    }
    // Preserve activation for history forks without freezing the parent's model-owned prompts.
    // Fresh child startup restores configured preferences from the retained snapshot.
    config.token_budget = turn.configured_token_budget.clone();
    if let Some(thread_namespaces) = &turn.overridden_direct_only_tool_namespaces {
        config
            .code_mode
            .direct_only_tool_namespaces
            .clone_from(thread_namespaces);
    }
    config.model = Some(turn.model_info().slug.clone());
    config.model_provider = turn.provider.info().clone();
    config.model_reasoning_effort = turn
        .reasoning_effort()
        .or(turn.model_info().default_reasoning_level.as_ref())
        .cloned();
    config.model_reasoning_summary = Some(turn.reasoning_summary());
    config.developer_instructions = turn.developer_instructions.clone();
    if turn.multi_agent_version == MultiAgentVersion::V2
        && let Some(developer_instructions) = turn
            .config
            .multi_agent_v2
            .subagent_developer_instructions
            .clone()
    {
        config.developer_instructions = Some(developer_instructions);
    }
    apply_spawn_agent_runtime_overrides(&mut config, turn)?;

    Ok(config)
}

fn reject_full_fork_agent_type_override(agent_type: Option<&str>) -> Result<(), String> {
    if agent_type.is_some() {
        return Err(
            "Full-history forked agents inherit the parent agent type; omit agent_type, or spawn without a full-history fork.".to_string());
    }
    Ok(())
}

/// Copies runtime-only turn state onto a child config before it is handed to `AgentControl`.
///
/// These values are chosen by the live turn rather than persisted config, so leaving them stale can
/// make a child agent disagree with its parent about approval policy, cwd, or sandboxing.
fn apply_spawn_agent_runtime_overrides(
    config: &mut Config,
    turn: &TurnContext,
) -> Result<(), String> {
    config
        .permissions
        .approval_policy
        .set(turn.approval_policy())
        .map_err(|err| format!("approval_policy is invalid: {err}"))?;
    config.approvals_reviewer = turn.config.approvals_reviewer;
    #[allow(deprecated)]
    let turn_cwd = turn.cwd.clone();
    config.cwd = turn_cwd;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(
            turn.config
                .permissions
                .permission_profile_state()
                .snapshot(),
        )
        .map_err(|err| format!("permission_profile is invalid: {err}"))?;
    Ok(())
}

async fn apply_requested_spawn_agent_model_overrides(
    session: &Session,
    step_context: &StepContext,
    config: &mut Config,
    requested_provider: Option<&str>,
    requested_model: Option<&str>,
    requested_reasoning_effort: Option<ReasoningEffort>,
) -> Result<Option<SharedModelsManager>, String> {
    let turn = step_context.turn.as_ref();
    // The configured provider/model defaults resolve as a pair: an explicit value in either
    // field means the caller already picked a coherent pair and the configured defaults would
    // only mix in a mismatched provider or model.
    let (requested_provider, requested_model) =
        if requested_provider.is_none() && requested_model.is_none() {
            (
                turn.config.agent_default_subagent_provider.as_deref(),
                turn.config.agent_default_subagent_model.as_deref(),
            )
        } else {
            (requested_provider, requested_model)
        };
    let requested_reasoning_effort = requested_reasoning_effort
        .or_else(|| turn.config.agent_default_subagent_reasoning_effort.clone());
    if requested_provider.is_none()
        && requested_model.is_none()
        && requested_reasoning_effort.is_none()
    {
        return Ok(None);
    }

    // A provider override selects the child's endpoint before its model is resolved, so the
    // model must validate against that provider's catalog rather than the caller's manager.
    if let Some(required) = config.config_layer_stack.required_model_provider()
        && requested_provider.is_some_and(|provider_id| provider_id != required)
    {
        return Err(format!(
            "Provider override is not allowed for spawn_agent: this session requires the managed provider `{required}`"
        ));
    }
    if let Some(provider_id) = requested_provider {
        let provider_info = config.model_providers.get(provider_id).cloned().ok_or_else(|| {
            let available = config
                .model_providers
                .keys()
                .map(String::as_str)
                .take(MAX_SPAWN_AGENT_MODEL_OVERRIDES)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Unknown provider `{provider_id}` for spawn_agent. Available providers: {available}"
            )
        })?;
        config.model_provider = provider_info;
        config.model_provider_id = provider_id.to_string();
    }

    let cross_provider =
        requested_provider.is_some_and(|provider_id| provider_id != turn.config.model_provider_id);
    // A cross-provider child keeps its own manager: the shared one only serves the caller's
    // provider, and this build also carries the endpoint overlays and discovery the child needs.
    let override_models_manager = if cross_provider {
        Some(crate::thread_manager::build_models_manager(
            config,
            std::sync::Arc::clone(&session.services.auth_manager),
            /*runtime*/ None,
        ))
    } else {
        None
    };
    let models_manager: &dyn codex_models_manager::manager::ModelsManager = override_models_manager
        .as_deref()
        .unwrap_or(session.services.models_manager.as_ref());
    let refresh_strategy = if cross_provider {
        RefreshStrategy::OnlineIfUncached
    } else {
        RefreshStrategy::Offline
    };

    if let Some(requested_model) = requested_model {
        let available_models = models_manager
            .list_models(refresh_strategy, config.http_client_factory())
            .await;
        let available_models = available_models
            .into_iter()
            .filter(|model| model_available_from_provider(&model.model, &config.model_provider_id))
            .collect::<Vec<_>>();
        let selected_model_name = find_spawn_agent_model_name(
            &available_models,
            requested_model,
            turn.multi_agent_version,
        )?;
        let selected_model_info = models_manager
            .get_model_info(&selected_model_name, &config.to_models_manager_config())
            .await;

        config.model = Some(selected_model_name.clone());
        if let Some(reasoning_effort) = requested_reasoning_effort {
            validate_spawn_agent_reasoning_effort(
                &selected_model_name,
                &selected_model_info.supported_reasoning_levels,
                &reasoning_effort,
            )?;
            config.model_reasoning_effort = Some(reasoning_effort);
        } else {
            config.model_reasoning_effort = selected_model_info.default_reasoning_level.clone();
        }

        return Ok(override_models_manager);
    }

    // Provider changed without an explicit model: pick the new provider's default catalog entry.
    if cross_provider && requested_model.is_none() {
        let available_models = models_manager
            .list_models(refresh_strategy, config.http_client_factory())
            .await;
        let selected_model = available_models
            .iter()
            .find(|model| {
                model.is_default
                    && model_available_from_provider(&model.model, &config.model_provider_id)
                    && model_supports_multi_agent_backend(model, turn.multi_agent_version)
            })
            .or_else(|| {
                available_models.iter().find(|model| {
                    model.show_in_picker
                        && model_available_from_provider(&model.model, &config.model_provider_id)
                        && model_supports_multi_agent_backend(model, turn.multi_agent_version)
                })
            })
            .ok_or_else(|| {
                format!(
                    "Provider `{}` has no models usable by spawn_agent; pass `model` explicitly",
                    config.model_provider_id
                )
            })?;
        config.model = Some(selected_model.model.clone());
        config.model_reasoning_effort = None;
    }

    if let Some(reasoning_effort) = requested_reasoning_effort {
        let model_info = models_manager
            .get_model_info(
                config.model.as_deref().unwrap_or_default(),
                &config.to_models_manager_config(),
            )
            .await;
        validate_spawn_agent_reasoning_effort(
            config.model.as_deref().unwrap_or_default(),
            &model_info.supported_reasoning_levels,
            &reasoning_effort,
        )?;
        config.model_reasoning_effort = Some(reasoning_effort);
    }

    Ok(override_models_manager)
}

pub(crate) async fn apply_spawn_agent_service_tier(
    session: &Session,
    models_manager: Option<&SharedModelsManager>,
    config: &mut Config,
) -> Result<(), String> {
    let Some(service_tier) = session.services.agent_control.service_tier() else {
        config.service_tier = None;
        return Ok(());
    };
    if service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE {
        config.service_tier = Some(service_tier);
        return Ok(());
    }

    let model = config.model.clone().ok_or_else(|| {
        "spawn_agent could not resolve the child model for service tier validation".to_string()
    })?;
    let model_info = models_manager
        .map(AsRef::as_ref)
        .unwrap_or(session.services.models_manager.as_ref())
        .get_model_info(model.as_str(), &config.to_models_manager_config())
        .await;

    config.service_tier = model_info
        .supports_service_tier(service_tier.as_str())
        .then_some(service_tier);
    Ok(())
}

async fn apply_spawn_agent_role(
    session: &Session,
    models_manager: Option<&SharedModelsManager>,
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), String> {
    let previous_model = config.model.clone();
    let previous_reasoning_effort = config.model_reasoning_effort.clone();
    apply_role_to_config(config, role_name).await?;
    if config.model == previous_model && config.model_reasoning_effort == previous_reasoning_effort
    {
        return Ok(());
    }

    let Some(reasoning_effort) = config.model_reasoning_effort.clone() else {
        return Ok(());
    };
    let model = config.model.clone().ok_or_else(|| {
        "spawn_agent could not resolve the child model for reasoning effort validation".to_string()
    })?;
    let model_info = models_manager
        .map(AsRef::as_ref)
        .unwrap_or(session.services.models_manager.as_ref())
        .get_model_info(&model, &config.to_models_manager_config())
        .await;
    if model_info.origin == ModelOrigin::Synthetic {
        return Ok(());
    }

    validate_spawn_agent_reasoning_effort(
        &model,
        &model_info.supported_reasoning_levels,
        &reasoning_effort,
    )
}

fn find_spawn_agent_model_name(
    available_models: &[ModelPreset],
    requested_model: &str,
    multi_agent_version: MultiAgentVersion,
) -> Result<String, String> {
    let matches =
        ranked_spawn_model_matches(available_models, requested_model, multi_agent_version);
    if let Some((best, score)) = matches.first() {
        if matches.get(1).is_some_and(|(_, next_score)| {
            next_score == score || (*score >= 100 && *next_score <= *score + 1)
        }) {
            let choices = matches
                .iter()
                .take_while(|(_, candidate_score)| {
                    candidate_score == score || (*score >= 100 && *candidate_score <= *score + 1)
                })
                .map(|(model, _)| model.model.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "Ambiguous model `{requested_model}` for spawn_agent. Choose a full model ID: {choices}"
            ));
        }
        return Ok(best.model.clone());
    }
    Err({
        let available = available_models
            .iter()
            .filter(|model| model.show_in_picker)
            .filter(|model| model_supports_multi_agent_backend(model, multi_agent_version))
            .take(MAX_SPAWN_AGENT_MODEL_OVERRIDES)
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!("Unknown model `{requested_model}` for spawn_agent. Available models: {available}")
    })
}

/// Score names from the selected provider's current catalog. No model aliases are baked in:
/// short names are the final word of an advertised slug or display name, if unambiguous.
pub(crate) fn ranked_spawn_model_matches<'a>(
    available_models: &'a [ModelPreset],
    query: &str,
    version: MultiAgentVersion,
) -> Vec<(&'a ModelPreset, usize)> {
    let query = query.trim();
    let normalized_query = normalized_model_name(query);
    if normalized_query.is_empty() || normalized_query.len() > 128 {
        return Vec::new();
    }
    let query_words = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(normalized_model_name)
        .collect::<Vec<_>>();
    let mut matches = available_models
        .iter()
        .filter(|model| model_supports_multi_agent_backend(model, version))
        .filter_map(|model| {
            let slug = model.model.as_str();
            let display = model.display_name.as_str();
            let normalized_slug = normalized_model_name(slug);
            let normalized_display = normalized_model_name(display);
            let rank = if slug.eq_ignore_ascii_case(query) {
                Some(0)
            } else if display.eq_ignore_ascii_case(query) {
                Some(1)
            } else if normalized_slug == normalized_query || normalized_display == normalized_query
            {
                Some(2)
            } else if [slug, display].iter().any(|name| {
                last_model_word(name)
                    .is_some_and(|word| normalized_model_name(word) == normalized_query)
            }) {
                Some(3)
            } else if [normalized_slug.as_str(), normalized_display.as_str()]
                .iter()
                .any(|name| name.len() >= 5 && normalized_query.contains(name))
            {
                Some(4)
            } else if [slug, display].iter().any(|name| {
                last_model_word(name).is_some_and(|word| {
                    let tail = normalized_model_name(word);
                    tail.len() >= 3 && query_words.iter().any(|token| token == &tail)
                })
            }) {
                Some(5)
            } else if normalized_query.len() >= 5
                && [normalized_slug.as_str(), normalized_display.as_str()]
                    .iter()
                    .any(|name| name.contains(&normalized_query))
            {
                Some(6)
            } else if normalized_query.len() >= 4 {
                let distance = edit_distance(&normalized_query, &normalized_slug)
                    .min(edit_distance(&normalized_query, &normalized_display))
                    .min(last_model_word(slug).map_or(usize::MAX, |word| {
                        edit_distance(&normalized_query, &normalized_model_name(word))
                    }))
                    .min(last_model_word(display).map_or(usize::MAX, |word| {
                        edit_distance(&normalized_query, &normalized_model_name(word))
                    }))
                    .min(
                        query_words
                            .iter()
                            .filter(|token| token.len() >= 4)
                            .map(|token| {
                                [slug, display]
                                    .iter()
                                    .filter_map(|name| last_model_word(name))
                                    .map(|word| edit_distance(token, &normalized_model_name(word)))
                                    .min()
                                    .unwrap_or(usize::MAX)
                            })
                            .min()
                            .unwrap_or(usize::MAX),
                    );
                let allowed = if normalized_query.len() >= 5 { 2 } else { 1 };
                (distance <= allowed).then_some(100 + distance)
            } else {
                None
            };
            rank.map(|rank| (model, rank))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left, left_rank), (right, right_rank)| {
        left_rank
            .cmp(right_rank)
            .then_with(|| left.model.cmp(&right.model))
    });
    matches
}

fn last_model_word(name: &str) -> Option<&str> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .next_back()
}

/// Shared picker catalogs can include models for other endpoints. A child's
/// chosen provider must actually serve the matched model before it is spawned.
pub(crate) fn model_available_from_provider(model: &str, provider: &str) -> bool {
    use codex_model_provider_info as info;
    if info::is_multidex_model(model) {
        info::is_multidex_provider_id(provider)
    } else if info::is_openrouter_model(model) {
        provider == info::OPENROUTER_PROVIDER_ID
    } else if info::is_lithos_model(model) {
        provider == info::LITHOS_PROVIDER_ID
    } else if info::is_wafer_model(model) {
        provider == info::WAFER_PROVIDER_ID
    } else {
        // Other IDs may be discovered on OpenAI or a custom provider. Endpoint
        // providers only serve their own registered model IDs.
        !info::is_multidex_provider_id(provider)
            && !matches!(
                provider,
                info::OPENROUTER_PROVIDER_ID | info::LITHOS_PROVIDER_ID | info::WAFER_PROVIDER_ID
            )
    }
}

pub(crate) fn normalized_model_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.chars().count()).collect::<Vec<_>>();
    let mut current = vec![0; previous.len()];
    for (row, left_char) in left.chars().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.chars().enumerate() {
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + usize::from(left_char != right_char));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.chars().count()]
}

fn validate_spawn_agent_reasoning_effort(
    model: &str,
    supported_reasoning_levels: &[ReasoningEffortPreset],
    requested_reasoning_effort: &ReasoningEffort,
) -> Result<(), String> {
    if supported_reasoning_levels
        .iter()
        .any(|preset| &preset.effort == requested_reasoning_effort)
    {
        return Ok(());
    }

    let supported = supported_reasoning_levels
        .iter()
        .map(|preset| preset.effort.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "Reasoning effort `{requested_reasoning_effort}` is not supported for model `{model}`. Supported reasoning efforts: {supported}"
    ))
}

#[cfg(test)]
mod model_alias_tests {
    use super::*;

    #[test]
    fn short_names_come_from_the_selected_provider_catalog() {
        let model = codex_models_manager::model_info::model_info_from_slug("gpt-6-astra");
        let available = vec![ModelPreset::from(model)];
        assert_eq!(
            find_spawn_agent_model_name(&available, "astra", MultiAgentVersion::V2).unwrap(),
            "gpt-6-astra"
        );
        assert_eq!(
            find_spawn_agent_model_name(&available, "ASTRA", MultiAgentVersion::V2).unwrap(),
            "gpt-6-astra"
        );
        assert!(find_spawn_agent_model_name(&[], "astra", MultiAgentVersion::V2).is_err());
        assert_eq!(
            find_spawn_agent_model_name(&available, "gpt 6 astra", MultiAgentVersion::V2).unwrap(),
            "gpt-6-astra"
        );
        assert_eq!(
            find_spawn_agent_model_name(&available, "gpt-6-astr", MultiAgentVersion::V2).unwrap(),
            "gpt-6-astra"
        );
        assert_eq!(
            find_spawn_agent_model_name(
                &available,
                "review with atsra/high",
                MultiAgentVersion::V2
            )
            .unwrap(),
            "gpt-6-astra"
        );
    }

    #[test]
    fn ambiguous_short_names_do_not_pick_an_arbitrary_model() {
        let mut first = ModelPreset::from(codex_models_manager::model_info::model_info_from_slug(
            "gpt-6-astra",
        ));
        let mut second = first.clone();
        first.model = "vendor-a/astra".to_string();
        second.model = "vendor-b/astra".to_string();
        let error = find_spawn_agent_model_name(&[first, second], "astra", MultiAgentVersion::V2)
            .unwrap_err();
        assert!(error.contains("Ambiguous model"));
    }
}
