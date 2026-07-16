mod archive_thread;
mod create_thread;
mod helpers;
mod list_threads;
mod live_writer;
mod read_thread;
mod search_threads;
mod thread_history_projection;
mod unarchive_thread;
mod update_thread_metadata;

#[cfg(test)]
mod test_support;

use async_trait::async_trait;
use codex_protocol::ThreadId;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::RolloutRecorder;
use codex_rollout::StateDbHandle;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::AppendThreadItemsParams;
use crate::ArchiveThreadParams;
use crate::CreateThreadParams;
use crate::ListThreadsParams;
use crate::LoadThreadHistoryParams;
use crate::ReadThreadByRolloutPathParams;
use crate::ReadThreadParams;
use crate::ResumeThreadParams;
use crate::SearchThreadsParams;
use crate::StoredThread;
use crate::StoredThreadHistory;
use crate::ThreadPage;
use crate::ThreadSearchPage;
use crate::ThreadStore;
use crate::ThreadStoreError;
use crate::ThreadStoreResult;
use crate::UpdateThreadMetadataParams;
use thread_history_projection::ThreadHistoryProjectionController;

/// Local filesystem/SQLite-backed implementation of [`ThreadStore`].
///
/// Local storage has two compatibility surfaces. Rollout JSONL files are the
/// durable replay format and remain readable without SQLite, including older
/// files that encode metadata in `SessionMeta` items and name-index entries.
/// The SQLite state DB, when available, is the queryable metadata index used by
/// list/read paths for fast lookup.
///
/// Live appends still write canonical JSONL history, but append-derived
/// metadata is observed above the store and applied through
/// [`ThreadStore::update_thread_metadata`]. This implementation applies that
/// patch literally to SQLite while keeping the JSONL/name-index compatibility
/// behavior needed for SQLite-less reads, repair, and old local rollout files.
#[derive(Clone)]
pub struct LocalThreadStore {
    pub(super) config: LocalThreadStoreConfig,
    live_recorders: Arc<Mutex<HashMap<ThreadId, LiveRecorderEntry>>>,
    state_db: Option<StateDbHandle>,
    history_projection: Arc<ThreadHistoryProjectionController>,
}

struct LiveRecorderEntry {
    recorder: RolloutRecorder,
    history_mode: ThreadHistoryMode,
}

/// Process-scoped configuration for local thread storage.
///
/// This describes where local storage lives. New-thread rollout metadata such
/// as cwd, provider, and memory mode is supplied when live persistence is opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalThreadStoreConfig {
    pub codex_home: PathBuf,
    pub sqlite_home: PathBuf,
    /// Provider used only when older local metadata does not contain one.
    pub default_model_provider_id: String,
}

impl LocalThreadStoreConfig {
    pub fn from_config(config: &impl codex_rollout::RolloutConfigView) -> Self {
        Self {
            codex_home: config.codex_home().to_path_buf(),
            sqlite_home: config.sqlite_home().to_path_buf(),
            default_model_provider_id: config.model_provider_id().to_string(),
        }
    }
}

