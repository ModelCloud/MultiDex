use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use crate::agent::api::AgentControl;
use crate::agent::control::LocalAgentRuntime;
use crate::agents_md_manager::AgentsMdManager;
use crate::attestation::AttestationProvider;
use crate::client::ModelClient;
use crate::config::NetworkProxyAuditMetadata;
use crate::config::StartedNetworkProxy;
use crate::current_time::TimeProvider;
use crate::elicitation::ElicitationService;
use crate::environment_selection::ThreadEnvironments;
use crate::exec_policy::ExecPolicyManager;
use crate::mcp::McpManager;
use crate::mcp_tool_exposure::McpHandlerCache;
use crate::tools::ExecutedToolCalls;
use crate::tools::code_mode::CodeModeService;
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::sandboxing::ApprovalStore;
use crate::unified_exec::UnifiedExecProcessManager;
use arc_swap::ArcSwap;
use arc_swap::ArcSwapOption;
use codex_analytics::AnalyticsEventsClient;
use codex_attachment_store::AttachmentStore;
use codex_core_plugins::PluginsManager;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::ExtensionRegistry;
use codex_hooks::Hooks;
use codex_http_client::RouteAwareClientPool;
use codex_login::AuthManager;
use codex_mcp::McpRuntime;
use codex_models_manager::CatalogModel;
use codex_models_manager::ModelsManagerConfig;
use codex_models_manager::manager::ModelsManager;
use codex_models_manager::manager::ModelsManagerFuture;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_otel::SessionTelemetry;
use codex_protocol::capabilities::SelectedCapabilityRoot;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_rollout::state_db::StateDbHandle;
use codex_rollout_trace::ThreadTraceContext;
use codex_sandboxing::policy_transforms::merge_permission_profiles;
use codex_skills_extension::HostSkillsService;
use codex_thread_store::LiveThread;
use codex_thread_store::ThreadStore;
use codex_utils_git_discovery::GitRootDiscovery;
use tokio::runtime::Handle;
use tokio::sync::Mutex;

pub(crate) struct SessionServices {
    /// The single owner of live MCP connections for this thread.
    pub(crate) mcp_runtime: Arc<McpRuntime>,
    /// Immutable MCP handlers scoped to this thread's current binding.
    pub(crate) mcp_handler_cache: McpHandlerCache,
    pub(crate) unified_exec_manager: UnifiedExecProcessManager,
    pub(crate) elicitations: ElicitationService,
    pub(crate) analytics_events_client: AnalyticsEventsClient,
    pub(crate) hooks: ArcSwap<Hooks>,
    pub(crate) rollout_thread_trace: ThreadTraceContext,
    pub(crate) user_shell: Arc<crate::shell::Shell>,
    pub(crate) show_raw_agent_reasoning: bool,
    pub(crate) exec_policy: Arc<ExecPolicyManager>,
    pub(crate) auth_manager: Arc<AuthManager>,
    /// Upload-only clients shared across turns without logging signed blob URLs.
    pub(crate) openai_file_upload_client_pool: RouteAwareClientPool,
    /// Stable handle to the session's models manager; resolves through `models_manager_swappable`
    /// so a provider switch mid-session re-points every consumer at the new provider's catalog.
    pub(crate) models_manager: SharedModelsManager,
    /// Concrete handle used to swap the inner manager when the session changes provider.
    pub(crate) models_manager_swappable: Arc<SwappableModelsManager>,
    pub(crate) git_root_discovery: Arc<GitRootDiscovery>,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) tool_approvals: Mutex<ApprovalStore>,
    /// Shared with captured steps so later calls observe newly approved permissions.
    pub(crate) granted_permissions_by_environment_id:
        Arc<StdMutex<HashMap<String, AdditionalPermissionProfile>>>,
    pub(crate) runtime_handle: Handle,
    pub(crate) skills_service: Arc<HostSkillsService>,
    pub(crate) agents_md_manager: Arc<AgentsMdManager>,
    pub(crate) plugins_manager: Arc<PluginsManager>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) extensions: Arc<ExtensionRegistry<crate::config::Config>>,
    pub(crate) session_extension_data: ExtensionData,
    pub(crate) thread_extension_data: ExtensionData,
    /// MCP extensions fixed when this session is created.
    pub(crate) client_mcp_extensions: ClientMcpExtensions,
    /// Roots supplied when the thread started or loaded from saved history.
    /// Keep roots for unselected environments so selecting them again restores their capabilities.
    pub(crate) selected_capability_roots: Vec<SelectedCapabilityRoot>,
    pub(crate) mcp_thread_init: ExtensionDataInit,
    pub(crate) agent_control: Arc<dyn AgentControl>,
    pub(crate) local_agent_runtime: LocalAgentRuntime,
    pub(crate) network_proxy: ArcSwapOption<StartedNetworkProxy>,
    pub(crate) network_proxy_audit_metadata: NetworkProxyAuditMetadata,
    pub(crate) managed_network_requirements_configured: bool,
    pub(crate) network_approval: Arc<NetworkApprovalService>,
    pub(crate) state_db: Option<StateDbHandle>,
    pub(crate) live_thread: Option<LiveThread>,
    pub(crate) image_store: Arc<dyn AttachmentStore>,
    pub(crate) thread_store: Arc<dyn ThreadStore>,
    pub(crate) attestation_provider: Option<Arc<dyn AttestationProvider>>,
    pub(crate) time_provider: Arc<dyn TimeProvider>,
    /// Session-scoped model client shared across turns.
    pub(crate) model_client: ModelClient,
    /// Clients for providers selected after the session starts.
    pub(crate) alternate_model_clients: Mutex<Vec<ModelClient>>,
    pub(crate) executed_tool_calls: ExecutedToolCalls,
    pub(crate) code_mode_service: CodeModeService,
    pub(crate) tool_search_handler_cache: ToolSearchHandlerCache,
    pub(crate) turn_environments: Arc<ThreadEnvironments>,
}

