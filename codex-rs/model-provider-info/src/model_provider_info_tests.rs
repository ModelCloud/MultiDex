use super::*;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::AbsolutePathBufGuard;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;
use tempfile::tempdir;

#[test]
fn runtime_internal_metadata_opt_in_is_not_serialized_or_configurable() {
    let trusted = ModelProviderInfo {
        include_internal_metadata: true,
        provider: None,
        ..Default::default()
    };
    assert_eq!(
        toml::to_string(&trusted).unwrap(),
        toml::to_string(&ModelProviderInfo::default()).unwrap(),
    );
    assert_eq!(
        toml::from_str::<ModelProviderInfo>("include_internal_metadata = true").unwrap(),
        ModelProviderInfo::default(),
    );
}

#[test]
fn test_api_provider_applies_current_managed_residency() {
    let info = ModelProviderInfo {
        http_headers: Some(maplit::hashmap! {
            "X-OpenAI-Internal-Codex-Residency".to_string() => "eu".into(),
            "x-provider-header".to_string() => "preserved".into(),
        }),
        ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
    };
    let original_info = info.clone();
    let previous_requirement = read_managed_residency_requirement();
    set_managed_residency_requirement(Some(ResidencyRequirement::Us));
    let managed = info.to_api_provider(/*auth_mode*/ None);
    set_managed_residency_requirement(/*enforce_residency*/ None);
    let unmanaged = info.to_api_provider(/*auth_mode*/ None);
    set_managed_residency_requirement(previous_requirement);

    assert_eq!(
        managed.expect("managed provider should resolve").headers,
        HeaderMap::from_iter([
            (
                HeaderName::from_static(RESIDENCY_HEADER_NAME),
                HeaderValue::from_static("us")
            ),
            (
                HeaderName::from_static("x-provider-header"),
                HeaderValue::from_static("preserved")
            ),
        ])
    );
    assert_eq!(
        unmanaged
            .expect("unmanaged provider should resolve")
            .headers,
        HeaderMap::from_iter([
            (
                HeaderName::from_static(RESIDENCY_HEADER_NAME),
                HeaderValue::from_static("eu")
            ),
            (
                HeaderName::from_static("x-provider-header"),
                HeaderValue::from_static("preserved")
            ),
        ])
    );
    assert_eq!(info, original_info);
}

