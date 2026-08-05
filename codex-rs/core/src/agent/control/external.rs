use super::spawn::SpawnInitialInput;
use super::*;
use crate::agent::external_command::ExternalAgentLaunch;
use crate::agent::external_diagnostics::ExternalAgentFailureDetail;
use crate::agent::external_diagnostics::ExternalAgentProviderProvenance;
use crate::agent::external_diagnostics::permission_profile_is_read_only;
use crate::agent::external_diagnostics::redact_external_agent_status;
use crate::agent::registry::SpawnReservation;
use crate::config::ExternalCommandAgentBackendConfig;
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
        let cancellation_token = self.state.register_external_agent(
            thread_id,
            parent_thread_id,
            AgentStatus::PendingInit,
            provider,
        );
        reservation.commit(agent_metadata.clone());
        self.persist_thread_spawn_edge(parent_thread_id, thread_id)
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
}
