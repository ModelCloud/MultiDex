use super::*;

#[tokio::test]
async fn session_switches_between_omnigent_multidex_and_openai() {
    let mut session = make_session_configuration_for_tests().await;
    let mut config = (*session.original_config_do_not_use).clone();
    let mut local_provider = config.model_providers["openai"].clone();
    local_provider.name = "Omnigent MultiDex".to_string();
    local_provider.base_url = Some("http://127.0.0.1:2120/v1".to_string());
    local_provider.requires_openai_auth = false;
    config.model_providers = codex_model_provider_info::merge_configured_model_providers(
        config.model_providers,
        std::collections::HashMap::from([
            ("multidex".to_string(), local_provider.clone()),
            ("omnigent-multidex-test".to_string(), local_provider),
        ]),
    )
    .expect("MultiDex providers should normalize");
    config.model_provider_id = "omnigent-multidex-test".to_string();
    config.model_provider = config.model_providers["omnigent-multidex-test"].clone();
    session.original_config_do_not_use = Arc::new(config);
    let mut step_settings = (*session.step_settings).clone();
    step_settings.collaboration_mode.settings.model = "DS v4.1 Flash 1017".to_string();
    session.step_settings = Arc::new(step_settings);

    let official = session
        .apply(
            &SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    model: Some("gpt-6-sol".to_string()),
                    ..Default::default()
                },
                model_provider: Some("omnigent-multidex-test".to_string()),
                ..Default::default()
            },
            &[],
        )
        .expect("switch to OpenAI");
    assert_eq!(
        official.original_config_do_not_use.model_provider_id,
        "openai"
    );
    assert_eq!(
        official.provider.info(),
        &official.original_config_do_not_use.model_provider
    );

    assert!(official.provider.capabilities().namespace_tools);

    let official_with_stale_provider = official
        .apply(
            &SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    model: Some("gpt-6-sol".to_string()),
                    ..Default::default()
                },
                model_provider: Some("omnigent-multidex-test".to_string()),
                ..Default::default()
            },
            &[],
        )
        .expect("keep OpenAI when Omnigent repeats its stale provider");
    assert_eq!(
        official_with_stale_provider
            .original_config_do_not_use
            .model_provider_id,
        "openai"
    );

    let local = official_with_stale_provider
        .apply(
            &SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    model: Some("DS v4.1 Flash 1017".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            &[],
        )
        .expect("switch back to MultiDex");
    assert_eq!(
        local.original_config_do_not_use.model_provider_id,
        "multidex"
    );
    assert_eq!(
        local.provider.info(),
        &local.original_config_do_not_use.model_provider
    );

    assert!(!local.provider.capabilities().namespace_tools);

    let official_again = local
        .apply(
            &SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    model: Some("gpt-6-sol".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            &[],
        )
        .expect("switch back to OpenAI");
    assert_eq!(
        official_again.original_config_do_not_use.model_provider_id,
        "openai"
    );
    assert_eq!(
        official_again.provider.info(),
        &official_again.original_config_do_not_use.model_provider
    );

    let mut collaboration_mode = official_again.step_settings.collaboration_mode.clone();
    collaboration_mode.settings.model = "DS v4.1 Flash 1017".to_string();
    let local_from_mode = official_again
        .apply(
            &SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    collaboration_mode: Some(collaboration_mode),
                    ..Default::default()
                },
                ..Default::default()
            },
            &[],
        )
        .expect("collaboration-mode switch to MultiDex");
    assert_eq!(
        local_from_mode.original_config_do_not_use.model_provider_id,
        "multidex"
    );
}

#[tokio::test]
async fn session_switches_between_openrouter_multidex_and_openai() {
    let mut session = make_session_configuration_for_tests().await;
    let mut config = (*session.original_config_do_not_use).clone();
    config.model_providers.insert(
        "multidex".to_string(),
        ModelProviderInfo {
            name: "MultiDex".to_string(),
            base_url: Some("http://127.0.0.1:2120/v1".to_string()),
            multidex_compatibility: true,
            ..Default::default()
        },
    );
    session.original_config_do_not_use = Arc::new(config);
    for (model, explicit_provider, expected_provider) in [
        ("deepseek/deepseek-v4.1-flash-1017", None, "openrouter"),
        ("gpt-5.6-sol", None, "openai"),
        ("DS v4.1 Flash 1017", None, "multidex"),
        (
            "deepseek/deepseek-v4.1-flash-1017",
            Some("omnigent-multidex-test"),
            "openrouter",
        ),
        ("another-vendor/model", None, "openrouter"),
        ("gpt-5.6-sol", Some("openrouter"), "openai"),
    ] {
        session = session
            .apply(
                &SessionSettingsUpdate {
                    step_settings: StepSettingsUpdate {
                        model: Some(model.to_string()),
                        ..Default::default()
                    },
                    model_provider: explicit_provider.map(str::to_string),
                    ..Default::default()
                },
                &[],
            )
            .expect("switch provider");
        assert_eq!(
            session.original_config_do_not_use.model_provider_id, expected_provider,
            "{model}"
        );
        assert_eq!(
            session.provider.info(),
            &session.original_config_do_not_use.model_provider
        );
    }

    let mut mode = session.step_settings.collaboration_mode.clone();
    mode.settings.model = "deepseek/deepseek-v4.1-flash-1017".to_string();
    let from_mode = session
        .apply(
            &SessionSettingsUpdate {
                step_settings: StepSettingsUpdate {
                    collaboration_mode: Some(mode),
                    ..Default::default()
                },
                ..Default::default()
            },
            &[],
        )
        .expect("collaboration-mode switch to OpenRouter");
    assert_eq!(
        from_mode.original_config_do_not_use.model_provider_id,
        "openrouter"
    );
}
