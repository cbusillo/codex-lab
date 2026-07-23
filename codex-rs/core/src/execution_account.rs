use std::fmt;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use arc_swap::ArcSwap;
use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::StoredAccount;
use codex_protocol::ThreadId;
use codex_protocol::account::PlanType;
use serde::Deserialize;
use serde::Serialize;
use tracing::warn;

use crate::account_switching;
use crate::account_switching::RateLimitSwitchState;

const LEASE_VERSION: u32 = 1;
const LEASE_SUBDIR: &str = "execution-account-leases";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionAccountPooling {
    Disabled,
    Enabled,
}

impl ExecutionAccountPooling {
    fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionAccountStart {
    New,
    Cleared,
    Resumed,
    Forked { source_thread_id: Option<ThreadId> },
}

#[derive(Clone)]
pub(crate) struct ExecutionAccountLease {
    inner: Arc<ExecutionAccountLeaseInner>,
}

struct ExecutionAccountLeaseInner {
    current: ArcSwap<ExecutionAccount>,
    control_auth_manager: Arc<AuthManager>,
    config: ExecutionAccountConfig,
    thread_id: ThreadId,
    next_generation: AtomicU64,
    revision: StdMutex<ExecutionAccountRevision>,
}

#[derive(Clone)]
struct ExecutionAccountConfig {
    codex_home: PathBuf,
    auth_home: PathBuf,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    chatgpt_base_url: String,
    allow_api_key_fallback: bool,
    pooling: ExecutionAccountPooling,
}

struct ExecutionAccount {
    stored_account_id: Option<String>,
    label: Option<String>,
    mode: AuthMode,
    auth_manager: Arc<AuthManager>,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionAccountIdentity {
    pub(crate) stored_account_id: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) mode: AuthMode,
}

#[derive(Default)]
struct ExecutionAccountRevision {
    generation: u64,
    auth_revision: u64,
    combined_revision: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedExecutionAccountLease {
    version: u32,
    account_id: String,
}

pub(crate) struct ExecutionAccountOptions {
    pub(crate) codex_home: PathBuf,
    pub(crate) auth_home: PathBuf,
    pub(crate) auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub(crate) chatgpt_base_url: String,
    pub(crate) allow_api_key_fallback: bool,
    pub(crate) pooling: ExecutionAccountPooling,
    pub(crate) start: ExecutionAccountStart,
}

impl ExecutionAccountConfig {
    fn matching_control_account_id(
        &self,
        control_auth_manager: &AuthManager,
    ) -> io::Result<Option<String>> {
        let Some(account_id) =
            codex_login::get_active_account_id(&self.auth_home, self.auth_credentials_store_mode)?
        else {
            return Ok(None);
        };
        let Some(account) = codex_login::find_account(
            &self.auth_home,
            self.auth_credentials_store_mode,
            &account_id,
        )?
        else {
            return Ok(None);
        };
        Ok(control_auth_manager
            .auth_cached()
            .as_ref()
            .is_some_and(|auth| auth_matches_account(auth, &account))
            .then_some(account_id))
    }
}

impl fmt::Debug for ExecutionAccountLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionAccountLease")
            .field("thread_id", &self.inner.thread_id)
            .field("identity", &self.identity())
            .finish_non_exhaustive()
    }
}