/// `ModelsManager` whose inner manager can be replaced after construction. A mid-session
/// provider switch swaps the inner manager so catalogs, ladders, and instructions resolve
/// against the active provider instead of the one the session was constructed with.
pub(crate) struct SwappableModelsManager {
    inner: ArcSwap<SharedModelsManager>,
    auth_manager: Arc<AuthManager>,
}

impl SwappableModelsManager {
    pub(crate) fn new(manager: SharedModelsManager, auth_manager: Arc<AuthManager>) -> Self {
        Self {
            inner: ArcSwap::from_pointee(manager),
            auth_manager,
        }
    }

    /// Point this handle at a manager built for the session's new provider.
    pub(crate) fn swap(&self, manager: SharedModelsManager) {
        self.inner.store(Arc::new(manager));
    }
}

impl fmt::Debug for SwappableModelsManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SwappableModelsManager")
            .field("inner", &self.inner.load())
            .finish()
    }
}

impl ModelsManager for SwappableModelsManager {
    fn for_originator(&self, originator: &str) -> Option<SharedModelsManager> {
        self.inner.load().for_originator(originator)
    }

    fn set_api_key_model_discovery_enabled(&self, enabled: bool) {
        self.inner
            .load()
            .set_api_key_model_discovery_enabled(enabled);
    }

    fn model_catalog_with_origins(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: codex_http_client::HttpClientFactory,
    ) -> ModelsManagerFuture<'_, Vec<CatalogModel>> {
        let manager = self.inner.load_full();
        Box::pin(async move {
            manager
                .model_catalog_with_origins(refresh_strategy, http_client_factory)
                .await
        })
    }

    fn refresh_after_auth_change(
        &self,
        http_client_factory: codex_http_client::HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        let manager = self.inner.load_full();
        Box::pin(async move { manager.refresh_after_auth_change(http_client_factory).await })
    }

    fn get_remote_models(
        &self,
    ) -> ModelsManagerFuture<'_, Vec<codex_protocol::openai_models::ModelInfo>> {
        let manager = self.inner.load_full();
        Box::pin(async move { manager.get_remote_models().await })
    }

    fn try_get_remote_models(
        &self,
    ) -> Result<Vec<codex_protocol::openai_models::ModelInfo>, tokio::sync::TryLockError> {
        self.inner.load().try_get_remote_models()
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        Some(self.auth_manager.as_ref())
    }

    fn list_collaboration_modes(&self) -> Vec<codex_protocol::config_types::CollaborationModeMask> {
        self.inner.load().list_collaboration_modes()
    }

    fn get_model_info<'a>(
        &'a self,
        model: &'a str,
        config: &'a ModelsManagerConfig,
    ) -> ModelsManagerFuture<'a, CatalogModel> {
        let manager = self.inner.load_full();
        Box::pin(async move { manager.get_model_info(model, config).await })
    }

    fn refresh_if_new_etag(
        &self,
        etag: String,
        http_client_factory: codex_http_client::HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        let manager = self.inner.load_full();
        Box::pin(async move { manager.refresh_if_new_etag(etag, http_client_factory).await })
    }
}

impl SessionServices {
    #[expect(
        clippy::expect_used,
        reason = "poisoned grant state must not authorize further operations"
    )]
    pub(crate) fn record_granted_permissions(
        &self,
        environment_id: &str,
        permissions: AdditionalPermissionProfile,
    ) {
        let mut grants = self
            .granted_permissions_by_environment_id
            .lock()
            .expect("session permission grants lock poisoned");
        let granted_permissions =
            merge_permission_profiles(grants.get(environment_id), Some(&permissions));
        if let Some(granted_permissions) = granted_permissions {
            grants.insert(environment_id.to_string(), granted_permissions);
        }
    }
}
