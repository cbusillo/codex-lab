use crate::agent::external_diagnostics::ExternalAgentFailureDetail;
use crate::agent::external_diagnostics::ExternalAgentProviderProvenance;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use rand::prelude::IndexedRandom;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// This structure is used to add some limits on the multi-agent capabilities for Codex. In
/// the current implementation, it limits:
/// * Total number of sub-agents (i.e. threads) per user session
///
/// This structure is shared by all agents in the same user session (because the `AgentControl`
/// is).
#[derive(Default)]
pub(crate) struct AgentRegistry {
    active_agents: Mutex<ActiveAgents>,
    external_agents: Mutex<HashMap<ThreadId, ExternalAgentRuntime>>,
    total_count: AtomicUsize,
}

struct ExternalAgentRuntime {
    parent_thread_id: ThreadId,
    status_tx: watch::Sender<AgentStatus>,
    _status_rx: watch::Receiver<AgentStatus>,
    cancellation_token: CancellationToken,
    provider: ExternalAgentProviderProvenance,
    failure: Option<ExternalAgentFailureDetail>,
    started_at: Instant,
    completed_at: Option<Instant>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExternalAgentRuntimeSnapshot {
    pub(crate) status: AgentStatus,
    pub(crate) provider: ExternalAgentProviderProvenance,
    pub(crate) failure: Option<ExternalAgentFailureDetail>,
    pub(crate) duration_ms: u64,
}

#[derive(Default)]
struct ActiveAgents {
    agent_tree: HashMap<String, AgentMetadata>,
    thread_paths: HashMap<ThreadId, String>,
    used_agent_nicknames: HashSet<String>,
    nickname_reset_count: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AgentMetadata {
    pub(crate) agent_id: Option<ThreadId>,
    pub(crate) agent_path: Option<AgentPath>,
    pub(crate) agent_nickname: Option<String>,
    pub(crate) agent_role: Option<String>,
}

fn format_agent_nickname(name: &str, nickname_reset_count: usize) -> String {
    match nickname_reset_count {
        0 => name.to_string(),
        reset_count => {
            let value = reset_count + 1;
            let suffix = match value % 100 {
                11..=13 => "th",
                _ => match value % 10 {
                    1 => "st", // codespell:ignore
                    2 => "nd", // codespell:ignore
                    3 => "rd", // codespell:ignore
                    _ => "th", // codespell:ignore
                },
            };
            format!("{name} the {value}{suffix}")
        }
    }
}

fn session_depth(session_source: &SessionSource) -> i32 {
    match session_source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { depth, .. }) => *depth,
        SessionSource::SubAgent(_) => 0,
        _ => 0,
    }
}

pub(crate) fn next_thread_spawn_depth(session_source: &SessionSource) -> i32 {
    session_depth(session_source).saturating_add(1)
}

pub(crate) fn exceeds_thread_spawn_depth_limit(depth: i32, max_depth: i32) -> bool {
    depth > max_depth
}

impl AgentRegistry {
    pub(crate) fn reserve_spawn_slot(
        self: &Arc<Self>,
        max_threads: Option<usize>,
    ) -> Result<SpawnReservation> {
        if let Some(max_threads) = max_threads {
            if !self.try_increment_spawned(max_threads) {
                return Err(CodexErr::new(CodexErrorDetails::AgentLimitReached {
                    max_threads,
                }));
            }
        } else {
            self.total_count.fetch_add(1, Ordering::AcqRel);
        }
        Ok(SpawnReservation {
            state: Arc::clone(self),
            active: true,
            reserved_agent_nickname: None,
            reserved_agent_path: None,
        })
    }

    pub(crate) fn release_spawned_thread(&self, thread_id: ThreadId) {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&thread_id);
        let removed_counted_agent = {
            let mut active_agents = self
                .active_agents
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            active_agents
                .thread_paths
                .remove(&thread_id)
                .and_then(|key| active_agents.agent_tree.remove(key.as_str()))
                .is_some_and(|metadata| {
                    !metadata.agent_path.as_ref().is_some_and(AgentPath::is_root)
                })
        };
        if removed_counted_agent {
            self.total_count.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn register_root_thread(&self, thread_id: ThreadId) {
        let mut active_agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root_path = AgentPath::ROOT.to_string();
        let root_thread_id = active_agents
            .agent_tree
            .entry(root_path.clone())
            .or_insert_with(|| AgentMetadata {
                agent_id: Some(thread_id),
                agent_path: Some(AgentPath::root()),
                ..Default::default()
            })
            .agent_id;
        if let Some(root_thread_id) = root_thread_id {
            active_agents.thread_paths.insert(root_thread_id, root_path);
        }
    }

    pub(crate) fn agent_id_for_path(&self, agent_path: &AgentPath) -> Option<ThreadId> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .get(agent_path.as_str())
            .and_then(|metadata| metadata.agent_id)
    }

