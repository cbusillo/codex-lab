use super::spawn::SpawnInitialInput;
use super::*;
use crate::agent::external_command::ExternalAgentLaunch;
use crate::agent::external_diagnostics::ExternalAgentFailureDetail;
use crate::agent::external_diagnostics::ExternalAgentProviderProvenance;
use crate::agent::external_diagnostics::ExternalAgentQuotaDiagnostic;
use crate::agent::external_diagnostics::permission_profile_is_read_only;
use crate::agent::external_diagnostics::redact_external_agent_status;
use crate::agent::provider_routing::ProviderRoutingKind;
use crate::agent::provider_routing::ProviderRoutingSummary;
use crate::agent::registry::SpawnReservation;
use crate::config::ExternalCommandAgentBackendConfig;
use codex_agent_graph_store::ExternalAgentRunOutcome;
use codex_agent_graph_store::ExternalAgentRunStart;
use std::collections::HashSet;
use std::path::Path;

pub(super) struct ExternalAgentSpawn {
    pub(super) config: Config,
    pub(super) initial_input: SpawnInitialInput,
    pub(super) notification_source: Option<SessionSource>,
    pub(super) options: SpawnAgentOptions,
    pub(super) reservation: SpawnReservation,
    pub(super) agent_metadata: AgentMetadata,
    pub(super) backend: ExternalCommandAgentBackendConfig,
}

impl ListedAgent {
    /// Apply the completion-payload budget to every agent-authored string this entry can carry
    /// before it reaches a model-visible tool output.
    pub(crate) fn bounded_for_model(mut self) -> Self {
        self.agent_status = crate::session_prefix::bounded_status(&self.agent_status);
        if let Some(failure) = self.failure.as_mut()
            && let Some(message) = failure.message.as_mut()
        {
            *message = crate::session_prefix::bounded_completion_payload(message);
        }
        self
    }

    pub(crate) fn redact_external_metadata(mut self) -> Self {
        if self.provider.is_none() {
            return self;
        }
        self.agent_status = redact_external_agent_status(self.agent_status, self.failure.as_ref());
        self.provider = None;
        self.failure = self
            .failure
            .as_ref()
            .map(ExternalAgentFailureDetail::redacted);
        self.quota_diagnostic = None;
        self
    }
}