#[test]
fn test_deserialize_ollama_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Ollama"
base_url = "http://localhost:11434/v1"
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Ollama".into(),
        base_url: Some("http://localhost:11434/v1".into()),
        model_catalog_url: None,
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        gateway_oauth: None,
        aws: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        server_overloaded_retry_delay_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_responses_continuation: false,
        multidex_compatibility: false,
        supports_standalone_web_search: false,
        capabilities: None,
        project_id: None,
        location: None,
        credential_file: None,
        include_internal_metadata: false,
        provider: None,
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_azure_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Azure"
base_url = "https://xxxxx.openai.azure.com/openai"
env_key = "AZURE_OPENAI_API_KEY"
query_params = { api-version = "2025-04-01-preview" }
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Azure".into(),
        base_url: Some("https://xxxxx.openai.azure.com/openai".into()),
        model_catalog_url: None,
        env_key: Some("AZURE_OPENAI_API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        gateway_oauth: None,
        aws: None,
        wire_api: WireApi::Responses,
        query_params: Some(maplit::hashmap! {
            "api-version".to_string() => "2025-04-01-preview".into(),
        }),
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        server_overloaded_retry_delay_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_responses_continuation: false,
        multidex_compatibility: false,
        supports_standalone_web_search: false,
        capabilities: None,
        project_id: None,
        location: None,
        credential_file: None,
        include_internal_metadata: false,
        provider: None,
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_example_model_provider_toml() {
    let azure_provider_toml = r#"
name = "Example"
base_url = "https://example.com"
env_key = "API_KEY"
http_headers = { "X-Example-Header" = "example-value" }
env_http_headers = { "X-Example-Env-Header" = "EXAMPLE_ENV_VAR" }
supports_standalone_web_search = true
        "#;
    let expected_provider = ModelProviderInfo {
        name: "Example".into(),
        base_url: Some("https://example.com".into()),
        model_catalog_url: None,
        env_key: Some("API_KEY".into()),
        env_key_instructions: None,
        experimental_bearer_token: None,
        auth: None,
        gateway_oauth: None,
        aws: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: Some(maplit::hashmap! {
            "X-Example-Header".to_string() => "example-value".into(),
        }),
        env_http_headers: Some(maplit::hashmap! {
            "X-Example-Env-Header".to_string() => "EXAMPLE_ENV_VAR".to_string(),
        }),
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        websocket_connect_timeout_ms: None,
        server_overloaded_retry_delay_ms: None,
        requires_openai_auth: false,
        supports_websockets: false,
        supports_responses_continuation: false,
        multidex_compatibility: false,
        supports_standalone_web_search: true,
        capabilities: None,
        project_id: None,
        location: None,
        credential_file: None,
        include_internal_metadata: false,
        provider: None,
    };

    let provider: ModelProviderInfo = toml::from_str(azure_provider_toml).unwrap();
    assert_eq!(expected_provider, provider);
}

#[test]
fn test_deserialize_chat_wire_api_shows_helpful_error() {
    let provider_toml = r#"
name = "OpenAI using Chat Completions"
base_url = "https://api.openai.com/v1"
env_key = "OPENAI_API_KEY"
wire_api = "chat"
        "#;

    let err = toml::from_str::<ModelProviderInfo>(provider_toml).unwrap_err();
    assert!(err.to_string().contains(CHAT_WIRE_API_REMOVED_ERROR));
}

#[test]
fn server_overloaded_retry_delay_prefers_field_then_env_then_default() {
    // SAFETY: single-threaded mutation of this var; restored before returning.
    unsafe { std::env::set_var("CODEX_SERVER_OVERLOADED_RETRY_DELAY_MS", "75") };
    let configured = ModelProviderInfo {
        server_overloaded_retry_delay_ms: Some(50),
        ..ModelProviderInfo::default()
    };
    assert_eq!(
        configured.server_overloaded_retry_delay(),
        Duration::from_millis(50),
        "explicit provider field wins over the env var"
    );
    assert_eq!(
        ModelProviderInfo::default().server_overloaded_retry_delay(),
        Duration::from_millis(75),
        "env var applies when the field is unset"
    );
    unsafe { std::env::remove_var("CODEX_SERVER_OVERLOADED_RETRY_DELAY_MS") };
    assert_eq!(
        ModelProviderInfo::default().server_overloaded_retry_delay(),
        Duration::from_millis(1_000),
        "neither set: default 1s"
    );
}

#[test]
fn test_deserialize_websocket_connect_timeout() {
    let provider_toml = r#"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
websocket_connect_timeout_ms = 15000
supports_websockets = true
        "#;

    let provider: ModelProviderInfo = toml::from_str(provider_toml).unwrap();
    assert_eq!(provider.websocket_connect_timeout_ms, Some(15_000));
}

#[test]
fn test_personal_access_token_uses_chatgpt_codex_base_url() {
    let api_provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        .to_api_provider(Some(AuthMode::PersonalAccessToken))
        .expect("OpenAI provider should build API provider");

    assert_eq!(api_provider.base_url, CHATGPT_CODEX_BASE_URL);
}

#[test]
fn test_header_auth_uses_chatgpt_codex_base_url() {
    let api_provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        .to_api_provider(Some(AuthMode::Headers))
        .expect("OpenAI provider should build API provider");

    assert_eq!(api_provider.base_url, CHATGPT_CODEX_BASE_URL);
}

#[test]
fn codex_backend_routes_require_codex_base_url() {
    for (base_url, expected) in [
        (None, true),
        (Some(CHATGPT_CODEX_BASE_URL), true),
        (Some("https://chatgpt-staging.com/backend-api/codex/"), true),
        (Some("https://backend.example.com/api/codex"), false),
        (Some("https://backend.example.com/api/codex/"), false),
        (Some("https://proxy.example.com/v1"), false),
    ] {
        let provider = ModelProviderInfo::create_openai_provider(base_url.map(str::to_owned));
        assert_eq!(provider.supports_codex_backend_routes(), expected);
    }
}

#[test]
fn test_uses_openai_actor_authorization() {
    let mut provider = ModelProviderInfo {
        http_headers: Some(maplit::hashmap! {
            "X-OpenAI-Actor-Authorization".to_string() => "actor-token".into(),
        }),
        ..ModelProviderInfo::default()
    };
    assert!(provider.uses_openai_actor_authorization());

    provider.http_headers = None;
    assert!(!provider.uses_openai_actor_authorization());

    provider.http_headers = Some(maplit::hashmap! {
        OPENAI_ACTOR_AUTHORIZATION_HEADER.to_string() => "  ".into(),
    });
    assert!(!provider.uses_openai_actor_authorization());

    provider.http_headers = Some(maplit::hashmap! {
        OPENAI_ACTOR_AUTHORIZATION_HEADER.to_string() => "actor-token".into(),
    });
    provider.requires_openai_auth = true;
    assert!(!provider.uses_openai_actor_authorization());
}

#[test]
fn test_deserialize_provider_auth_config_defaults() {
    let base_dir = tempdir().unwrap();
    let provider_toml = r#"
name = "Corp"

[auth]
command = "./scripts/print-token"
args = ["--format=text"]
        "#;

    let provider: ModelProviderInfo = {
        let _guard = AbsolutePathBufGuard::new(base_dir.path());
        toml::from_str(provider_toml).unwrap()
    };

    assert_eq!(
        provider.auth,
        Some(ModelProviderAuthInfo {
            command: "./scripts/print-token".to_string(),
            args: vec!["--format=text".into()],
            timeout_ms: NonZeroU64::new(5_000).unwrap(),
            refresh_interval_ms: 300_000,
            cwd: AbsolutePathBuf::resolve_path_against_base(".", base_dir.path()),
        })
    );
}

#[test]
fn test_deserialize_provider_aws_config() {
    let provider_toml = r#"
name = "Amazon Bedrock"
base_url = "https://bedrock.example.com/v1"

[aws]
region = "us-west-2"

[aws.credential_export]
command = "aws-vault"
args = ["--profile", "codex-bedrock"]

[aws.auth_refresh]
command = "aws"
args = ["login", "--profile", "codex-bedrock"]
        "#;

    let provider: ModelProviderInfo = toml::from_str(provider_toml).unwrap();

    assert_eq!(
        provider.aws,
        Some(ModelProviderAwsAuthInfo {
            profile: None,
            region: Some("us-west-2".to_string()),
            credential_export: Some(AwsCredentialExportConfig {
                command: "aws-vault".to_string(),
                args: vec!["--profile".into(), "codex-bedrock".into()],
                timeout_ms: NonZeroU64::new(30_000).expect("timeout should be non-zero"),
            }),
            auth_refresh: Some(AwsAuthRefreshConfig {
                command: "aws".to_string(),
                args: vec!["login".into(), "--profile".into(), "codex-bedrock".into()],
                timeout_ms: NonZeroU64::new(300_000).expect("timeout should be non-zero"),
            }),
        })
    );
}

#[test]
fn test_create_amazon_bedrock_provider() {
    assert_eq!(
        ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
        ModelProviderInfo {
            name: "Amazon Bedrock".to_string(),
            base_url: None,
            model_catalog_url: None,
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            gateway_oauth: None,
            aws: Some(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
                credential_export: None,
                auth_refresh: None,
            }),
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: Some(maplit::hashmap! {
                AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_HEADER.to_string() =>
                    AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_VALUE.into(),
            }),
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            websocket_connect_timeout_ms: None,
            server_overloaded_retry_delay_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
            supports_responses_continuation: false,
            multidex_compatibility: false,
            supports_standalone_web_search: false,
            capabilities: None,
            project_id: None,
            location: None,
            credential_file: None,
            include_internal_metadata: false,
            provider: None,
        }
    );
}