    pub(crate) fn agent_metadata_for_thread(&self, thread_id: ThreadId) -> Option<AgentMetadata> {
        let active_agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        active_agents
            .thread_paths
            .get(&thread_id)
            .and_then(|path| active_agents.agent_tree.get(path))
            .cloned()
    }

    pub(crate) fn live_agents(&self) -> Vec<AgentMetadata> {
        self.active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .agent_tree
            .values()
            .filter(|metadata| {
                metadata.agent_id.is_some()
                    && !metadata.agent_path.as_ref().is_some_and(AgentPath::is_root)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn register_external_agent(
        &self,
        thread_id: ThreadId,
        parent_thread_id: ThreadId,
        initial_status: AgentStatus,
        provider: ExternalAgentProviderProvenance,
    ) -> CancellationToken {
        let (status_tx, status_rx) = watch::channel(initial_status);
        let cancellation_token = CancellationToken::new();
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                thread_id,
                ExternalAgentRuntime {
                    parent_thread_id,
                    status_tx,
                    _status_rx: status_rx,
                    cancellation_token: cancellation_token.clone(),
                    provider,
                    failure: None,
                    started_at: Instant::now(),
                    completed_at: None,
                },
            );
        cancellation_token
    }

    pub(crate) fn external_agent_snapshot(
        &self,
        thread_id: ThreadId,
    ) -> Option<ExternalAgentRuntimeSnapshot> {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread_id)
            .map(|runtime| {
                let completed_at = runtime.completed_at.unwrap_or_else(Instant::now);
                let duration_ms = completed_at
                    .saturating_duration_since(runtime.started_at)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX);
                ExternalAgentRuntimeSnapshot {
                    status: runtime.status_tx.borrow().clone(),
                    provider: runtime.provider.clone(),
                    failure: runtime.failure.clone(),
                    duration_ms,
                }
            })
    }

