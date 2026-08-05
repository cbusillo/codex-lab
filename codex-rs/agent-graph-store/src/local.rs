use codex_protocol::ThreadId;
use codex_state::StateRuntime;
use std::sync::Arc;

use crate::AgentGraphStore;
use crate::AgentGraphStoreError;
use crate::AgentGraphStoreFuture;
use crate::ExternalAgentRunOutcome;
use crate::ExternalAgentRunRecord;
use crate::ExternalAgentRunStart;
use crate::ThreadSpawnEdgeStatus;

/// SQLite-backed implementation of [`AgentGraphStore`] using an existing state runtime.
#[derive(Clone)]
pub struct LocalAgentGraphStore {
    state_db: Arc<StateRuntime>,
}

impl std::fmt::Debug for LocalAgentGraphStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalAgentGraphStore")
            .field("sqlite", self.state_db.sqlite())
            .finish_non_exhaustive()
    }
}

impl LocalAgentGraphStore {
    /// Create a local graph store from an already-initialized state runtime.
    pub fn new(state_db: Arc<StateRuntime>) -> Self {
        Self { state_db }
    }
}

impl AgentGraphStore for LocalAgentGraphStore {
    fn insert_external_agent_run(
        &self,
        run: ExternalAgentRunStart,
    ) -> AgentGraphStoreFuture<'_, ()> {
        Box::pin(async move {
            self.state_db
                .insert_external_agent_run(to_state_run_start(run))
                .await
                .map_err(internal_error)
        })
    }

    fn finish_external_agent_run(
        &self,
        child_thread_id: ThreadId,
        outcome: ExternalAgentRunOutcome,
    ) -> AgentGraphStoreFuture<'_, ()> {
        Box::pin(async move {
            self.state_db
                .finish_external_agent_run(child_thread_id, to_state_run_outcome(outcome))
                .await
                .map_err(internal_error)
        })
    }

    fn list_external_agent_runs(
        &self,
        parent_thread_id: ThreadId,
    ) -> AgentGraphStoreFuture<'_, Vec<ExternalAgentRunRecord>> {
        Box::pin(async move {
            self.state_db
                .list_external_agent_runs(parent_thread_id)
                .await
                .map(|runs| runs.into_iter().map(from_state_run).collect())
                .map_err(internal_error)
        })
    }

    fn upsert_thread_spawn_edge(
        &self,
        parent_thread_id: ThreadId,
        child_thread_id: ThreadId,
        status: ThreadSpawnEdgeStatus,
    ) -> AgentGraphStoreFuture<'_, ()> {
        Box::pin(async move {
            self.state_db
                .upsert_thread_spawn_edge(
                    parent_thread_id,
                    child_thread_id,
                    to_state_status(status),
                )
                .await
                .map_err(internal_error)
        })
    }

    fn set_thread_spawn_edge_status(
        &self,
        child_thread_id: ThreadId,
        status: ThreadSpawnEdgeStatus,
    ) -> AgentGraphStoreFuture<'_, ()> {
        Box::pin(async move {
            self.state_db
                .set_thread_spawn_edge_status(child_thread_id, to_state_status(status))
                .await
                .map_err(internal_error)
        })
    }

    fn list_thread_spawn_children(
        &self,
        parent_thread_id: ThreadId,
        status_filter: Option<ThreadSpawnEdgeStatus>,
    ) -> AgentGraphStoreFuture<'_, Vec<ThreadId>> {
        Box::pin(async move {
            if let Some(status) = status_filter {
                return self
                    .state_db
                    .list_thread_spawn_children_with_status(
                        parent_thread_id,
                        to_state_status(status),
                    )
                    .await
                    .map_err(internal_error);
            }

            self.state_db
                .list_thread_spawn_children(parent_thread_id)
                .await
                .map_err(internal_error)
        })
    }

    fn list_thread_spawn_descendants(
        &self,
        root_thread_id: ThreadId,
        status_filter: Option<ThreadSpawnEdgeStatus>,
    ) -> AgentGraphStoreFuture<'_, Vec<ThreadId>> {
        Box::pin(async move {
            match status_filter {
                Some(status) => self
                    .state_db
                    .list_thread_spawn_descendants_with_status(
                        root_thread_id,
                        to_state_status(status),
                    )
                    .await
                    .map_err(internal_error),
                None => self
                    .state_db
                    .list_thread_spawn_descendants(root_thread_id)
                    .await
                    .map_err(internal_error),
            }
        })
    }
}