#[test]
fn test_create_amazon_bedrock_runtime_provider() {
    let mut expected = ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None);
    expected.name = "Amazon Bedrock Runtime".to_string();

    assert_eq!(
        ModelProviderInfo::create_amazon_bedrock_runtime_provider(/*aws*/ None),
        expected
    );
}

#[test]
fn test_create_amazon_bedrock_runtime_provider_with_aws_configuration() {
    let provider =
        ModelProviderInfo::create_amazon_bedrock_runtime_provider(Some(ModelProviderAwsAuthInfo {
            profile: Some("runtime-profile".to_string()),
            region: Some("us-west-2".to_string()),
            credential_export: None,
            auth_refresh: None,
        }));

    assert_eq!(
        (
            provider.name.as_str(),
            provider.aws,
            provider.http_headers,
            provider.supports_standalone_web_search,
        ),
        (
            "Amazon Bedrock Runtime",
            Some(ModelProviderAwsAuthInfo {
                profile: Some("runtime-profile".to_string()),
                region: Some("us-west-2".to_string()),
                credential_export: None,
                auth_refresh: None,
            }),
            Some(maplit::hashmap! {
                AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_HEADER.to_string() =>
                    AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_VALUE.into(),
            }),
            false,
        )
    );
}

fn provider_auth_for_test() -> ModelProviderAuthInfo {
    ModelProviderAuthInfo {
        command: "token-fetcher".to_string(),
        args: vec!["fetch".into()],
        timeout_ms: NonZeroU64::new(5_000).expect("timeout should be non-zero"),
        refresh_interval_ms: 300_000,
        cwd: std::env::current_dir()
            .expect("current directory should be available")
            .try_into()
            .expect("current directory should be absolute"),
    }
}