    pub(crate) fn external_agent_status(&self, thread_id: ThreadId) -> Option<AgentStatus> {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread_id)
            .map(|runtime| runtime.status_tx.borrow().clone())
    }

    pub(crate) fn has_registered_external_agents(&self) -> bool {
        !self
            .external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    pub(crate) fn live_external_thread_spawn_edges(&self) -> Vec<(ThreadId, ThreadId)> {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|(thread_id, runtime)| {
                let status = runtime.status_tx.borrow().clone();
                (!crate::agent::status::is_final(&status))
                    .then_some((runtime.parent_thread_id, *thread_id))
            })
            .collect()
    }

    pub(crate) fn registered_external_thread_spawn_edges(&self) -> Vec<(ThreadId, ThreadId)> {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(thread_id, runtime)| (runtime.parent_thread_id, *thread_id))
            .collect()
    }

    pub(crate) fn external_agent_status_rx(
        &self,
        thread_id: ThreadId,
    ) -> Option<watch::Receiver<AgentStatus>> {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread_id)
            .map(|runtime| runtime.status_tx.subscribe())
    }

    pub(crate) fn update_external_agent_status(
        &self,
        thread_id: ThreadId,
        status: AgentStatus,
    ) -> bool {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&thread_id)
            .is_some_and(|runtime| {
                if crate::agent::status::is_final(&status) {
                    runtime.completed_at.get_or_insert_with(Instant::now);
                }
                runtime.status_tx.send(status).is_ok()
            })
    }

    pub(crate) fn update_external_agent_failure(
        &self,
        thread_id: ThreadId,
        status: AgentStatus,
        failure: ExternalAgentFailureDetail,
    ) -> bool {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&thread_id)
            .is_some_and(|runtime| {
                runtime.failure = Some(failure);
                runtime.completed_at.get_or_insert_with(Instant::now);
                runtime.status_tx.send(status).is_ok()
            })
    }

    pub(crate) fn cancel_external_agent(&self, thread_id: ThreadId) -> bool {
        self.external_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread_id)
            .is_some_and(|runtime| {
                if crate::agent::status::is_final(&runtime.status_tx.borrow()) {
                    return false;
                }
                runtime.cancellation_token.cancel();
                true
            })
    }

    fn register_spawned_thread(&self, agent_metadata: AgentMetadata) {
        let Some(thread_id) = agent_metadata.agent_id else {
            return;
        };
        let mut active_agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = agent_metadata
            .agent_path
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("thread:{thread_id}"));
        if let Some(agent_nickname) = agent_metadata.agent_nickname.clone() {
            active_agents.used_agent_nicknames.insert(agent_nickname);
        }
        if let Some(previous_key) = active_agents.thread_paths.insert(thread_id, key.clone())
            && previous_key != key
        {
            active_agents.agent_tree.remove(previous_key.as_str());
        }
        if let Some(previous_metadata) = active_agents.agent_tree.insert(key, agent_metadata)
            && let Some(previous_thread_id) = previous_metadata.agent_id
            && previous_thread_id != thread_id
        {
            active_agents.thread_paths.remove(&previous_thread_id);
        }
    }

    fn reserve_agent_nickname(&self, names: &[&str], preferred: Option<&str>) -> Option<String> {
        let mut active_agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let agent_nickname = if let Some(preferred) = preferred {
            preferred.to_string()
        } else {
            if names.is_empty() {
                return None;
            }
            let available_names: Vec<String> = names
                .iter()
                .map(|name| format_agent_nickname(name, active_agents.nickname_reset_count))
                .filter(|name| !active_agents.used_agent_nicknames.contains(name))
                .collect();
            if let Some(name) = available_names.choose(&mut rand::rng()) {
                name.clone()
            } else {
                active_agents.used_agent_nicknames.clear();
                active_agents.nickname_reset_count += 1;
                if let Some(metrics) = codex_otel::global() {
                    let _ = metrics.counter(
                        "codex.multi_agent.nickname_pool_reset",
                        /*inc*/ 1,
                        &[],
                    );
                }
                format_agent_nickname(
                    names.choose(&mut rand::rng())?,
                    active_agents.nickname_reset_count,
                )
            }
        };
        active_agents
            .used_agent_nicknames
            .insert(agent_nickname.clone());
        Some(agent_nickname)
    }

    fn reserve_agent_path(&self, agent_path: &AgentPath) -> Result<()> {
        let mut active_agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match active_agents.agent_tree.entry(agent_path.to_string()) {
            Entry::Occupied(_) => Err(CodexErr::UnsupportedOperation(format!(
                "agent path `{agent_path}` already exists"
            ))),
            Entry::Vacant(entry) => {
                entry.insert(AgentMetadata {
                    agent_path: Some(agent_path.clone()),
                    ..Default::default()
                });
                Ok(())
            }
        }
    }

    fn release_reserved_agent_path(&self, agent_path: &AgentPath) {
        let mut active_agents = self
            .active_agents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active_agents
            .agent_tree
            .get(agent_path.as_str())
            .is_some_and(|metadata| metadata.agent_id.is_none())
        {
            active_agents.agent_tree.remove(agent_path.as_str());
        }
    }

    fn try_increment_spawned(&self, max_threads: usize) -> bool {
        let mut current = self.total_count.load(Ordering::Acquire);
        loop {
            if current >= max_threads {
                return false;
            }
            match self.total_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(updated) => current = updated,
            }
        }
    }
}

pub(crate) struct SpawnReservation {
    state: Arc<AgentRegistry>,
    active: bool,
    reserved_agent_nickname: Option<String>,
    reserved_agent_path: Option<AgentPath>,
}

impl SpawnReservation {
    pub(crate) fn reserve_agent_nickname_with_preference(
        &mut self,
        names: &[&str],
        preferred: Option<&str>,
    ) -> Result<String> {
        let agent_nickname = self
            .state
            .reserve_agent_nickname(names, preferred)
            .ok_or_else(|| {
                CodexErr::UnsupportedOperation("no available agent nicknames".to_string())
            })?;
        self.reserved_agent_nickname = Some(agent_nickname.clone());
        Ok(agent_nickname)
    }

    pub(crate) fn reserve_agent_path(&mut self, agent_path: &AgentPath) -> Result<()> {
        self.state.reserve_agent_path(agent_path)?;
        self.reserved_agent_path = Some(agent_path.clone());
        Ok(())
    }

    pub(crate) fn commit(mut self, agent_metadata: AgentMetadata) {
        self.reserved_agent_nickname = None;
        self.reserved_agent_path = None;
        self.state.register_spawned_thread(agent_metadata);
        self.active = false;
    }
}

impl Drop for SpawnReservation {
    fn drop(&mut self) {
        if self.active {
            if let Some(agent_path) = self.reserved_agent_path.take() {
                self.state.release_reserved_agent_path(&agent_path);
            }
            self.state.total_count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
