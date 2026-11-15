use super::*;
use crate::create_model_provider;
use codex_model_provider_info::ModelProviderCapabilities;
use pretty_assertions::assert_eq;

#[test]
fn capability_overrides_preserve_unspecified_provider_defaults() {
    for info in [
        ModelProviderInfo::default(),
        ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        ModelProviderInfo {
            name: "Azure".to_string(),
            ..Default::default()
        },
    ] {
        let defaults = create_model_provider(info.clone(), /*auth_manager*/ None).capabilities();
        let provider = create_model_provider(
            ModelProviderInfo {
                capabilities: Some(ModelProviderCapabilities {
                    external_web_access: Some(false),
                    ..Default::default()
                }),
                ..info
            },
            /*auth_manager*/ None,
        );
        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                external_web_access: false,
                ..defaults
            },
        );
    }
}

#[test]
fn generic_provider_preserves_upstream_capabilities_without_openai_auth() {
    for name in ["custom", "Azure", "MultiDex display name only"] {
        let info = ModelProviderInfo {
            name: name.to_string(),
            requires_openai_auth: false,
            ..Default::default()
        };
        let expected_remote_compaction = if name == "Azure" {
            RemoteCompactionSupport::V2
        } else {
            RemoteCompactionSupport::Unsupported
        };
        assert_eq!(
            create_model_provider(info, /*auth_manager*/ None).capabilities(),
            ProviderCapabilities {
                remote_compaction: expected_remote_compaction,
                ..ProviderCapabilities::default()
            },
        );
    }
}

#[test]
fn multidex_provider_enables_v2_remote_compaction() {
    let info = ModelProviderInfo {
        multidex_compatibility: true,
        requires_openai_auth: false,
        ..Default::default()
    };
    let capabilities = create_model_provider(info, /*auth_manager*/ None).capabilities();
    assert_eq!(capabilities.remote_compaction, RemoteCompactionSupport::V2);
    // MultiDex keeps the portable tool surface; unrelated defaults stay put.
    assert!(!capabilities.namespace_tools);
    assert_eq!(
        capabilities,
        ProviderCapabilities {
            namespace_tools: false,
            reasoning_summaries: false,
            image_generation: false,
            web_search: false,
            external_web_access: false,
            remote_compaction: RemoteCompactionSupport::V2,
            ..ProviderCapabilities::default()
        },
    );
}

#[test]
fn deepinfra_uses_portable_function_tools_without_remote_compaction() {
    let capabilities = create_model_provider(
        ModelProviderInfo::create_deepinfra_provider(),
        /*auth_manager*/ None,
    )
    .capabilities();
    assert!(!capabilities.namespace_tools);
    assert!(!capabilities.web_search);
    assert!(!capabilities.image_generation);
    assert_eq!(
        capabilities.remote_compaction,
        RemoteCompactionSupport::Unsupported
    );
}

#[test]
fn multidex_remote_compaction_override_wins() {
    let info = ModelProviderInfo {
        multidex_compatibility: true,
        capabilities: Some(ModelProviderCapabilities {
            remote_compaction: Some(RemoteCompactionSupport::Unsupported),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        create_model_provider(info, /*auth_manager*/ None)
            .capabilities()
            .remote_compaction,
        RemoteCompactionSupport::Unsupported
    );
}