#[test]
fn test_amazon_bedrock_providers_add_mantle_client_agent_header() {
    for provider in [
        ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
        ModelProviderInfo::create_amazon_bedrock_runtime_provider(/*aws*/ None),
    ] {
        let api_provider = provider
            .to_api_provider(/*auth_mode*/ None)
            .expect("Amazon Bedrock provider should build API provider");

        assert_eq!(
            api_provider
                .headers
                .get(AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(AMAZON_BEDROCK_MANTLE_CLIENT_AGENT_VALUE),
            "provider: {}",
            provider.name
        );
    }
}

#[test]
fn test_built_in_model_providers_include_amazon_bedrock_endpoints() {
    let providers = built_in_model_providers(/*openai_base_url*/ None);

    assert_eq!(
        [
            AMAZON_BEDROCK_PROVIDER_ID,
            AMAZON_BEDROCK_RUNTIME_PROVIDER_ID
        ]
        .into_iter()
        .map(|provider_id| {
            providers
                .get(provider_id)
                .map(ModelProviderInfo::is_amazon_bedrock)
        })
        .collect::<Vec<_>>(),
        vec![Some(true), Some(true)]
    );
}

#[test]
fn test_built_in_model_providers_include_amazon_bedrock_runtime() {
    let providers = built_in_model_providers(/*openai_base_url*/ None);
    let runtime = providers
        .get(AMAZON_BEDROCK_RUNTIME_PROVIDER_ID)
        .expect("Amazon Bedrock Runtime provider should be built in");

    assert!(runtime.is_amazon_bedrock());
    assert!(runtime.is_amazon_bedrock_runtime());
    assert!(
        !providers
            .get(AMAZON_BEDROCK_PROVIDER_ID)
            .expect("Amazon Bedrock provider should be built in")
            .is_amazon_bedrock_runtime()
    );
}

#[test]
fn test_merge_configured_model_providers_adds_custom_provider() {
    let custom_provider = ModelProviderInfo {
        name: "Custom".to_string(),
        base_url: Some("https://example.com/v1".to_string()),
        ..ModelProviderInfo::default()
    };
    let configured_model_providers =
        std::collections::HashMap::from([("custom".to_string(), custom_provider.clone())]);

    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    expected.insert("custom".to_string(), custom_provider);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_multidex_provider_enables_responses_continuation() {
    let multidex_provider = ModelProviderInfo {
        name: "MultiDex".to_string(),
        base_url: Some("http://127.0.0.1:2120/v1".to_string()),
        ..ModelProviderInfo::default()
    };
    let configured_model_providers =
        std::collections::HashMap::from([(MULTIDEX_PROVIDER_ID.to_string(), multidex_provider)]);

    let providers = merge_configured_model_providers(
        built_in_model_providers(/*openai_base_url*/ None),
        configured_model_providers,
    )
    .expect("MultiDex provider should merge");

    assert!(
        providers[MULTIDEX_PROVIDER_ID].supports_responses_continuation,
        "MultiDex should enable its supported continuation protocol by default"
    );
    assert!(providers[MULTIDEX_PROVIDER_ID].multidex_compatibility);
}

#[test]
fn test_is_multidex_model_uses_only_the_canonical_id() {
    assert!(is_multidex_model(MULTIDEX_MODEL_ID));
    assert!(!is_multidex_model("DS v4.1 Flash 1017-preview"));
    assert!(!is_multidex_model("gpt-5.6"));
}

#[test]
fn test_merge_configured_model_providers_applies_amazon_bedrock_aws_override() {
    let credential_export = AwsCredentialExportConfig {
        command: "aws-vault".to_string(),
        args: vec!["export".into(), "codex-bedrock".into()],
        timeout_ms: NonZeroU64::new(30_000).expect("timeout should be non-zero"),
    };
    let auth_refresh = AwsAuthRefreshConfig {
        command: "aws".to_string(),
        args: vec!["login".into(), "--profile".into(), "codex-bedrock".into()],
        timeout_ms: NonZeroU64::new(10_000).expect("timeout should be non-zero"),
    };
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            aws: Some(ModelProviderAwsAuthInfo {
                profile: None,
                region: Some("us-west-2".to_string()),
                credential_export: Some(credential_export.clone()),
                auth_refresh: Some(auth_refresh.clone()),
            }),
            ..ModelProviderInfo::default()
        },
    )]);

    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    expected
        .get_mut(AMAZON_BEDROCK_PROVIDER_ID)
        .expect("Amazon Bedrock provider should be built in")
        .aws = Some(ModelProviderAwsAuthInfo {
        profile: None,
        region: Some("us-west-2".to_string()),
        credential_export: Some(credential_export),
        auth_refresh: Some(auth_refresh),
    });

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_model_providers_applies_runtime_overrides_independently() {
    let runtime_aws = ModelProviderAwsAuthInfo {
        profile: Some("runtime-profile".to_string()),
        region: Some("eu-west-1".to_string()),
        credential_export: None,
        auth_refresh: None,
    };
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_RUNTIME_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            base_url: Some("https://runtime.example.com/openai/v1".to_string()),
            aws: Some(runtime_aws.clone()),
            ..ModelProviderInfo::default()
        },
    )]);
    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    let expected_runtime = expected
        .get_mut(AMAZON_BEDROCK_RUNTIME_PROVIDER_ID)
        .expect("Amazon Bedrock Runtime provider should be built in");
    expected_runtime.base_url = Some("https://runtime.example.com/openai/v1".to_string());
    expected_runtime.aws = Some(runtime_aws);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_model_providers_applies_amazon_bedrock_transport_overrides() {
    let auth = provider_auth_for_test();
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            base_url: Some("https://proxy.example.com/v1".to_string()),
            auth: Some(auth.clone()),
            aws: Some(ModelProviderAwsAuthInfo {
                profile: Some("codex-bedrock".to_string()),
                region: Some("us-west-2".to_string()),
                credential_export: None,
                auth_refresh: None,
            }),
            http_headers: Some(maplit::hashmap! {
                "x-example-header".to_string() => "value".into(),
            }),
            ..ModelProviderInfo::default()
        },
    )]);

    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    let expected_provider = expected
        .get_mut(AMAZON_BEDROCK_PROVIDER_ID)
        .expect("Amazon Bedrock provider should be built in");
    expected_provider.base_url = Some("https://proxy.example.com/v1".to_string());
    expected_provider.auth = Some(auth);
    expected_provider.aws = Some(ModelProviderAwsAuthInfo {
        profile: Some("codex-bedrock".to_string()),
        region: Some("us-west-2".to_string()),
        credential_export: None,
        auth_refresh: None,
    });
    expected_provider
        .http_headers
        .get_or_insert_default()
        .insert("x-example-header".to_string(), "value".into());

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_model_providers_applies_gcp_transport_overrides() {
    let auth = provider_auth_for_test();
    let credential_file = std::env::current_dir().unwrap().join("adc.json");
    let configured_model_providers = std::collections::HashMap::from([(
        GCP_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            base_url: Some("https://vertex.example.com/v1".to_string()),
            project_id: Some("configured-project".to_string()),
            location: Some("us-east4".to_string()),
            auth: Some(auth.clone()),
            credential_file: Some(credential_file.clone()),
            http_headers: Some(maplit::hashmap! {
                "x-example-header".to_string() => "value".into(),
            }),
            ..ModelProviderInfo::default()
        },
    )]);

    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    let provider = expected
        .get_mut(GCP_PROVIDER_ID)
        .expect("GCP provider should be built in");
    provider.base_url = Some("https://vertex.example.com/v1".to_string());
    provider.project_id = Some("configured-project".to_string());
    provider.location = Some("us-east4".to_string());
    provider.auth = Some(auth);
    provider.credential_file = Some(credential_file);
    provider.http_headers = Some(maplit::hashmap! {
        "x-example-header".to_string() => "value".into(),
    });

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_model_providers_applies_gcp_endpoint_override() {
    let configured_model_providers = std::collections::HashMap::from([(
        GCP_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            base_url: Some("https://vertex.example.com/v1".to_string()),
            ..ModelProviderInfo::default()
        },
    )]);
    let mut expected = built_in_model_providers(/*openai_base_url*/ None);
    expected
        .get_mut(GCP_PROVIDER_ID)
        .expect("GCP provider should be built in")
        .base_url = Some("https://vertex.example.com/v1".to_string());

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(expected)
    );
}

