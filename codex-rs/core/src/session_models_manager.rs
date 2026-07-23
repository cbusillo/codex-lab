use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use codex_login::AuthManager;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::ModelsManager;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelsResponse;
use tokio::sync::TryLockError;

use crate::config::Config;
use crate::execution_account::ExecutionAccountLease;
use crate::execution_account::ExecutionAccountModelsContext;

const SESSION_MODELS_CACHE_SUBDIR: &str = "models-cache";

#[derive(Debug)]
struct LeaseAwareModelsManager {
    lease: ExecutionAccountLease,
    provider_id: String,
    provider: ModelProviderInfo,
    codex_home: PathBuf,
    config_model_catalog: Option<ModelsResponse>,
    state: Mutex<(u64, SharedModelsManager)>,
}

pub(crate) fn models_manager_for_execution_account(
    config: &Config,
    lease: ExecutionAccountLease,
) -> SharedModelsManager {
    Arc::new(LeaseAwareModelsManager::new(config, lease))
}

impl LeaseAwareModelsManager {
    fn new(config: &Config, lease: ExecutionAccountLease) -> Self {
        let models_context = lease.models_context();
        let manager = Self::build_manager(
            config.codex_home.as_path(),
            &config.model_provider_id,
            &config.model_provider,
            config.model_catalog.clone(),
            &models_context,
        );
        Self {
            lease,
            provider_id: config.model_provider_id.clone(),
            provider: config.model_provider.clone(),
            codex_home: config.codex_home.to_path_buf(),
            config_model_catalog: config.model_catalog.clone(),
            state: Mutex::new((models_context.generation, manager)),
        }
    }

    fn current_manager(&self) -> SharedModelsManager {
        let models_context = self.lease.models_context();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.0 != models_context.generation {
            state.1 = Self::build_manager(
                &self.codex_home,
                &self.provider_id,
                &self.provider,
                self.config_model_catalog.clone(),
                &models_context,
            );
            state.0 = models_context.generation;
        }
        Arc::clone(&state.1)
    }

    fn build_manager(
        codex_home: &Path,
        provider_id: &str,
        provider: &ModelProviderInfo,
        config_model_catalog: Option<ModelsResponse>,
        models_context: &ExecutionAccountModelsContext,
    ) -> SharedModelsManager {
        let provider = create_model_provider(
            provider.clone(),
            Some(Arc::clone(&models_context.auth_manager)),
        );
        provider.models_manager(
            scoped_models_cache_home(codex_home, provider_id, &models_context.cache_key),
            config_model_catalog,
        )
    }
}

#[async_trait]
impl ModelsManager for LeaseAwareModelsManager {
    async fn raw_model_catalog(&self, refresh_strategy: RefreshStrategy) -> ModelsResponse {
        self.current_manager()
            .raw_model_catalog(refresh_strategy)
            .await
    }

    async fn get_remote_models(&self) -> Vec<ModelInfo> {
        self.current_manager().get_remote_models().await
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        self.current_manager().try_get_remote_models()
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        None
    }

    fn build_available_models(&self, mut remote_models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        remote_models.sort_by_key(|model| model.priority);
        let mut presets: Vec<ModelPreset> = remote_models.into_iter().map(Into::into).collect();
        let uses_codex_backend = self.lease.auth_manager().current_auth_uses_codex_backend();
        presets = ModelPreset::filter_by_auth(presets, uses_codex_backend);
        ModelPreset::mark_default_by_picker_visibility(&mut presets);
        presets
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        self.current_manager().list_collaboration_modes()
    }

    async fn refresh_if_new_etag(&self, etag: String) {
        self.current_manager().refresh_if_new_etag(etag).await;
    }
}

fn scoped_models_cache_home(
    codex_home: &Path,
    provider_key: &str,
    execution_account_key: &str,
) -> PathBuf {
    let provider_key = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("codex-model-cache-provider:{provider_key}").as_bytes(),
    )
    .simple()
    .to_string();
    codex_home
        .join(SESSION_MODELS_CACHE_SUBDIR)
        .join(provider_key)
        .join(execution_account_key)
}

#[cfg(test)]
#[path = "session_models_manager_tests.rs"]
mod tests;