impl AgentControl {
    pub(super) async fn spawn_external_agent(
        &self,
        spawn: ExternalAgentSpawn,
    ) -> CodexResult<LiveAgent> {
        let ExternalAgentSpawn {
            config,
            initial_input,
            notification_source,
            options,
            reservation,
            mut agent_metadata,
            backend,
        } = spawn;
        if options.fork_mode.is_some() {
            return Err(CodexErr::UnsupportedOperation(
                "external_command agents do not support fork_turns".to_string(),
            ));
        }
        let Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            agent_path,
            agent_role,
            ..
        })) = notification_source
        else {
            return Err(CodexErr::UnsupportedOperation(
                "external_command agents require a thread-spawn source".to_string(),
            ));
        };
        let Some(recipient) = agent_path.clone() else {
            return Err(CodexErr::UnsupportedOperation(
                "external_command agents require an agent path".to_string(),
            ));
        };
        let author = recipient
            .as_str()
            .rsplit_once('/')
            .and_then(|(parent, _)| AgentPath::try_from(parent).ok())
            .unwrap_or_else(AgentPath::root);
        let initial_operation = match initial_input {
            SpawnInitialInput::UserInput(input) => input.into(),
            SpawnInitialInput::InterAgentCommunication(communication, _) => {
                if communication
                    .encrypted_content
                    .as_ref()
                    .is_some_and(|content| !content.is_empty())
                {
                    return Err(CodexErr::UnsupportedOperation(
                        "external_command agents require plaintext task content".to_string(),
                    ));
                }
                Op::InterAgentCommunication { communication }
            }
        };
        let thread_id = ThreadId::new();
        agent_metadata.agent_id = Some(thread_id);
        let is_read_only =
            permission_profile_is_read_only(&config.permissions.effective_permission_profile());
        let external_agent_provider = options.external_agent_provider.clone();
        let external_agent_routing = options.external_agent_routing.clone();
        let resolved_command = external_agent_provider
            .as_ref()
            .and_then(ExternalAgentProviderProvenance::resolved_command)
            .map(Path::to_path_buf);
        let preflight_completed = resolved_command.is_some();
        let provider = external_agent_provider.unwrap_or_else(|| {
            ExternalAgentProviderProvenance::new(
                agent_role.as_deref(),
                &backend,
                config.cwd.as_path(),
                is_read_only,
                /*cli_version*/ None,
            )
        });
        let claude_stream_json_enabled = provider.supports_claude_stream_json();
        let routing = external_agent_routing.unwrap_or_else(|| ProviderRoutingSummary {
            kind: ProviderRoutingKind::Explicit,
            requested: agent_role.clone(),
            effective: agent_role.clone().unwrap_or_else(|| "external".to_string()),
            reason: "External agent was selected explicitly.".to_string(),
            skipped_candidates: Vec::new(),
        });
        let cancellation_token = self.state.register_external_agent(
            thread_id,
            parent_thread_id,
            AgentStatus::PendingInit,
            provider.clone(),
        );
        reservation.commit(agent_metadata.clone());
        self.persist_thread_spawn_edge(parent_thread_id, thread_id)
            .await;
        self.persist_external_agent_run_started(
            parent_thread_id,
            thread_id,
            agent_metadata.agent_path.as_ref().map(ToString::to_string),
            &routing,
            &provider,
        )
        .await;
        let launch = ExternalAgentLaunch {
            thread_id,
            parent_thread_id,
            author,
            recipient,
            role: agent_role,
            task_name: agent_metadata
                .agent_path
                .as_ref()
                .map(|path| path.name().to_string()),
            initial_operation,
            backend,
            cwd: config.cwd.to_path_buf(),
            cancellation_token,
            is_read_only,
            preflight_completed,
            resolved_command,
            claude_stream_json_enabled,
            hide_provider_metadata: config.multi_agent_v2.hide_spawn_agent_metadata,
        };
        self.spawn_external_agent_task(launch);
        Ok(LiveAgent {
            thread_id,
            metadata: agent_metadata,
            status: AgentStatus::PendingInit,
        })
    }

    pub(crate) fn is_external_agent(&self, agent_id: ThreadId) -> bool {
        self.state.external_agent_status(agent_id).is_some()
    }

    pub(crate) fn redact_external_status(
        &self,
        agent_id: ThreadId,
        status: AgentStatus,
    ) -> AgentStatus {
        let Some(snapshot) = self.state.external_agent_snapshot(agent_id) else {
            return status;
        };
        redact_external_agent_status(status, snapshot.failure.as_ref())
    }

    fn spawn_external_agent_task(&self, launch: ExternalAgentLaunch) {
        let control = self.clone();
        tokio::spawn(async move {
            crate::agent::external_command::run_external_agent(launch, control).await;
        });
    }

    pub(crate) fn update_external_agent_status(&self, agent_id: ThreadId, status: AgentStatus) {
        let _ = self.state.update_external_agent_status(agent_id, status);
    }

    pub(crate) fn update_external_agent_status_with_quota(
        &self,
        agent_id: ThreadId,
        status: AgentStatus,
        quota_diagnostic: Option<ExternalAgentQuotaDiagnostic>,
    ) {
        let _ =
            self.state
                .update_external_agent_status_with_quota(agent_id, status, quota_diagnostic);
    }

    pub(crate) fn update_external_agent_failure(
        &self,
        agent_id: ThreadId,
        status: AgentStatus,
        failure: ExternalAgentFailureDetail,
    ) {
        let _ = self
            .state
            .update_external_agent_failure(agent_id, status, failure);
    }

    pub(crate) fn release_external_agent(&self, agent_id: ThreadId) {
        self.state.release_spawned_thread(agent_id);
    }

    pub(crate) async fn close_external_agent(&self, agent_id: ThreadId) {
        self.close_thread_spawn_edge(agent_id).await;
        self.release_external_agent(agent_id);
    }

    pub(crate) async fn persist_external_agent_run_finished(
        &self,
        agent_id: ThreadId,
        terminal_state: &str,
    ) {
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return;
        };
        let (duration_ms, failure) = match self.state.external_agent_snapshot(agent_id) {
            Some(snapshot) => (snapshot.duration_ms, snapshot.failure),
            None => {
                warn!(
                    "external-agent runtime snapshot missing while persisting outcome for {agent_id}"
                );
                (0, None)
            }
        };
        let outcome = ExternalAgentRunOutcome {
            completed_at_ms: chrono::Utc::now().timestamp_millis(),
            duration_ms,
            terminal_state: terminal_state.to_string(),
            failure_kind: failure
                .as_ref()
                .map(|failure| failure.kind.as_str().to_string()),
            failure_message: failure.and_then(|failure| failure.message),
        };
        if let Err(err) = agent_graph_store
            .finish_external_agent_run(agent_id, outcome)
            .await
        {
            warn!("failed to persist external-agent run outcome for {agent_id}: {err}");
        }
    }

    pub(crate) async fn close_thread_spawn_edge(&self, agent_id: ThreadId) {
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return;
        };
        if let Err(err) = agent_graph_store
            .set_thread_spawn_edge_status(
                agent_id,
                codex_agent_graph_store::ThreadSpawnEdgeStatus::Closed,
            )
            .await
        {
            warn!("failed to persist thread-spawn edge status for {agent_id}: {err}");
        }
    }

    pub(super) async fn registered_external_thread_spawn_descendants(
        &self,
        root_thread_id: ThreadId,
    ) -> CodexResult<Vec<ThreadId>> {
        let mut children_by_parent = self.live_thread_spawn_children().await?;
        let mut external_agent_ids = HashSet::new();
        for (parent_thread_id, child_thread_id) in
            self.state.registered_external_thread_spawn_edges()
        {
            let children = children_by_parent.entry(parent_thread_id).or_default();
            if children
                .iter()
                .all(|(existing_child_thread_id, _)| *existing_child_thread_id != child_thread_id)
            {
                children.push((child_thread_id, AgentMetadata::default()));
            }
            external_agent_ids.insert(child_thread_id);
        }

        let mut descendants = Vec::new();
        let mut stack = children_by_parent
            .remove(&root_thread_id)
            .unwrap_or_default()
            .into_iter()
            .map(|(child_thread_id, _)| child_thread_id)
            .rev()
            .collect::<Vec<_>>();

        while let Some(thread_id) = stack.pop() {
            if external_agent_ids.contains(&thread_id) {
                descendants.push(thread_id);
            }
            if let Some(children) = children_by_parent.remove(&thread_id) {
                for (child_thread_id, _) in children.into_iter().rev() {
                    stack.push(child_thread_id);
                }
            }
        }

        Ok(descendants)
    }

    pub(super) async fn persist_thread_spawn_edge(
        &self,
        parent_thread_id: ThreadId,
        child_thread_id: ThreadId,
    ) {
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return;
        };
        if let Err(err) = agent_graph_store
            .upsert_thread_spawn_edge(
                parent_thread_id,
                child_thread_id,
                codex_agent_graph_store::ThreadSpawnEdgeStatus::Open,
            )
            .await
        {
            warn!("failed to persist thread-spawn edge: {err}");
        }
    }

    pub(super) async fn persist_external_agent_run_started(
        &self,
        parent_thread_id: ThreadId,
        child_thread_id: ThreadId,
        agent_path: Option<String>,
        routing: &ProviderRoutingSummary,
        provider: &ExternalAgentProviderProvenance,
    ) {
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(agent_graph_store) = state.agent_graph_store() else {
            return;
        };
        let skipped_candidates_json =
            serde_json::to_string(&routing.skipped_candidates).unwrap_or_else(|_| "[]".to_string());
        let run = ExternalAgentRunStart {
            child_thread_id,
            parent_thread_id,
            agent_path,
            routing_kind: routing.kind.as_str().to_string(),
            requested_selector: routing.requested.clone(),
            effective_selector: routing.effective.clone(),
            routing_reason: routing.reason.clone(),
            skipped_candidates_json,
            provider_family: provider.provider_family.clone(),
            command: provider.command.clone(),
            cli_version: provider.cli_version.clone(),
            capability_source: provider.capability_source.as_str().to_string(),
            capability_freshness: provider
                .capability_freshness
                .map(|freshness| freshness.as_str().to_string()),
            protocol: provider.protocol.as_str().to_string(),
            mode: provider.mode.as_str().to_string(),
            workspace: provider.workspace.clone(),
            model: provider.model.clone(),
            effort: provider.effort.clone(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
        };
        if let Err(err) = agent_graph_store.insert_external_agent_run(run).await {
            warn!("failed to persist external-agent run start for {child_thread_id}: {err}");
        }
    }
}