#[test]
fn test_merge_configured_gcp_rejects_relative_credential_file() {
    for credential_file in ["", "adc.json", "../adc.json"] {
        let provider = ModelProviderInfo {
            credential_file: Some(credential_file.into()),
            ..ModelProviderInfo::create_gcp_provider()
        };
        assert_eq!(
            provider.validate(),
            Err("provider credential_file must be an absolute path".to_string()),
        );

        let overrides = HashMap::from([(
            GCP_PROVIDER_ID.to_string(),
            ModelProviderInfo {
                credential_file: Some(credential_file.into()),
                ..Default::default()
            },
        )]);

        assert_eq!(
            merge_configured_model_providers(
                built_in_model_providers(/*openai_base_url*/ None),
                overrides,
            ),
            Err(
                "model_providers.gcp provider credential_file must be an absolute path".to_string()
            ),
        );
    }
}

#[test]
fn test_merge_configured_model_providers_rejects_amazon_bedrock_non_default_fields() {
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            name: "Custom Bedrock".to_string(),
            aws: Some(ModelProviderAwsAuthInfo {
                profile: Some("codex-bedrock".to_string()),
                region: None,
                credential_export: None,
                auth_refresh: None,
            }),
            ..ModelProviderInfo::default()
        },
    )]);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Err(
            "model_providers.amazon-bedrock only supports changing `base_url`, `auth`, `http_headers`, `aws.profile`, `aws.region`, `aws.credential_export`, and `aws.auth_refresh`; other non-default provider fields are not supported"
                .to_string()
        )
    );
}

#[test]
fn test_merge_configured_model_providers_allows_amazon_bedrock_default_fields() {
    let configured_model_providers = std::collections::HashMap::from([(
        AMAZON_BEDROCK_PROVIDER_ID.to_string(),
        ModelProviderInfo {
            aws: Some(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
                credential_export: None,
                auth_refresh: None,
            }),
            wire_api: WireApi::Responses,
            ..ModelProviderInfo::default()
        },
    )]);

    assert_eq!(
        merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            configured_model_providers,
        ),
        Ok(built_in_model_providers(/*openai_base_url*/ None))
    );
}

#[test]
fn test_validate_provider_aws_rejects_conflicting_auth() {
    let provider = ModelProviderInfo {
        aws: Some(ModelProviderAwsAuthInfo {
            profile: None,
            region: None,
            credential_export: None,
            auth_refresh: None,
        }),
        env_key: Some("AWS_BEARER_TOKEN_BEDROCK".to_string()),
        supports_websockets: false,
        supports_responses_continuation: false,
        multidex_compatibility: false,
        ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
    };

    assert_eq!(
        provider.validate(),
        Err("provider aws cannot be combined with env_key, requires_openai_auth".to_string())
    );
}

#[test]
fn test_validate_provider_aws_rejects_websockets() {
    let provider = ModelProviderInfo {
        aws: Some(ModelProviderAwsAuthInfo {
            profile: None,
            region: None,
            credential_export: None,
            auth_refresh: None,
        }),
        requires_openai_auth: false,
        supports_websockets: true,
        supports_responses_continuation: false,
        multidex_compatibility: false,
        ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
    };

    assert_eq!(
        provider.validate(),
        Err("provider aws cannot be combined with supports_websockets".to_string())
    );
}

#[test]
fn test_validate_provider_aws_auth_refresh_command() {
    for (command, expected) in [
        (
            "  ",
            Err("provider aws.auth_refresh.command must not be empty".to_string()),
        ),
        (
            "other-command",
            Err("provider aws.auth_refresh.command must be `aws`".to_string()),
        ),
        ("aws", Ok(())),
    ] {
        let provider =
            ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
                profile: None,
                region: None,
                credential_export: None,
                auth_refresh: Some(AwsAuthRefreshConfig {
                    command: command.to_string(),
                    args: Vec::new(),
                    timeout_ms: NonZeroU64::new(300_000).expect("timeout should be non-zero"),
                }),
            }));

        assert_eq!(provider.validate(), expected);
    }
}