impl ExecutionAccountLease {
    pub(crate) async fn resolve(
        thread_id: ThreadId,
        control_auth_manager: Arc<AuthManager>,
        options: ExecutionAccountOptions,
    ) -> Self {
        let start = options.start;
        let mut config = ExecutionAccountConfig {
            codex_home: options.codex_home,
            auth_home: options.auth_home,
            auth_credentials_store_mode: options.auth_credentials_store_mode,
            chatgpt_base_url: options.chatgpt_base_url,
            allow_api_key_fallback: options.allow_api_key_fallback,
            pooling: options.pooling,
        };
        let control_account_id = config
            .matching_control_account_id(&control_auth_manager)
            .unwrap_or_else(|error| {
                warn!("failed to resolve active control catalog account: {error}");
                None
            });
        if config.pooling.is_enabled()
            && !control_auth_manager
                .auth_mode()
                .is_some_and(AuthMode::has_chatgpt_account)
        {
            config.pooling = ExecutionAccountPooling::Disabled;
        }
        if config.pooling.is_enabled() && control_account_id.is_none() {
            config.pooling = ExecutionAccountPooling::Disabled;
        }
        let initial = Self::resolve_initial_account(
            &config,
            thread_id,
            start,
            control_account_id,
            Arc::clone(&control_auth_manager),
        )
        .await;
        let generation = initial.generation;
        Self {
            inner: Arc::new(ExecutionAccountLeaseInner {
                current: ArcSwap::from(initial),
                control_auth_manager,
                config,
                thread_id,
                next_generation: AtomicU64::new(generation.saturating_add(1)),
                revision: StdMutex::new(ExecutionAccountRevision {
                    generation,
                    auth_revision: 0,
                    combined_revision: 0,
                }),
            }),
        }
    }

    async fn resolve_initial_account(
        config: &ExecutionAccountConfig,
        thread_id: ThreadId,
        start: ExecutionAccountStart,
        control_account_id: Option<String>,
        control_auth_manager: Arc<AuthManager>,
    ) -> Arc<ExecutionAccount> {
        let selected_account_id = if config.pooling.is_enabled() {
            match start {
                ExecutionAccountStart::New | ExecutionAccountStart::Cleared => {
                    account_switching::select_preferred_account_id(
                        &config.codex_home,
                        &config.auth_home,
                        config.auth_credentials_store_mode,
                        config.allow_api_key_fallback,
                        Utc::now(),
                    )
                    .map_err(|err| warn!("failed to select execution account: {err}"))
                    .ok()
                    .flatten()
                }
                ExecutionAccountStart::Resumed => {
                    read_persisted_lease(&config.codex_home, thread_id)
                }
                ExecutionAccountStart::Forked { source_thread_id } => {
                    source_thread_id.and_then(|source_thread_id| {
                        read_persisted_lease(&config.codex_home, source_thread_id)
                    })
                }
            }
        } else {
            control_account_id.clone()
        };

        let selected = match selected_account_id.as_deref() {
            Some(account_id) => {
                Self::load_account(
                    config,
                    account_id,
                    control_account_id.as_deref(),
                    Arc::clone(&control_auth_manager),
                    /*generation*/ 0,
                )
                .await
            }
            None => None,
        };
        let account = selected.unwrap_or_else(|| {
            Arc::new(ExecutionAccount::from_control(
                control_account_id,
                control_auth_manager,
                /*generation*/ 0,
            ))
        });
        if config.pooling.is_enabled()
            && let Some(account_id) = account.stored_account_id.as_deref()
            && let Err(err) = persist_lease(&config.codex_home, thread_id, account_id)
        {
            warn!("failed to persist execution account lease: {err}");
        }
        account
    }

    async fn load_account(
        config: &ExecutionAccountConfig,
        account_id: &str,
        control_account_id: Option<&str>,
        control_auth_manager: Arc<AuthManager>,
        generation: u64,
    ) -> Option<Arc<ExecutionAccount>> {
        let account = codex_login::find_account(
            &config.auth_home,
            config.auth_credentials_store_mode,
            account_id,
        )
        .ok()
        .flatten()?;
        let use_control_auth_manager = control_account_id == Some(account_id)
            && control_auth_manager
                .auth_cached()
                .as_ref()
                .is_some_and(|auth| auth_matches_account(auth, &account));
        let auth_manager = if use_control_auth_manager {
            control_auth_manager
        } else {
            match AuthManager::for_catalog_account(
                config.auth_home.clone(),
                account_id.to_string(),
                config.auth_credentials_store_mode,
                Some(config.chatgpt_base_url.clone()),
            )
            .await
            {
                Ok(auth_manager) => auth_manager,
                Err(err) => {
                    warn!(account_id, "failed to load execution account auth: {err}");
                    return None;
                }
            }
        };
        if !auth_manager
            .auth_cached()
            .as_ref()
            .is_some_and(|auth| auth_matches_account(auth, &account))
        {
            warn!(
                account_id,
                "execution account auth does not match catalog account"
            );
            return None;
        }
        Some(Arc::new(ExecutionAccount::from_stored(
            account,
            auth_manager,
            generation,
        )))
    }