impl std::fmt::Debug for LocalThreadStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalThreadStore")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl LocalThreadStore {
    /// Create a local store using an already initialized state DB handle.
    pub fn new(config: LocalThreadStoreConfig, state_db: Option<StateDbHandle>) -> Self {
        let history_projection = Arc::new(ThreadHistoryProjectionController::new(
            config.sqlite_home.clone(),
        ));
        Self {
            config,
            live_recorders: Arc::new(Mutex::new(HashMap::new())),
            state_db,
            history_projection,
        }
    }

    /// Return the state DB handle used by local rollout writers.
    pub async fn state_db(&self) -> Option<StateDbHandle> {
        self.state_db.clone()
    }

    /// Read a local rollout-backed thread by path.
    pub async fn read_thread_by_rollout_path(
        &self,
        rollout_path: PathBuf,
        include_archived: bool,
        include_history: bool,
    ) -> ThreadStoreResult<StoredThread> {
        read_thread::read_thread_by_rollout_path(
            self,
            rollout_path,
            include_archived,
            include_history,
        )
        .await
    }

    /// Return the live local rollout path for legacy local-only code paths.
    pub async fn live_rollout_path(&self, thread_id: ThreadId) -> ThreadStoreResult<PathBuf> {
        live_writer::rollout_path(self, thread_id).await
    }

    pub(super) async fn live_recorder(
        &self,
        thread_id: ThreadId,
    ) -> ThreadStoreResult<RolloutRecorder> {
        self.live_recorders
            .lock()
            .await
            .get(&thread_id)
            .map(|entry| entry.recorder.clone())
            .ok_or(ThreadStoreError::ThreadNotFound { thread_id })
    }

    pub(super) async fn live_history_mode(&self, thread_id: ThreadId) -> Option<ThreadHistoryMode> {
        self.live_recorders
            .lock()
            .await
            .get(&thread_id)
            .map(|entry| entry.history_mode)
    }

    pub(super) async fn ensure_live_recorder_absent(
        &self,
        thread_id: ThreadId,
    ) -> ThreadStoreResult<()> {
        if self.live_recorders.lock().await.contains_key(&thread_id) {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!("thread {thread_id} already has a live local writer"),
            });
        }
        Ok(())
    }

    pub(super) async fn insert_live_recorder(
        &self,
        thread_id: ThreadId,
        recorder: RolloutRecorder,
        history_mode: ThreadHistoryMode,
    ) -> ThreadStoreResult<()> {
        match self.live_recorders.lock().await.entry(thread_id) {
            Entry::Occupied(entry) => Err(ThreadStoreError::InvalidRequest {
                message: format!("thread {} already has a live local writer", entry.key()),
            }),
            Entry::Vacant(entry) => {
                entry.insert(LiveRecorderEntry {
                    recorder,
                    history_mode,
                });
                Ok(())
            }
        }
    }
}