#[test]
fn test_validate_provider_aws_credential_export_command() {
    let absolute_command = std::env::current_exe()
        .expect("current executable should have a path")
        .to_string_lossy()
        .into_owned();
    for (command, expected) in [
        (
            String::new(),
            Err("provider aws.credential_export.command must not be empty".to_string()),
        ),
        (
            "  ".to_string(),
            Err("provider aws.credential_export.command must not be empty".to_string()),
        ),
        ("aws-vault".to_string(), Ok(())),
        (absolute_command, Ok(())),
        (
            "./scripts/export-credentials".to_string(),
            Err(
                "provider aws.credential_export.command must be an absolute path or a bare executable name"
                    .to_string(),
            ),
        ),
        (
            "scripts/export-credentials".to_string(),
            Err(
                "provider aws.credential_export.command must be an absolute path or a bare executable name"
                    .to_string(),
            ),
        ),
        (
            "scripts/.".to_string(),
            Err(
                "provider aws.credential_export.command must be an absolute path or a bare executable name"
                    .to_string(),
            ),
        ),
    ] {
        let mut provider =
            ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
                profile: None,
                region: Some("us-west-2".to_string()),
                credential_export: Some(AwsCredentialExportConfig {
                    command,
                    args: vec!["--secret-argument".into()],
                    timeout_ms: NonZeroU64::new(30_000).expect("timeout should be non-zero"),
                }),
                auth_refresh: Some(AwsAuthRefreshConfig {
                    command: "aws".to_string(),
                    args: Vec::new(),
                    timeout_ms: NonZeroU64::new(300_000).expect("timeout should be non-zero"),
                }),
            }));

        assert_eq!(provider.validate(), expected);
        if expected.is_ok() {
            provider.aws.as_mut().expect("AWS config").profile = Some("codex-bedrock".to_string());
            assert_eq!(
                provider.validate(),
                Err("provider aws.credential_export cannot be combined with aws.profile".to_string()),
            );
        }
        let export = provider
            .aws
            .as_ref()
            .and_then(|aws| aws.credential_export.as_ref())
            .expect("credential export should be configured");
        assert!(!format!("{export:?}").contains("--secret-argument"));
    }
}

#[test]
fn test_deserialize_provider_auth_config_allows_zero_refresh_interval() {
    let base_dir = tempdir().unwrap();
    let provider_toml = r#"
name = "Corp"

[auth]
command = "./scripts/print-token"
refresh_interval_ms = 0
        "#;

    let provider: ModelProviderInfo = {
        let _guard = AbsolutePathBufGuard::new(base_dir.path());
        toml::from_str(provider_toml).unwrap()
    };

    let auth = provider.auth.expect("auth config should deserialize");
    assert_eq!(auth.refresh_interval_ms, 0);
    assert_eq!(auth.refresh_interval(), None);
}

#[test]
fn model_catalog_url_deserializes_without_changing_inference_routing() {
    let provider: ModelProviderInfo = toml::from_str(
        r#"
name = "Gateway"
base_url = "https://gateway.example/v1"
model_catalog_url = "https://gateway.example/codex/catalog?token=catalog-secret"
"#,
    )
    .unwrap();
    assert!(!format!("{provider:?}").contains("catalog-secret"));
    assert_eq!(
        provider,
        ModelProviderInfo {
            name: "Gateway".to_string(),
            base_url: Some("https://gateway.example/v1".to_string()),
            model_catalog_url: Some(
                "https://gateway.example/codex/catalog?token=catalog-secret".into()
            ),
            ..ModelProviderInfo::default()
        }
    );
    assert_eq!(
        provider
            .to_api_provider(Some(AuthMode::ApiKey))
            .unwrap()
            .base_url,
        "https://gateway.example/v1"
    );
}

#[test]
fn test_built_in_model_providers_include_openrouter() {
    let providers = built_in_model_providers(/*openai_base_url*/ None);
    let provider = providers
        .get(OPENROUTER_PROVIDER_ID)
        .expect("openrouter provider is built in");

    assert_eq!(provider.name, "OpenRouter");
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(provider.env_key, None, "credentials come from config.toml");
    assert_eq!(provider.wire_api, WireApi::Responses);
    assert!(!provider.requires_openai_auth);
    assert!(provider.is_openrouter());
    // OpenRouter's /models response is not a Codex catalog, so discovery must
    // not be attempted; models resolve from bundled or fallback metadata.
    assert!(!provider.supports_api_key_models());
}

#[test]
fn lithos_uses_its_own_key_and_does_not_replay_failed_generations() {
    let provider = ModelProviderInfo::create_lithos_provider();
    assert_eq!(provider.name, "Lithos");
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://api.lithosai.cloud/v1")
    );
    assert_eq!(provider.env_key, None, "credentials come from config.toml");
    assert!(!provider.requires_openai_auth);
    assert!(!provider.supports_websockets);
    assert!(!provider.supports_responses_continuation);
    assert!(provider.multidex_compatibility);
    assert_eq!(provider.stream_max_retries, Some(0));
    assert_eq!(provider.request_max_retries, Some(0));
    assert!(is_lithos_model("deepseek-ai/DeepSeek-V4.1-Flash-ultra"));
    assert!(!is_lithos_model("gpt-6-sol"));
}