    pub(crate) fn identity(&self) -> ExecutionAccountIdentity {
        self.inner.current.load().identity()
    }

    pub(crate) fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.inner.current.load().auth_manager)
    }

    pub(crate) async fn auth_with_revision(&self) -> (Option<CodexAuth>, u64) {
        let account = self.inner.current.load_full();
        let (auth, auth_revision) = account.auth_manager.auth_with_revision().await;
        let revision = self.revision_for(account.generation, auth_revision);
        (auth, revision)
    }

    pub(crate) fn auth_revision(&self) -> u64 {
        let account = self.inner.current.load();
        self.revision_for(account.generation, account.auth_manager.auth_revision())
    }

    fn revision_for(&self, generation: u64, auth_revision: u64) -> u64 {
        let mut revision = self
            .inner
            .revision
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if revision.generation != generation || revision.auth_revision != auth_revision {
            revision.generation = generation;
            revision.auth_revision = auth_revision;
            revision.combined_revision = revision.combined_revision.saturating_add(1);
        }
        revision.combined_revision
    }

    pub(crate) fn prompt_cache_discriminator(&self) -> Option<String> {
        let account = self.inner.current.load();
        if Arc::ptr_eq(&account.auth_manager, &self.inner.control_auth_manager) {
            return None;
        }
        account
            .stored_account_id
            .clone()
            .or_else(|| account.auth_manager.auth_cached()?.get_account_id())
    }

    pub(crate) fn usage_context(&self) -> Option<(String, Option<PlanType>)> {
        let account = self.inner.current.load();
        let auth = account.auth_manager.auth_cached()?;
        let account_id = account
            .stored_account_id
            .clone()
            .or_else(|| auth.get_account_id())?;
        Some((account_id, auth.account_plan_type()))
    }

    pub(crate) async fn failover_after_usage_limit(
        &self,
        switch_state: &mut RateLimitSwitchState,
        blocked_until: Option<DateTime<Utc>>,
    ) -> io::Result<Option<ExecutionAccountIdentity>> {
        if !self.inner.config.pooling.is_enabled() {
            return Ok(None);
        }
        let Some(control_account_id) = self
            .inner
            .config
            .matching_control_account_id(&self.inner.control_auth_manager)?
        else {
            return Ok(None);
        };
        let current = self.inner.current.load_full();
        let Some(current_account_id) = current.stored_account_id.as_deref() else {
            return Ok(None);
        };
        switch_state.mark_limited(current_account_id, current.mode, blocked_until);
        let Some(next_account_id) = account_switching::select_next_account_id(
            &self.inner.config.codex_home,
            &self.inner.config.auth_home,
            self.inner.config.auth_credentials_store_mode,
            switch_state,
            self.inner.config.allow_api_key_fallback,
            Utc::now(),
            Some(current_account_id),
        )?
        else {
            return Ok(None);
        };
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let Some(next) = Self::load_account(
            &self.inner.config,
            &next_account_id,
            Some(&control_account_id),
            Arc::clone(&self.inner.control_auth_manager),
            generation,
        )
        .await
        else {
            return Ok(None);
        };
        let identity = next.identity();
        self.inner.current.store(next);
        if let Err(err) = persist_lease(
            &self.inner.config.codex_home,
            self.inner.thread_id,
            &next_account_id,
        ) {
            warn!("failed to persist execution account failover lease: {err}");
        }
        Ok(Some(identity))
    }

    pub(crate) async fn prepare_for_control_auth_reload(&self) {
        if !self.inner.config.pooling.is_enabled() {
            return;
        }
        let current = self.inner.current.load_full();
        if !Arc::ptr_eq(&current.auth_manager, &self.inner.control_auth_manager) {
            return;
        }
        let Some(current_account_id) = current.stored_account_id.as_deref() else {
            return;
        };
        let active_account_id = codex_login::get_active_account_id(
            &self.inner.config.auth_home,
            self.inner.config.auth_credentials_store_mode,
        )
        .ok()
        .flatten();
        if active_account_id.as_deref() == Some(current_account_id) {
            return;
        }
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let Some(detached) = Self::load_account(
            &self.inner.config,
            current_account_id,
            /*control_account_id*/ None,
            Arc::clone(&self.inner.control_auth_manager),
            generation,
        )
        .await
        else {
            return;
        };
        self.inner.current.store(detached);
    }

    pub(crate) async fn reconcile_after_control_auth_reload(&self) {
        let current = self.inner.current.load_full();
        let uses_control_auth_manager =
            Arc::ptr_eq(&current.auth_manager, &self.inner.control_auth_manager);
        let active_account_id = self
            .inner
            .config
            .matching_control_account_id(&self.inner.control_auth_manager)
            .unwrap_or_else(|error| {
                warn!("failed to reconcile active control catalog account: {error}");
                None
            });
        if active_account_id.is_none() {
            if uses_control_auth_manager && current.stored_account_id.is_none() {
                return;
            }
            let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
            self.inner
                .current
                .store(Arc::new(ExecutionAccount::from_control(
                    /*stored_account_id*/ None,
                    Arc::clone(&self.inner.control_auth_manager),
                    generation,
                )));
            return;
        }
        if uses_control_auth_manager && current.stored_account_id == active_account_id {
            return;
        }
        if !uses_control_auth_manager && current.stored_account_id != active_account_id {
            return;
        }
        let generation = self.inner.next_generation.fetch_add(1, Ordering::Relaxed);
        let replacement = match active_account_id.as_deref() {
            Some(account_id) => {
                Self::load_account(
                    &self.inner.config,
                    account_id,
                    Some(account_id),
                    Arc::clone(&self.inner.control_auth_manager),
                    generation,
                )
                .await
            }
            None => None,
        }
        .unwrap_or_else(|| {
            Arc::new(ExecutionAccount::from_control(
                active_account_id,
                Arc::clone(&self.inner.control_auth_manager),
                generation,
            ))
        });
        if !uses_control_auth_manager
            && !Arc::ptr_eq(&replacement.auth_manager, &self.inner.control_auth_manager)
        {
            return;
        }
        let account_id = replacement.stored_account_id.clone();
        self.inner.current.store(replacement);
        if self.inner.config.pooling.is_enabled()
            && let Some(account_id) = account_id.as_deref()
            && let Err(error) = persist_lease(
                &self.inner.config.codex_home,
                self.inner.thread_id,
                account_id,
            )
        {
            warn!("failed to persist reconciled execution account lease: {error}");
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_auth_manager_for_testing(&self, auth_manager: Arc<AuthManager>) {
        let stored_account_id = auth_manager
            .auth_cached()
            .and_then(|auth| auth.get_account_id());
        self.replace_auth_manager_for_testing_inner(stored_account_id, auth_manager);
    }

    #[cfg(test)]
    pub(crate) fn replace_with_detached_auth_manager_for_testing(
        &self,
        stored_account_id: String,
        auth_manager: Arc<AuthManager>,
    ) {
        self.replace_auth_manager_for_testing_inner(Some(stored_account_id), auth_manager);
    }

    #[cfg(test)]
    fn replace_auth_manager_for_testing_inner(
        &self,
        stored_account_id: Option<String>,
        auth_manager: Arc<AuthManager>,
    ) {
        let generation = self
            .inner
            .next_generation
            .fetch_add(/*val*/ 1, Ordering::Relaxed);
        self.inner
            .current
            .store(Arc::new(ExecutionAccount::from_control(
                stored_account_id,
                auth_manager,
                generation,
            )));
    }
}

impl ExecutionAccount {
    fn from_control(
        stored_account_id: Option<String>,
        auth_manager: Arc<AuthManager>,
        generation: u64,
    ) -> Self {
        let auth = auth_manager.auth_cached();
        Self {
            stored_account_id,
            label: None,
            mode: auth
                .as_ref()
                .map(CodexAuth::auth_mode)
                .unwrap_or(AuthMode::ApiKey),
            auth_manager,
            generation,
        }
    }

    fn from_stored(
        account: StoredAccount,
        auth_manager: Arc<AuthManager>,
        generation: u64,
    ) -> Self {
        Self {
            stored_account_id: Some(account.id),
            label: account.label,
            mode: account.mode,
            auth_manager,
            generation,
        }
    }

    fn identity(&self) -> ExecutionAccountIdentity {
        ExecutionAccountIdentity {
            stored_account_id: self.stored_account_id.clone(),
            label: self.label.clone(),
            mode: self.mode,
        }
    }
}

fn auth_matches_account(auth: &CodexAuth, account: &StoredAccount) -> bool {
    match (account.mode, auth.auth_mode()) {
        (AuthMode::ApiKey, AuthMode::ApiKey) => auth.api_key() == account.openai_api_key.as_deref(),
        (
            AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens,
            AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens,
        ) => {
            let Some(tokens) = account.tokens.as_ref() else {
                return false;
            };
            let Some(account_id) = tokens
                .account_id
                .as_deref()
                .or(tokens.id_token.chatgpt_account_id.as_deref())
            else {
                return false;
            };
            if auth.get_account_id().as_deref() != Some(account_id) {
                return false;
            }
            if let (Some(stored_user_id), Some(auth_user_id)) = (
                tokens.id_token.chatgpt_user_id.as_deref(),
                auth.get_chatgpt_user_id().as_deref(),
            ) && stored_user_id != auth_user_id
            {
                return false;
            }
            if let (Some(stored_email), Some(auth_email)) = (
                tokens.id_token.email.as_deref(),
                auth.get_account_email().as_deref(),
            ) && !stored_email.trim().eq_ignore_ascii_case(auth_email.trim())
            {
                return false;
            }
            true
        }
        _ => false,
    }
}

fn lease_path(codex_home: &Path, thread_id: ThreadId) -> PathBuf {
    codex_home
        .join(LEASE_SUBDIR)
        .join(format!("{thread_id}.json"))
}

fn read_persisted_lease(codex_home: &Path, thread_id: ThreadId) -> Option<String> {
    let path = lease_path(codex_home, thread_id);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            warn!(path = %path.display(), "failed to read execution account lease: {error}");
            return None;
        }
    };
    let persisted: PersistedExecutionAccountLease = match serde_json::from_slice(&contents) {
        Ok(persisted) => persisted,
        Err(error) => {
            warn!(path = %path.display(), "ignoring corrupt execution account lease: {error}");
            return None;
        }
    };
    if persisted.version != LEASE_VERSION || persisted.account_id.trim().is_empty() {
        warn!(path = %path.display(), "ignoring invalid execution account lease");
        return None;
    }
    Some(persisted.account_id)
}

fn persist_lease(codex_home: &Path, thread_id: ThreadId, account_id: &str) -> io::Result<()> {
    let path = lease_path(codex_home, thread_id);
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("lease path {} has no parent", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let persisted = PersistedExecutionAccountLease {
        version: LEASE_VERSION,
        account_id: account_id.to_string(),
    };
    let mut contents = serde_json::to_vec_pretty(&persisted).map_err(io::Error::other)?;
    contents.push(b'\n');
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
#[path = "execution_account_tests.rs"]
mod tests;