fn to_state_run_start(run: ExternalAgentRunStart) -> codex_state::ExternalAgentRunStart {
    codex_state::ExternalAgentRunStart {
        child_thread_id: run.child_thread_id,
        parent_thread_id: run.parent_thread_id,
        agent_path: run.agent_path,
        routing_kind: run.routing_kind,
        requested_selector: run.requested_selector,
        effective_selector: run.effective_selector,
        routing_reason: run.routing_reason,
        skipped_candidates_json: run.skipped_candidates_json,
        provider_family: run.provider_family,
        command: run.command,
        cli_version: run.cli_version,
        capability_source: run.capability_source,
        capability_freshness: run.capability_freshness,
        protocol: run.protocol,
        mode: run.mode,
        workspace: run.workspace,
        model: run.model,
        effort: run.effort,
        started_at_ms: run.started_at_ms,
    }
}

fn to_state_run_outcome(outcome: ExternalAgentRunOutcome) -> codex_state::ExternalAgentRunOutcome {
    codex_state::ExternalAgentRunOutcome {
        completed_at_ms: outcome.completed_at_ms,
        duration_ms: outcome.duration_ms,
        terminal_state: outcome.terminal_state,
        failure_kind: outcome.failure_kind,
        failure_message: outcome.failure_message,
    }
}

fn from_state_run(run: codex_state::ExternalAgentRun) -> ExternalAgentRunRecord {
    ExternalAgentRunRecord {
        child_thread_id: run.child_thread_id,
        parent_thread_id: run.parent_thread_id,
        agent_path: run.agent_path,
        routing_kind: run.routing_kind,
        requested_selector: run.requested_selector,
        effective_selector: run.effective_selector,
        routing_reason: run.routing_reason,
        skipped_candidates_json: run.skipped_candidates_json,
        provider_family: run.provider_family,
        command: run.command,
        cli_version: run.cli_version,
        capability_source: run.capability_source,
        capability_freshness: run.capability_freshness,
        protocol: run.protocol,
        mode: run.mode,
        workspace: run.workspace,
        model: run.model,
        effort: run.effort,
        started_at_ms: run.started_at_ms,
        completed_at_ms: run.completed_at_ms,
        duration_ms: run.duration_ms,
        terminal_state: run.terminal_state,
        failure_kind: run.failure_kind,
        failure_message: run.failure_message,
    }
}

fn to_state_status(status: ThreadSpawnEdgeStatus) -> codex_state::DirectionalThreadSpawnEdgeStatus {
    match status {
        ThreadSpawnEdgeStatus::Open => codex_state::DirectionalThreadSpawnEdgeStatus::Open,
        ThreadSpawnEdgeStatus::Closed => codex_state::DirectionalThreadSpawnEdgeStatus::Closed,
    }
}