#[test]
fn lithos_auth_override_preserves_native_transport_identity() {
    let configured: ModelProviderInfo =
        toml::from_str("experimental_bearer_token = \"private-token\"\n").expect("valid provider");
    let merged = merge_configured_model_providers(
        built_in_model_providers(None),
        HashMap::from([(LITHOS_PROVIDER_ID.to_string(), configured)]),
    )
    .expect("override merges");
    let provider = &merged[LITHOS_PROVIDER_ID];
    assert!(provider.is_lithos());
    assert!(provider.multidex_compatibility);
    assert_eq!(
        provider
            .experimental_bearer_token
            .clone()
            .map(|token| token.into_inner())
            .as_deref(),
        Some("private-token")
    );
}

#[test]
fn built_in_overrides_reject_env_key() {
    for provider_id in [OPENROUTER_PROVIDER_ID, LITHOS_PROVIDER_ID] {
        let configured: ModelProviderInfo =
            toml::from_str("env_key = \"SOME_API_KEY\"\n").expect("valid provider");
        let err = merge_configured_model_providers(
            built_in_model_providers(None),
            HashMap::from([(provider_id.to_string(), configured)]),
        )
        .unwrap_err();
        assert!(
            err.contains("does not support `env_key`"),
            "provider {provider_id} returned unexpected error: {err}"
        );
    }
}

#[test]
fn test_deserialize_openrouter_provider_routing_toml() {
    let provider: ModelProviderInfo = toml::from_str(
        r#"
name = "OpenRouter"
base_url = "https://openrouter.ai/api/v1"
env_key = "OPENROUTER_API_KEY"

[provider]
only = ["wafer/mxfp4"]
allow_fallbacks = false
"#,
    )
    .unwrap();

    let routing = provider.provider.expect("provider routing parsed");
    assert_eq!(
        routing.only.as_deref(),
        Some(&["wafer/mxfp4".to_string()][..])
    );
    assert_eq!(routing.allow_fallbacks, Some(false));
}

