//! Resolve a user's approximate model name against the configured provider catalogs.

use super::*;
use crate::agent::child_config::edit_distance;
use crate::agent::child_config::model_available_from_provider;
use crate::agent::child_config::model_supports_multi_agent_backend;
use crate::agent::child_config::normalized_model_name;
use crate::agent::child_config::ranked_spawn_model_matches;
use crate::tools::context::FunctionToolOutput;
use crate::tools::handlers::multi_agents_spec::create_resolve_model_tool;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::openai_models::ModelPreset;
use codex_tools::ToolSpec;

pub(crate) struct Handler;

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("resolve_model")
    }

    fn spec(&self) -> ToolSpec {
        create_resolve_model_tool()
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let arguments = function_arguments(invocation.payload)?;
            let args: ResolveModelArgs = parse_arguments(&arguments)?;
            let turn = &invocation.step_context.turn;
            let config = &turn.config;
            if let Some(provider) = args.provider.as_deref()
                && !config.model_providers.contains_key(provider)
            {
                return Err(FunctionCallError::RespondToModel(format!(
                    "Unknown provider `{provider}` for resolve_model"
                )));
            }

            let mut providers = config
                .model_providers
                .keys()
                .filter(|id| {
                    args.provider
                        .as_deref()
                        .is_none_or(|requested| requested == *id)
                })
                .filter(|id| {
                    args.provider.is_some()
                        || *id == &config.model_provider_id
                        || codex_model_provider_info::is_multidex_provider_id(id)
                        || *id == codex_model_provider_info::OPENAI_PROVIDER_ID
                        || matches!(
                            id.as_str(),
                            codex_model_provider_info::OPENROUTER_PROVIDER_ID
                                | codex_model_provider_info::LITHOS_PROVIDER_ID
                                | codex_model_provider_info::WAFER_PROVIDER_ID
                        )
                        || !codex_model_provider_info::built_in_model_providers(None)
                            .contains_key(*id)
                })
                .cloned()
                .collect::<Vec<_>>();
            providers.sort();

            let mut available = Vec::<ModelChoice>::new();
            for provider_id in providers {
                let mut provider_config = (**config).clone();
                let Some(provider_info) = config.model_providers.get(&provider_id) else {
                    continue;
                };
                provider_config.model_provider = provider_info.clone();
                provider_config.model_provider_id = provider_id.clone();
                let models = if provider_id == config.model_provider_id {
                    turn.available_models.clone()
                } else {
                    let manager = crate::thread_manager::build_models_manager(
                        &provider_config,
                        std::sync::Arc::clone(&invocation.session.services.auth_manager),
                        /*runtime*/ None,
                    );
                    manager
                        .list_models(
                            RefreshStrategy::Offline,
                            provider_config.http_client_factory(),
                        )
                        .await
                };
                for model in models {
                    if model_supports_multi_agent_backend(&model, turn.multi_agent_version)
                        && model_available_from_provider(&model.model, &provider_id)
                        && !available.iter().any(|choice| {
                            choice.provider == provider_id && choice.model.model == model.model
                        })
                    {
                        available.push(ModelChoice {
                            provider: provider_id.clone(),
                            model,
                        });
                    }
                }
            }

            let result =
                resolve_candidates(&available, args.query.as_deref(), turn.multi_agent_version);
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                serde_json::to_string(&result).map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "could not encode resolve_model result: {err}"
                    ))
                })?,
                Some(true),
            )))
        })
    }
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveModelArgs {
    query: Option<String>,
    provider: Option<String>,
}

struct ModelChoice {
    provider: String,
    model: ModelPreset,
}

#[derive(Serialize)]
struct ResolvedChoice {
    provider: String,
    model: String,
    display_name: String,
    reasoning_efforts: Vec<String>,
}

#[derive(Serialize)]
struct ResolveModelResult {
    status: &'static str,
    choices: Vec<ResolvedChoice>,
    total: usize,
}