fn internal_error(err: impl std::fmt::Display) -> AgentGraphStoreError {
    AgentGraphStoreError::Internal {
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_state::DirectionalThreadSpawnEdgeStatus;
    use codex_utils_absolute_path::test_support::PathExt;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    struct TestRuntime {
        state_db: Arc<StateRuntime>,
        _codex_home: TempDir,
    }

    fn thread_id(suffix: u128) -> ThreadId {
        ThreadId::from_string(&format!("00000000-0000-0000-0000-{suffix:012}"))
            .expect("valid thread id")
    }

    async fn state_runtime() -> TestRuntime {
        let codex_home = TempDir::new().expect("tempdir should be created");
        let state_db = StateRuntime::init(
            codex_state::SqliteConfig::new_for_testing(codex_home.path().abs()),
            "test-provider".to_string(),
        )
        .await
        .expect("state db should initialize");
        TestRuntime {
            state_db,
            _codex_home: codex_home,
        }
    }

    #[tokio::test]
    async fn local_store_upserts_and_lists_direct_children_with_status_filters() {
        let fixture = state_runtime().await;
        let state_db = fixture.state_db;
        let store = LocalAgentGraphStore::new(state_db.clone());
        let parent_thread_id = thread_id(/*suffix*/ 1);
        let first_child_thread_id = thread_id(/*suffix*/ 2);
        let second_child_thread_id = thread_id(/*suffix*/ 3);

        store
            .upsert_thread_spawn_edge(
                parent_thread_id,
                second_child_thread_id,
                ThreadSpawnEdgeStatus::Closed,
            )
            .await
            .expect("closed child edge should insert");
        store
            .upsert_thread_spawn_edge(
                parent_thread_id,
                first_child_thread_id,
                ThreadSpawnEdgeStatus::Open,
            )
            .await
            .expect("open child edge should insert");

        let all_children = store
            .list_thread_spawn_children(parent_thread_id, /*status_filter*/ None)
            .await
            .expect("all children should load");
        assert_eq!(
            all_children,
            vec![first_child_thread_id, second_child_thread_id]
        );

        let open_children = store
            .list_thread_spawn_children(parent_thread_id, Some(ThreadSpawnEdgeStatus::Open))
            .await
            .expect("open children should load");
        let state_open_children = state_db
            .list_thread_spawn_children_with_status(
                parent_thread_id,
                DirectionalThreadSpawnEdgeStatus::Open,
            )
            .await
            .expect("state open children should load");
        assert_eq!(open_children, state_open_children);
        assert_eq!(open_children, vec![first_child_thread_id]);

        let closed_children = store
            .list_thread_spawn_children(parent_thread_id, Some(ThreadSpawnEdgeStatus::Closed))
            .await
            .expect("closed children should load");
        assert_eq!(closed_children, vec![second_child_thread_id]);
    }

    #[tokio::test]
    async fn local_store_updates_edge_status() {
        let fixture = state_runtime().await;
        let state_db = fixture.state_db;
        let store = LocalAgentGraphStore::new(state_db);
        let parent_thread_id = thread_id(/*suffix*/ 10);
        let child_thread_id = thread_id(/*suffix*/ 11);

        store
            .upsert_thread_spawn_edge(
                parent_thread_id,
                child_thread_id,
                ThreadSpawnEdgeStatus::Open,
            )
            .await
            .expect("child edge should insert");
        store
            .set_thread_spawn_edge_status(child_thread_id, ThreadSpawnEdgeStatus::Closed)
            .await
            .expect("child edge should close");

        let open_children = store
            .list_thread_spawn_children(parent_thread_id, Some(ThreadSpawnEdgeStatus::Open))
            .await
            .expect("open children should load");
        assert_eq!(open_children, Vec::<ThreadId>::new());

        let closed_children = store
            .list_thread_spawn_children(parent_thread_id, Some(ThreadSpawnEdgeStatus::Closed))
            .await
            .expect("closed children should load");
        assert_eq!(closed_children, vec![child_thread_id]);
    }

    #[tokio::test]
    async fn local_store_persists_external_agent_run_lifecycle() {
        let fixture = state_runtime().await;
        let store = LocalAgentGraphStore::new(fixture.state_db);
        let parent_thread_id = thread_id(/*suffix*/ 12);
        let child_thread_id = thread_id(/*suffix*/ 13);
        store
            .insert_external_agent_run(ExternalAgentRunStart {
                child_thread_id,
                parent_thread_id,
                agent_path: Some("worker/reviewer".to_string()),
                routing_kind: "automatic_external".to_string(),
                requested_selector: None,
                effective_selector: "claude-sonnet-4.6".to_string(),
                routing_reason: "selected first eligible candidate".to_string(),
                skipped_candidates_json: "[]".to_string(),
                provider_family: Some("claude".to_string()),
                command: "claude".to_string(),
                cli_version: Some("1.2.3".to_string()),
                capability_source: "local_cli".to_string(),
                capability_freshness: Some("fresh".to_string()),
                protocol: "raw_cli".to_string(),
                mode: "read_only".to_string(),
                workspace: "/workspace".to_string(),
                model: Some("claude-sonnet-4.6".to_string()),
                effort: Some("high".to_string()),
                started_at_ms: 100,
            })
            .await
            .expect("external run should insert");
        store
            .finish_external_agent_run(
                child_thread_id,
                ExternalAgentRunOutcome {
                    completed_at_ms: 175,
                    duration_ms: 75,
                    terminal_state: "completed".to_string(),
                    failure_kind: None,
                    failure_message: None,
                },
            )
            .await
            .expect("external run should finish");

        let runs = store
            .list_external_agent_runs(parent_thread_id)
            .await
            .expect("external runs should load");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].child_thread_id, child_thread_id);
        assert_eq!(runs[0].duration_ms, Some(75));
        assert_eq!(runs[0].terminal_state.as_deref(), Some("completed"));
        assert_eq!(runs[0].capability_source, "local_cli");
    }

    #[tokio::test]
    async fn local_store_lists_descendants_breadth_first_with_status_filters() {
        let fixture = state_runtime().await;
        let state_db = fixture.state_db;
        let store = LocalAgentGraphStore::new(state_db.clone());
        let root_thread_id = thread_id(/*suffix*/ 20);
        let later_child_thread_id = thread_id(/*suffix*/ 22);
        let earlier_child_thread_id = thread_id(/*suffix*/ 21);
        let closed_grandchild_thread_id = thread_id(/*suffix*/ 23);
        let open_grandchild_thread_id = thread_id(/*suffix*/ 24);
        let closed_child_thread_id = thread_id(/*suffix*/ 25);
        let closed_great_grandchild_thread_id = thread_id(/*suffix*/ 26);

        for (parent_thread_id, child_thread_id, status) in [
            (
                root_thread_id,
                later_child_thread_id,
                ThreadSpawnEdgeStatus::Open,
            ),
            (
                root_thread_id,
                earlier_child_thread_id,
                ThreadSpawnEdgeStatus::Open,
            ),
            (
                earlier_child_thread_id,
                open_grandchild_thread_id,
                ThreadSpawnEdgeStatus::Open,
            ),
            (
                later_child_thread_id,
                closed_grandchild_thread_id,
                ThreadSpawnEdgeStatus::Closed,
            ),
            (
                root_thread_id,
                closed_child_thread_id,
                ThreadSpawnEdgeStatus::Closed,
            ),
            (
                closed_child_thread_id,
                closed_great_grandchild_thread_id,
                ThreadSpawnEdgeStatus::Closed,
            ),
        ] {
            store
                .upsert_thread_spawn_edge(parent_thread_id, child_thread_id, status)
                .await
                .expect("edge should insert");
        }

        let all_descendants = store
            .list_thread_spawn_descendants(root_thread_id, /*status_filter*/ None)
            .await
            .expect("all descendants should load");
        assert_eq!(
            all_descendants,
            vec![
                earlier_child_thread_id,
                later_child_thread_id,
                closed_child_thread_id,
                closed_grandchild_thread_id,
                open_grandchild_thread_id,
                closed_great_grandchild_thread_id,
            ]
        );

        let open_descendants = store
            .list_thread_spawn_descendants(root_thread_id, Some(ThreadSpawnEdgeStatus::Open))
            .await
            .expect("open descendants should load");
        let state_open_descendants = state_db
            .list_thread_spawn_descendants_with_status(
                root_thread_id,
                DirectionalThreadSpawnEdgeStatus::Open,
            )
            .await
            .expect("state open descendants should load");
        assert_eq!(open_descendants, state_open_descendants);
        assert_eq!(
            open_descendants,
            vec![
                earlier_child_thread_id,
                later_child_thread_id,
                open_grandchild_thread_id,
            ]
        );

        let closed_descendants = store
            .list_thread_spawn_descendants(root_thread_id, Some(ThreadSpawnEdgeStatus::Closed))
            .await
            .expect("closed descendants should load");
        assert_eq!(
            closed_descendants,
            vec![closed_child_thread_id, closed_great_grandchild_thread_id]
        );
    }
}