#[test]
fn test_openrouter_routing_rejects_unknown_fields() {
    let err = toml::from_str::<ModelProviderInfo>(
        r#"
name = "OpenRouter"
base_url = "https://openrouter.ai/api/v1"

[provider]
oonly = ["wafer/mxfp4"]
"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("oonly"), "unexpected error: {err}");
}

#[test]
fn test_openrouter_override_merges_onto_built_in() {
    let configured: ModelProviderInfo = toml::from_str(
        r#"
[provider]
only = ["wafer/mxfp4"]
allow_fallbacks = false
"#,
    )
    .unwrap();

    let merged = merge_configured_model_providers(
        built_in_model_providers(/*openai_base_url*/ None),
        HashMap::from([(OPENROUTER_PROVIDER_ID.to_string(), configured)]),
    )
    .unwrap();
    let provider = merged.get(OPENROUTER_PROVIDER_ID).unwrap();

    // Built-in fields survive the override.
    assert_eq!(provider.name, "OpenRouter");
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(provider.env_key, None);
    let routing = provider.provider.as_ref().expect("provider routing merged");
    assert_eq!(
        routing.only.as_deref(),
        Some(&["wafer/mxfp4".to_string()][..])
    );
    assert_eq!(routing.allow_fallbacks, Some(false));
}

#[test]
fn wafer_provider_is_us_only_and_uses_configured_token() {
    let configured: ModelProviderInfo = toml::from_str(
        "base_url = \"https://us.pass.wafer.ai/v1\"\nexperimental_bearer_token = \"test-token\"",
    )
    .unwrap();
    let merged = merge_configured_model_providers(
        built_in_model_providers(None),
        HashMap::from([(WAFER_PROVIDER_ID.to_string(), configured)]),
    )
    .unwrap();
    let wafer = merged.get(WAFER_PROVIDER_ID).unwrap();
    assert_eq!(wafer.base_url.as_deref(), Some(WAFER_US_BASE_URL));
    assert_eq!(
        wafer
            .experimental_bearer_token
            .clone()
            .map(|token| token.into_inner())
            .as_deref(),
        Some("test-token")
    );

    let wrong_region: ModelProviderInfo =
        toml::from_str("base_url = \"https://pass.wafer.ai/v1\"").unwrap();
    assert!(
        merge_configured_model_providers(
            built_in_model_providers(None),
            HashMap::from([(WAFER_PROVIDER_ID.to_string(), wrong_region)]),
        )
        .is_err()
    );
}

#[test]
fn test_openrouter_override_rejects_unsupported_fields() {
    let configured: ModelProviderInfo = toml::from_str(
        r#"
requires_openai_auth = true
"#,
    )
    .unwrap();

    let err = merge_configured_model_providers(
        built_in_model_providers(/*openai_base_url*/ None),
        HashMap::from([(OPENROUTER_PROVIDER_ID.to_string(), configured)]),
    )
    .unwrap_err();
    assert!(
        err.contains("only supports changing"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_built_in_override_bearer_token_replaces_env_key() {
    for provider_id in [OPENROUTER_PROVIDER_ID, LITHOS_PROVIDER_ID] {
        let configured: ModelProviderInfo = toml::from_str(
            r#"
experimental_bearer_token = "sk-test-token"
"#,
        )
        .unwrap();

        let merged = merge_configured_model_providers(
            built_in_model_providers(/*openai_base_url*/ None),
            HashMap::from([(provider_id.to_string(), configured)]),
        )
        .unwrap();
        let provider = merged.get(provider_id).unwrap();

        assert_eq!(
            provider
                .experimental_bearer_token
                .clone()
                .map(|token| token.into_inner())
                .as_deref(),
            Some("sk-test-token"),
            "provider {provider_id} did not keep the bearer token"
        );
        assert_eq!(
            provider.env_key, None,
            "provider {provider_id} must not look up credentials in the environment"
        );
    }
}

#[test]
fn builtin_openai_catalog_override_preserves_inference_and_identity() {
    for inference_url in [None, Some("https://us.api.openai.com/v1")] {
        for catalog_url in [None, Some("https://chatgpt.com/backend-api/codex/models")] {
            let configured: ModelProviderInfo = toml::from_str(&format!(
                "name = \"Ignored name\"\nbase_url = \"https://ignored.example/v1\"\nrequires_openai_auth = false\n{}",
                catalog_url.map(|url| format!("model_catalog_url = {url:?}")).unwrap_or_default()
            )).unwrap();
            let builtins = built_in_model_providers(inference_url.map(str::to_owned));
            let mut expected = builtins.clone();
            expected.get_mut("openai").unwrap().model_catalog_url = catalog_url.map(Into::into);
            assert_eq!(
                merge_configured_model_providers(
                    builtins,
                    HashMap::from([("openai".to_string(), configured)]),
                )
                .unwrap(),
                expected,
            );
        }
    }
}

#[test]
fn builtin_openai_catalog_rejects_untrusted_destinations() {
    for url in [
        "http://chatgpt.com/backend-api/codex/models",
        "https://chatgpt.com.evil.example/models",
        "https://evilchatgpt.com/models",
        "https://api.openai.com/v1/models",
        "https://example.com/models?secret=do-not-log",
        "https://user:secret@chatgpt.com/models",
        "/models",
        "not a URL",
    ] {
        let provider = ModelProviderInfo {
            model_catalog_url: Some(url.into()),
            ..Default::default()
        };
        assert_eq!(
            merge_configured_model_providers(
                built_in_model_providers(/*openai_base_url*/ None),
                HashMap::from([("openai".to_string(), provider.clone())]),
            )
            .unwrap_err(),
            "model_providers.openai.model_catalog_url must use an allowed ChatGPT host. If you need to use a 3rd party host, configure a custom model_provider",
        );
        let mut expected = built_in_model_providers(/*openai_base_url*/ None);
        expected.insert("custom".into(), provider.clone());
        assert_eq!(
            merge_configured_model_providers(
                built_in_model_providers(/*openai_base_url*/ None),
                HashMap::from([("custom".into(), provider)]),
            )
            .unwrap(),
            expected,
        );
    }
}

#[test]
fn builtin_openai_catalog_accepts_existing_chatgpt_hosts() {
    for host in [
        "chatgpt.com",
        "gov.chatgpt.com",
        "chat.openai.com",
        "chatgpt-staging.com",
        "api.chatgpt-staging.com",
        "CHATGPT.COM",
    ] {
        let url = format!("https://{host}/backend-api/codex/models");
        let provider = ModelProviderInfo {
            model_catalog_url: Some(url.clone().into()),
            ..Default::default()
        };
        let mut expected = built_in_model_providers(/*openai_base_url*/ None);
        expected.get_mut("openai").unwrap().model_catalog_url = Some(url.into());
        assert_eq!(
            merge_configured_model_providers(
                built_in_model_providers(/*openai_base_url*/ None),
                HashMap::from([("openai".into(), provider)]),
            )
            .unwrap(),
            expected
        );
    }
}

#[test]
fn cyber_access_program_capability_uses_provider_identity_and_explicit_overrides() {
    for provider_id in [
        OPENAI_PROVIDER_ID,
        "proxy",
        AMAZON_BEDROCK_PROVIDER_ID,
        GCP_PROVIDER_ID,
    ] {
        for configured in [None, Some(false), Some(true)] {
            // A custom provider cannot gain the default by copying OpenAI's display name.
            let config = match configured {
                Some(enabled) => format!(
                    "name = \"OpenAI\"\n[capabilities]\ncyber_access_programs = {enabled}\n"
                ),
                None => "name = \"OpenAI\"\n".to_owned(),
            };
            let provider: ModelProviderInfo = toml::from_str(&config).unwrap();
            assert_eq!(
                provider.supports_cyber_access_programs(provider_id),
                configured.unwrap_or(provider_id == OPENAI_PROVIDER_ID),
            );
            let round_trip: ModelProviderInfo =
                toml::from_str(&toml::to_string(&provider).unwrap()).unwrap();
            assert_eq!(round_trip, provider);
        }
    }
}