#[async_trait]
impl ThreadStore for LocalThreadStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreResult<()> {
        live_writer::create_thread(self, params).await
    }

    async fn resume_thread(&self, params: ResumeThreadParams) -> ThreadStoreResult<()> {
        live_writer::resume_thread(self, params).await
    }

    async fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreResult<()> {
        live_writer::append_items(self, params).await
    }

    async fn persist_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        live_writer::persist_thread(self, thread_id).await
    }

    async fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        live_writer::flush_thread(self, thread_id).await
    }

    async fn shutdown_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        live_writer::shutdown_thread(self, thread_id).await
    }

    async fn discard_thread(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        live_writer::discard_thread(self, thread_id).await
    }

    async fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreResult<StoredThreadHistory> {
        if let Ok(rollout_path) = live_writer::rollout_path(self, params.thread_id).await {
            if !params.include_archived
                && helpers::rollout_path_is_managed_archived(
                    self.config.codex_home.as_path(),
                    rollout_path.as_path(),
                )
                .await
            {
                return Err(ThreadStoreError::InvalidRequest {
                    message: format!("thread {} is archived", params.thread_id),
                });
            }
            return read_thread::read_thread_by_rollout_path(
                self,
                rollout_path,
                /*include_archived*/ true,
                /*include_history*/ true,
            )
            .await?
            .history
            .ok_or_else(|| ThreadStoreError::Internal {
                message: format!("failed to load history for thread {}", params.thread_id),
            });
        }

        read_thread::read_thread(
            self,
            ReadThreadParams {
                thread_id: params.thread_id,
                include_archived: params.include_archived,
                include_history: true,
            },
        )
        .await?
        .history
        .ok_or_else(|| ThreadStoreError::Internal {
            message: format!("failed to load history for thread {}", params.thread_id),
        })
    }

    async fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreResult<StoredThread> {
        read_thread::read_thread(self, params).await
    }

    async fn read_thread_by_rollout_path(
        &self,
        params: ReadThreadByRolloutPathParams,
    ) -> ThreadStoreResult<StoredThread> {
        read_thread::read_thread_by_rollout_path(
            self,
            params.rollout_path,
            params.include_archived,
            params.include_history,
        )
        .await
    }

    async fn list_threads(&self, params: ListThreadsParams) -> ThreadStoreResult<ThreadPage> {
        list_threads::list_threads(self, params).await
    }

    async fn search_threads(
        &self,
        params: SearchThreadsParams,
    ) -> ThreadStoreResult<ThreadSearchPage> {
        search_threads::search_threads(self, params).await
    }

    async fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreResult<StoredThread> {
        update_thread_metadata::update_thread_metadata(self, params).await
    }

    async fn archive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreResult<()> {
        archive_thread::archive_thread(self, params).await
    }

    async fn unarchive_thread(
        &self,
        params: ArchiveThreadParams,
    ) -> ThreadStoreResult<StoredThread> {
        unarchive_thread::unarchive_thread(self, params).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use codex_protocol::ThreadId;
    use codex_protocol::items::TurnItem;
    use codex_protocol::items::UserMessageItem;
    use codex_protocol::models::BaseInstructions;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::ItemCompletedEvent;
    use codex_protocol::protocol::RolloutItem;
    use codex_protocol::protocol::RolloutLine;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::ThreadHistoryMode;
    use codex_protocol::protocol::ThreadMemoryMode;
    use codex_protocol::protocol::UserMessageEvent;
    use tempfile::TempDir;

    use super::*;
    use crate::LiveThread;
    use crate::ThreadMetadataPatch;
    use crate::ThreadPersistenceMetadata;
    use crate::local::test_support::test_config;
    use crate::local::test_support::write_archived_session_file;
    use crate::local::test_support::write_session_file;

    #[test]
    fn local_store_owns_one_lazy_history_repository_across_clones() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let history_path = codex_state::thread_history_db_path(config.sqlite_home.as_path());
        let store = LocalThreadStore::new(config, /*state_db*/ None);
        let cloned = store.clone();

        assert!(!history_path.exists());
        assert!(Arc::ptr_eq(
            &store.history_projection,
            &cloned.history_projection
        ));
    }

    #[tokio::test]
    async fn live_writer_lifecycle_writes_and_closes() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let history_path = codex_state::thread_history_db_path(config.sqlite_home.as_path());
        let store = LocalThreadStore::new(config, /*state_db*/ None);
        let thread_id = ThreadId::default();

        store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect("create live thread");
        let rollout_path = store
            .live_rollout_path(thread_id)
            .await
            .expect("load rollout path");

        store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("first live write")],
            })
            .await
            .expect("append live item");
        store
            .persist_thread(thread_id)
            .await
            .expect("persist live thread");
        store
            .flush_thread(thread_id)
            .await
            .expect("flush live thread");

        assert_rollout_contains_message(rollout_path.as_path(), "first live write").await;

        store
            .shutdown_thread(thread_id)
            .await
            .expect("shutdown live thread");
        let err = store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("write after shutdown")],
            })
            .await
            .expect_err("shutdown should remove the live thread writer");
        assert!(
            matches!(err, ThreadStoreError::ThreadNotFound { thread_id: missing } if missing == thread_id)
        );
        assert!(!history_path.exists());
    }

    #[tokio::test]
    async fn raw_append_items_does_not_update_sqlite_metadata() {
        // This pins the ThreadStore contract: raw appends are history-only. Callers that need
        // metadata updates must use LiveThread or call update_thread_metadata explicitly.
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let runtime = codex_state::StateRuntime::init(
            config.sqlite_home.clone(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = LocalThreadStore::new(config, Some(runtime.clone()));
        let thread_id = ThreadId::default();

        store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect("create live thread");
        store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("raw append")],
            })
            .await
            .expect("append raw item");
        store.flush_thread(thread_id).await.expect("flush thread");

        assert_eq!(
            runtime
                .get_thread(thread_id)
                .await
                .expect("sqlite metadata read"),
            None
        );
    }

    #[tokio::test]
    async fn live_thread_observes_appended_items_into_sqlite_metadata() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let runtime = codex_state::StateRuntime::init(
            config.sqlite_home.clone(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = Arc::new(LocalThreadStore::new(config, Some(runtime.clone())));
        let thread_id = ThreadId::default();
        let live_thread = LiveThread::create(store.clone(), create_thread_params(thread_id))
            .await
            .expect("create live thread");

        live_thread
            .append_items(&[user_message_item("observed append")])
            .await
            .expect("append observed item");
        live_thread.flush().await.expect("flush thread");

        let metadata = runtime
            .get_thread(thread_id)
            .await
            .expect("sqlite metadata read")
            .expect("sqlite metadata");
        assert_eq!(
            metadata.first_user_message.as_deref(),
            Some("observed append")
        );
        assert_eq!(metadata.preview.as_deref(), Some("observed append"));
        assert_eq!(metadata.title, "observed append");
    }

    #[tokio::test]
    async fn live_thread_shutdown_does_not_materialize_empty_thread_metadata() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let runtime = codex_state::StateRuntime::init(
            config.sqlite_home.clone(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = Arc::new(LocalThreadStore::new(config, Some(runtime.clone())));
        let thread_id = ThreadId::default();
        let live_thread = LiveThread::create(store.clone(), create_thread_params(thread_id))
            .await
            .expect("create live thread");
        let rollout_path = store
            .live_rollout_path(thread_id)
            .await
            .expect("live rollout path");

        live_thread.shutdown().await.expect("shutdown thread");

        assert!(
            !tokio::fs::try_exists(rollout_path.as_path())
                .await
                .expect("rollout path should be checkable")
        );
        assert_eq!(
            runtime
                .get_thread(thread_id)
                .await
                .expect("sqlite metadata read"),
            None
        );
    }

    #[tokio::test]
    async fn live_thread_shutdown_with_buffered_items_materializes_before_metadata_read() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let runtime = codex_state::StateRuntime::init(
            config.sqlite_home.clone(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = Arc::new(LocalThreadStore::new(config, Some(runtime.clone())));
        let thread_id = ThreadId::default();
        let live_thread = LiveThread::create(store.clone(), create_thread_params(thread_id))
            .await
            .expect("create live thread");
        let rollout_path = store
            .live_rollout_path(thread_id)
            .await
            .expect("live rollout path");

        live_thread
            .append_items(&[RolloutItem::EventMsg(EventMsg::TokenCount(
                codex_protocol::protocol::TokenCountEvent {
                    info: None,
                    rate_limits: None,
                },
            ))])
            .await
            .expect("append metadata-only item");
        live_thread.shutdown().await.expect("shutdown thread");

        assert!(
            tokio::fs::try_exists(rollout_path.as_path())
                .await
                .expect("rollout path should be checkable")
        );
        let metadata = runtime
            .get_thread(thread_id)
            .await
            .expect("sqlite metadata read")
            .expect("sqlite metadata");
        assert_eq!(metadata.rollout_path, rollout_path);
    }

    #[tokio::test]
    async fn live_thread_resume_loads_history_before_observing_metadata() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let runtime = codex_state::StateRuntime::init(
            config.sqlite_home.clone(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = Arc::new(LocalThreadStore::new(config, Some(runtime.clone())));
        let uuid = uuid::Uuid::from_u128(401);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let rollout_path =
            write_session_file(home.path(), "2025-01-03T17-00-00", uuid).expect("session file");
        let live_thread = LiveThread::resume(
            store,
            ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path),
                history: None,
                include_archived: false,
                metadata: ThreadPersistenceMetadata {
                    cwd: Some(home.path().to_path_buf()),
                    model_provider: "different-provider".to_string(),
                    history_mode: ThreadHistoryMode::Legacy,
                    memory_mode: ThreadMemoryMode::Enabled,
                },
            },
        )
        .await
        .expect("resume live thread");

        live_thread
            .append_items(&[user_message_item("new live append")])
            .await
            .expect("append after resume");

        let metadata = runtime
            .get_thread(thread_id)
            .await
            .expect("sqlite metadata read")
            .expect("sqlite metadata");
        assert_eq!(
            metadata.created_at.to_rfc3339(),
            "2025-01-03T17:00:00+00:00"
        );
        assert_eq!(metadata.model_provider, "test-provider");
        assert_eq!(
            metadata.first_user_message.as_deref(),
            Some("Hello from user")
        );
    }

    #[tokio::test]
    async fn live_thread_resume_loads_history_from_explicit_external_rollout_path() {
        let home = TempDir::new().expect("temp dir");
        let external_home = TempDir::new().expect("external temp dir");
        let config = test_config(home.path());
        let runtime = codex_state::StateRuntime::init(
            config.sqlite_home.clone(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = Arc::new(LocalThreadStore::new(config, Some(runtime.clone())));
        let uuid = uuid::Uuid::from_u128(402);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let rollout_path = write_session_file(external_home.path(), "2025-01-03T17-30-00", uuid)
            .expect("external session file");
        let live_thread = LiveThread::resume(
            store,
            ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path),
                history: None,
                include_archived: false,
                metadata: ThreadPersistenceMetadata {
                    cwd: Some(home.path().to_path_buf()),
                    model_provider: "different-provider".to_string(),
                    history_mode: ThreadHistoryMode::Legacy,
                    memory_mode: ThreadMemoryMode::Enabled,
                },
            },
        )
        .await
        .expect("resume external live thread");

        live_thread
            .append_items(&[user_message_item("new external append")])
            .await
            .expect("append after external resume");

        let metadata = runtime
            .get_thread(thread_id)
            .await
            .expect("sqlite metadata read")
            .expect("sqlite metadata");
        assert_eq!(
            metadata.created_at.to_rfc3339(),
            "2025-01-03T17:30:00+00:00"
        );
        assert_eq!(metadata.model_provider, "test-provider");
        assert_eq!(
            metadata.first_user_message.as_deref(),
            Some("Hello from user")
        );
    }

    #[tokio::test]
    async fn create_thread_rejects_missing_cwd() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let thread_id = ThreadId::default();
        let mut params = create_thread_params(thread_id);
        params.metadata.cwd = None;

        let err = store
            .create_thread(params)
            .await
            .expect_err("local thread store should require cwd");

        assert!(matches!(
            err,
            ThreadStoreError::InvalidRequest { message }
                if message == "local thread store requires a cwd"
        ));
    }

    #[tokio::test]
    async fn paginated_live_appends_use_paginated_history_mode_and_ordinals() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let thread_id = ThreadId::default();
        let mut params = create_thread_params(thread_id);
        params.history_mode = ThreadHistoryMode::Paginated;
        params.metadata.history_mode = ThreadHistoryMode::Paginated;

        store
            .create_thread(params)
            .await
            .expect("create paginated thread");
        store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![
                    user_message_item("legacy event should not persist"),
                    paginated_user_message_item(thread_id, "turn-1", "item-1"),
                ],
            })
            .await
            .expect("append paginated item");
        let rollout_path = store
            .live_rollout_path(thread_id)
            .await
            .expect("paginated rollout path");
        let contents = tokio::fs::read_to_string(rollout_path)
            .await
            .expect("read paginated rollout");
        let lines = contents
            .lines()
            .map(|line| serde_json::from_str::<RolloutLine>(line).expect("rollout line"))
            .collect::<Vec<_>>();

        assert_eq!(
            lines.iter().map(|line| line.ordinal).collect::<Vec<_>>(),
            vec![Some(0), Some(1)]
        );
        assert!(matches!(
            &lines[1].item,
            RolloutItem::EventMsg(EventMsg::ItemCompleted(event)) if event.turn_id == "turn-1"
        ));
        assert!(!contents.contains("legacy event should not persist"));
        assert_eq!(
            store
                .history_projection
                .repository()
                .checkpoint(&thread_id.to_string())
                .await
                .expect("projection checkpoint")
                .expect("projection checkpoint row")
                .projection_status,
            codex_state::ThreadHistoryProjectionStatus::Dirty
        );
    }

    #[tokio::test]
    async fn discard_thread_drops_unmaterialized_live_writer() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let thread_id = ThreadId::default();

        store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect("create live thread");
        let rollout_path = store
            .live_rollout_path(thread_id)
            .await
            .expect("load rollout path");
        store
            .discard_thread(thread_id)
            .await
            .expect("discard live thread");

        assert!(
            !tokio::fs::try_exists(rollout_path.as_path())
                .await
                .expect("check rollout path")
        );
        let err = store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("write after discard")],
            })
            .await
            .expect_err("discard should remove the live thread writer");
        assert!(
            matches!(err, ThreadStoreError::ThreadNotFound { thread_id: missing } if missing == thread_id)
        );
    }

    #[tokio::test]
    async fn resume_thread_reopens_live_writer_and_appends() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let thread_id = ThreadId::default();

        let first_store = LocalThreadStore::new(config.clone(), /*state_db*/ None);
        first_store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect("create initial thread");
        first_store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("before resume")],
            })
            .await
            .expect("append initial item");
        first_store
            .persist_thread(thread_id)
            .await
            .expect("persist initial thread");
        first_store
            .flush_thread(thread_id)
            .await
            .expect("flush initial thread");
        let rollout_path = first_store
            .live_rollout_path(thread_id)
            .await
            .expect("load rollout path");
        first_store
            .shutdown_thread(thread_id)
            .await
            .expect("shutdown initial writer");

        let resumed_store = LocalThreadStore::new(config.clone(), /*state_db*/ None);
        resumed_store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: None,
                history: None,
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect("resume live thread");
        resumed_store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("after resume")],
            })
            .await
            .expect("append resumed item");
        resumed_store
            .flush_thread(thread_id)
            .await
            .expect("flush resumed thread");

        assert_rollout_contains_message(rollout_path.as_path(), "before resume").await;
        assert_rollout_contains_message(rollout_path.as_path(), "after resume").await;
    }

    #[tokio::test]
    async fn create_thread_rejects_duplicate_live_writer() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let thread_id = ThreadId::default();

        store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect("create live thread");

        let err = store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect_err("duplicate live writer should fail");

        assert!(matches!(err, ThreadStoreError::InvalidRequest { .. }));
        assert!(err.to_string().contains("already has a live local writer"));
    }

    #[tokio::test]
    async fn resume_thread_rejects_duplicate_live_writer() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let thread_id = ThreadId::default();

        store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect("create live thread");
        let rollout_path = store
            .live_rollout_path(thread_id)
            .await
            .expect("live rollout path");
        let err = store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path),
                history: None,
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect_err("duplicate live resume should fail");
        assert!(matches!(err, ThreadStoreError::InvalidRequest { .. }));
        assert!(err.to_string().contains("already has a live local writer"));
    }

    #[tokio::test]
    async fn resume_thread_rejects_missing_cwd() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let uuid = uuid::Uuid::from_u128(407);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let rollout_path =
            write_session_file(home.path(), "2025-01-04T11-30-00", uuid).expect("session file");
        let err = store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path),
                history: None,
                include_archived: true,
                metadata: ThreadPersistenceMetadata {
                    cwd: None,
                    model_provider: "test-provider".to_string(),
                    history_mode: ThreadHistoryMode::Legacy,
                    memory_mode: ThreadMemoryMode::Enabled,
                },
            })
            .await
            .expect_err("missing cwd should fail");

        assert!(matches!(err, ThreadStoreError::InvalidRequest { .. }));
        assert!(err.to_string().contains("requires a cwd"));
    }

    #[tokio::test]
    async fn resume_thread_rejects_paginated_history_mode() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let thread_id = ThreadId::default();
        let mut create_params = create_thread_params(thread_id);
        create_params.history_mode = ThreadHistoryMode::Paginated;
        create_params.metadata.history_mode = ThreadHistoryMode::Paginated;
        let first_store = LocalThreadStore::new(config.clone(), /*state_db*/ None);
        first_store
            .create_thread(create_params)
            .await
            .expect("create paginated thread");
        first_store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![paginated_user_message_item(thread_id, "turn-1", "item-1")],
            })
            .await
            .expect("append paginated item");
        let rollout_path = first_store
            .live_rollout_path(thread_id)
            .await
            .expect("paginated rollout path");
        first_store
            .shutdown_thread(thread_id)
            .await
            .expect("shutdown paginated writer");

        let store = LocalThreadStore::new(config, /*state_db*/ None);
        let err = store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path),
                history: None,
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect_err("history-less paginated resume should remain blocked");

        assert!(matches!(
            err,
            ThreadStoreError::UnsupportedHistoryMode {
                thread_id: rejected_thread_id,
                history_mode: ThreadHistoryMode::Paginated,
                operation: "resume_thread",
            } if rejected_thread_id == thread_id
        ));
    }

    #[tokio::test]
    async fn resume_paginated_thread_with_history_advances_ordinal() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let thread_id = ThreadId::default();
        let mut create_params = create_thread_params(thread_id);
        create_params.history_mode = ThreadHistoryMode::Paginated;
        create_params.metadata.history_mode = ThreadHistoryMode::Paginated;

        let first_store = LocalThreadStore::new(config.clone(), /*state_db*/ None);
        first_store
            .create_thread(create_params)
            .await
            .expect("create paginated thread");
        first_store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![paginated_user_message_item(thread_id, "turn-1", "item-1")],
            })
            .await
            .expect("append initial paginated item");
        let rollout_path = first_store
            .live_rollout_path(thread_id)
            .await
            .expect("paginated rollout path");
        first_store
            .shutdown_thread(thread_id)
            .await
            .expect("shutdown initial writer");

        let resumed_store = LocalThreadStore::new(config.clone(), /*state_db*/ None);
        resumed_store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path.clone()),
                history: Some(Vec::new().into()),
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect("resume paginated writer with explicit history");
        resumed_store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![paginated_user_message_item(thread_id, "turn-2", "item-2")],
            })
            .await
            .expect("append resumed paginated item");
        resumed_store
            .flush_thread(thread_id)
            .await
            .expect("flush resumed paginated writer");
        resumed_store
            .shutdown_thread(thread_id)
            .await
            .expect("shutdown resumed paginated writer");

        let second_resumed_store = LocalThreadStore::new(config, /*state_db*/ None);
        second_resumed_store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path.clone()),
                history: Some(Vec::new().into()),
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect("resume paginated writer a second time");
        second_resumed_store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![paginated_user_message_item(thread_id, "turn-3", "item-3")],
            })
            .await
            .expect("append second resumed paginated item");
        second_resumed_store
            .flush_thread(thread_id)
            .await
            .expect("flush second resumed paginated writer");

        let contents = tokio::fs::read_to_string(rollout_path)
            .await
            .expect("read resumed rollout");
        let lines = contents
            .lines()
            .map(|line| serde_json::from_str::<RolloutLine>(line).expect("rollout line"))
            .collect::<Vec<_>>();
        assert_eq!(
            lines.iter().map(|line| line.ordinal).collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2), Some(3)]
        );
        assert!(matches!(
            &lines[3].item,
            RolloutItem::EventMsg(EventMsg::ItemCompleted(event)) if event.turn_id == "turn-3"
        ));
    }

    #[tokio::test]
    async fn metadata_seed_uses_live_paginated_history_mode() {
        let home = TempDir::new().expect("temp dir");
        let config = test_config(home.path());
        let runtime = codex_state::StateRuntime::init(
            config.sqlite_home.clone(),
            config.default_model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let store = LocalThreadStore::new(config, Some(runtime.clone()));
        let thread_id = ThreadId::default();
        let mut create_params = create_thread_params(thread_id);
        create_params.history_mode = ThreadHistoryMode::Paginated;
        create_params.metadata.history_mode = ThreadHistoryMode::Paginated;
        store
            .create_thread(create_params)
            .await
            .expect("create deferred paginated thread");

        store
            .update_thread_metadata(UpdateThreadMetadataParams {
                thread_id,
                patch: ThreadMetadataPatch {
                    preview: Some("paginated preview".to_string()),
                    ..Default::default()
                },
                include_archived: false,
            })
            .await
            .expect("seed paginated metadata");

        let metadata = runtime
            .get_thread(thread_id)
            .await
            .expect("sqlite metadata read")
            .expect("sqlite metadata");
        assert_eq!(metadata.history_mode, ThreadHistoryMode::Paginated);
    }

    #[tokio::test]
    async fn load_history_uses_live_writer_rollout_path() {
        let home = TempDir::new().expect("temp dir");
        let external_home = TempDir::new().expect("external temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let uuid = uuid::Uuid::from_u128(404);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let rollout_path = write_session_file(external_home.path(), "2025-01-04T10-00-00", uuid)
            .expect("external session file");

        store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path),
                history: None,
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect("resume live thread");
        store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("external history item")],
            })
            .await
            .expect("append live item");
        store
            .flush_thread(thread_id)
            .await
            .expect("flush live thread");

        let history = store
            .load_history(LoadThreadHistoryParams {
                thread_id,
                include_archived: false,
            })
            .await
            .expect("load external live history");

        assert!(history.items.iter().any(|item| {
            matches!(
                item,
                RolloutItem::EventMsg(EventMsg::UserMessage(event)) if event.message == "external history item"
            )
        }));
    }

    #[tokio::test]
    async fn read_thread_uses_live_writer_rollout_path_for_external_resume() {
        let home = TempDir::new().expect("temp dir");
        let external_home = TempDir::new().expect("external temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let uuid = uuid::Uuid::from_u128(406);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let rollout_path = write_session_file(external_home.path(), "2025-01-04T11-00-00", uuid)
            .expect("external session file");

        store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path.clone()),
                history: None,
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect("resume live thread");

        let thread = store
            .read_thread(ReadThreadParams {
                thread_id,
                include_archived: false,
                include_history: true,
            })
            .await
            .expect("read external live thread");

        assert_eq!(thread.rollout_path, Some(rollout_path));
        assert!(thread.history.expect("history").items.iter().any(|item| {
            matches!(
                item,
                RolloutItem::EventMsg(EventMsg::UserMessage(event)) if event.message == "Hello from user"
            )
        }));
    }

    #[tokio::test]
    async fn load_history_uses_live_writer_rollout_path_for_archived_source() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let uuid = uuid::Uuid::from_u128(405);
        let thread_id = ThreadId::from_string(&uuid.to_string()).expect("valid thread id");
        let rollout_path = write_archived_session_file(home.path(), "2025-01-04T10-30-00", uuid)
            .expect("archived session file");

        store
            .resume_thread(ResumeThreadParams {
                thread_id,
                rollout_path: Some(rollout_path),
                history: None,
                include_archived: true,
                metadata: thread_metadata(),
            })
            .await
            .expect("resume live archived thread");
        store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("archived live history item")],
            })
            .await
            .expect("append live item");
        store
            .flush_thread(thread_id)
            .await
            .expect("flush live thread");

        let err = store
            .read_thread(ReadThreadParams {
                thread_id,
                include_archived: false,
                include_history: false,
            })
            .await
            .expect_err("active-only read should reject archived live thread");
        assert!(matches!(err, ThreadStoreError::InvalidRequest { .. }));

        let err = store
            .load_history(LoadThreadHistoryParams {
                thread_id,
                include_archived: false,
            })
            .await
            .expect_err("active-only history should reject archived live thread");
        assert!(matches!(err, ThreadStoreError::InvalidRequest { .. }));
        assert!(err.to_string().contains("archived"));

        let history = store
            .load_history(LoadThreadHistoryParams {
                thread_id,
                include_archived: true,
            })
            .await
            .expect("load archived live history");

        assert!(history.items.iter().any(|item| {
            matches!(
                item,
                RolloutItem::EventMsg(EventMsg::UserMessage(event)) if event.message == "archived live history item"
            )
        }));
    }

    #[tokio::test]
    async fn read_thread_by_rollout_path_includes_history() {
        let home = TempDir::new().expect("temp dir");
        let store = LocalThreadStore::new(test_config(home.path()), /*state_db*/ None);
        let thread_id = ThreadId::default();

        store
            .create_thread(create_thread_params(thread_id))
            .await
            .expect("create thread");
        store
            .append_items(AppendThreadItemsParams {
                thread_id,
                items: vec![user_message_item("path read")],
            })
            .await
            .expect("append item");
        store.flush_thread(thread_id).await.expect("flush thread");
        let rollout_path = store
            .live_rollout_path(thread_id)
            .await
            .expect("load rollout path");

        let thread = store
            .read_thread_by_rollout_path(
                rollout_path,
                /*include_archived*/ true,
                /*include_history*/ true,
            )
            .await
            .expect("read thread by rollout path");

        assert_eq!(thread.thread_id, thread_id);
        assert_eq!(
            thread
                .history
                .expect("history")
                .items
                .iter()
                .filter(|item| matches!(item, RolloutItem::EventMsg(EventMsg::UserMessage(_))))
                .count(),
            1
        );
    }

    fn create_thread_params(thread_id: ThreadId) -> CreateThreadParams {
        CreateThreadParams {
            session_id: thread_id.into(),
            thread_id,
            forked_from_id: None,
            parent_thread_id: None,
            source: SessionSource::Exec,
            session_provenance: None,
            thread_source: None,
            base_instructions: BaseInstructions::default(),
            dynamic_tools: Vec::new(),
            multi_agent_version: None,
            history_mode: ThreadHistoryMode::Legacy,
            initial_window_id: "019b0000-0000-7000-8000-000000000000".to_string(),
            metadata: thread_metadata(),
        }
    }

    fn thread_metadata() -> ThreadPersistenceMetadata {
        ThreadPersistenceMetadata {
            cwd: Some(std::env::current_dir().expect("cwd")),
            model_provider: "test-provider".to_string(),
            history_mode: ThreadHistoryMode::Legacy,
            memory_mode: ThreadMemoryMode::Enabled,
        }
    }

    fn user_message_item(message: &str) -> RolloutItem {
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: message.to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
            ..Default::default()
        }))
    }

    fn paginated_user_message_item(
        thread_id: ThreadId,
        turn_id: &str,
        item_id: &str,
    ) -> RolloutItem {
        RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item: TurnItem::UserMessage(UserMessageItem {
                id: item_id.to_string(),
                client_id: None,
                content: Vec::new(),
            }),
            completed_at_ms: 1,
        }))
    }

    async fn assert_rollout_contains_message(path: &std::path::Path, expected: &str) {
        let (items, _, _) = RolloutRecorder::load_rollout_items(path)
            .await
            .expect("load rollout items");
        assert!(items.iter().any(|item| {
            matches!(
                item,
                RolloutItem::EventMsg(EventMsg::UserMessage(event)) if event.message == expected
            )
        }));
    }
}