fn resolve_candidates(
    choices: &[ModelChoice],
    query: Option<&str>,
    version: codex_protocol::protocol::MultiAgentVersion,
) -> ResolveModelResult {
    let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
        let all = choices.iter().map(ResolvedChoice::from).collect();
        return ResolveModelResult {
            status: "catalog",
            choices: all,
            total: choices.len(),
        };
    };
    if query.chars().count() > 128 {
        return ResolveModelResult {
            status: "invalid_query",
            choices: Vec::new(),
            total: 0,
        };
    }
    let mut matches = choices
        .iter()
        .filter_map(|choice| {
            ranked_spawn_model_matches(std::slice::from_ref(&choice.model), query, version)
                .first()
                .map(|(_, score)| (choice, *score))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left, left_score), (right, right_score)| {
        left_score
            .cmp(right_score)
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.model.model.cmp(&right.model.model))
    });
    if let Some((_, best_score)) = matches.first() {
        let best = matches
            .iter()
            .take_while(|(_, score)| {
                score == best_score || (*best_score >= 100 && *score <= *best_score + 1)
            })
            .map(|(choice, _)| ResolvedChoice::from(*choice))
            .collect::<Vec<_>>();
        return ResolveModelResult {
            status: if best.len() == 1 {
                "match"
            } else {
                "ambiguous"
            },
            total: best.len(),
            choices: best,
        };
    }

    let normalized_query = normalized_model_name(query);
    let mut nearest = choices
        .iter()
        .map(|choice| {
            let distance = edit_distance(
                &normalized_query,
                &normalized_model_name(&choice.model.model),
            )
            .min(edit_distance(
                &normalized_query,
                &normalized_model_name(&choice.model.display_name),
            ));
            (choice, distance)
        })
        .collect::<Vec<_>>();
    nearest.sort_by(|(left, left_score), (right, right_score)| {
        left_score
            .cmp(right_score)
            .then_with(|| left.provider.cmp(&right.provider))
    });
    ResolveModelResult {
        status: "not_found",
        total: choices.len(),
        choices: nearest
            .into_iter()
            .take(5)
            .map(|(choice, _)| ResolvedChoice::from(choice))
            .collect(),
    }
}

impl From<&ModelChoice> for ResolvedChoice {
    fn from(choice: &ModelChoice) -> Self {
        Self {
            provider: choice.provider.clone(),
            model: choice.model.model.clone(),
            display_name: choice.model.display_name.clone(),
            reasoning_efforts: choice
                .model
                .supported_reasoning_efforts
                .iter()
                .map(|effort| effort.effort.to_string())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::MultiAgentVersion;

    #[test]
    fn live_catalog_resolves_short_names_without_model_specific_aliases() {
        let model = ModelPreset::from(codex_models_manager::model_info::model_info_from_slug(
            "gpt-6-astra",
        ));
        let choices = vec![ModelChoice {
            provider: "openai".into(),
            model,
        }];
        let result = resolve_candidates(&choices, Some("Astra"), MultiAgentVersion::V2);
        assert_eq!(result.status, "match");
        assert_eq!(result.choices[0].model, "gpt-6-astra");
        assert_eq!(result.choices[0].provider, "openai");
    }

    #[test]
    fn dynamic_aliases_and_typos_resolve_only_when_unambiguous() {
        let mut model = ModelPreset::from(codex_models_manager::model_info::model_info_from_slug(
            "gpt-6-astra",
        ));
        model.model = "vendor/new-frontier".into();
        model.display_name = "New Frontier".into();
        let one = vec![ModelChoice {
            provider: "custom".into(),
            model: model.clone(),
        }];
        let resolved = resolve_candidates(&one, Some("frontir"), MultiAgentVersion::V2);
        assert_eq!(resolved.status, "match");
        assert_eq!(resolved.choices[0].model, "vendor/new-frontier");

        let two = vec![
            ModelChoice {
                provider: "custom".into(),
                model: model.clone(),
            },
            ModelChoice {
                provider: "other".into(),
                model,
            },
        ];
        let ambiguous = resolve_candidates(&two, Some("frontir"), MultiAgentVersion::V2);
        assert_eq!(ambiguous.status, "ambiguous");
        assert_eq!(ambiguous.total, 2);
    }

    #[test]
    fn openai_models_are_not_claimed_by_the_local_provider() {
        assert!(!model_available_from_provider("gpt-6-astra", "multidex"));
        assert!(model_available_from_provider("gpt-6-astra", "openai"));
    }
}
