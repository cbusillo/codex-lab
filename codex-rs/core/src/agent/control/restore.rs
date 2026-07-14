use super::*;
use crate::path_utils;
use std::path::Path;

impl AgentControl {
    /// Restore persisted V2 agent identities without reopening their runtimes.
    pub(crate) async fn restore_v2_agent_metadata(
        &self,
        config: &Config,
        root_thread_id: ThreadId,
        root_rollout_path: Option<&Path>,
    ) {
        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(state_db) = state.state_db() else {
            return;
        };
        let Some(root_rollout_path) = root_rollout_path else {
            return;
        };
        let root_metadata = match state_db.get_thread(root_thread_id).await {
            Ok(Some(root_metadata)) => root_metadata,
            Ok(None) => return,
            Err(err) => {
                warn!("failed to validate persisted V2 root {root_thread_id}: {err}");
                return;
            }
        };
        if !path_utils::paths_match_after_normalization(
            &root_metadata.rollout_path,
            root_rollout_path,
        ) {
            return;
        }
        self.state.register_root_thread(root_thread_id);

        let descendant_ids = match state_db
            .list_thread_spawn_descendants_with_status(
                root_thread_id,
                DirectionalThreadSpawnEdgeStatus::Open,
            )
            .await
        {
            Ok(descendant_ids) => descendant_ids,
            Err(err) => {
                warn!("failed to restore persisted V2 agent metadata for {root_thread_id}: {err}");
                return;
            }
        };

        for thread_id in descendant_ids {
            if self.state.agent_metadata_for_thread(thread_id).is_some() {
                continue;
            }
            let stored_thread = match state
                .read_stored_thread(ReadThreadParams {
                    thread_id,
                    include_archived: true,
                    include_history: false,
                })
                .await
            {
                Ok(stored_thread) => stored_thread,
                Err(CodexErr::ThreadNotFound(_)) => {
                    match state_db.get_thread(thread_id).await {
                        Ok(None) => continue,
                        Ok(Some(_)) => {
                            warn!(
                                "failed to restore V2 agent metadata for {thread_id}: stored thread is missing"
                            );
                        }
                        Err(err) => {
                            warn!(
                                "failed to inspect missing stored thread {thread_id} while restoring V2 agent metadata: {err}"
                            );
                        }
                    }
                    continue;
                }
                Err(err) => {
                    warn!("failed to restore V2 agent metadata for {thread_id}: {err}");
                    continue;
                }
            };
            let Some(rollout_path) = stored_thread.rollout_path.as_ref() else {
                warn!("failed to restore V2 agent metadata for {thread_id}: rollout is missing");
                continue;
            };
            match tokio::fs::try_exists(rollout_path).await {
                Ok(true) => {}
                Ok(false) => {
                    warn!(
                        "failed to restore V2 agent metadata for {thread_id}: rollout does not exist"
                    );
                    continue;
                }
                Err(err) => {
                    warn!("failed to validate V2 agent rollout for {thread_id}: {err}");
                    continue;
                }
            }
            let restore_result = (|| {
                let stored_agent_path = stored_thread
                    .agent_path
                    .as_deref()
                    .map(AgentPath::try_from)
                    .transpose()
                    .map_err(|err| {
                        CodexErr::InvalidRequest(format!("invalid stored agent path: {err}"))
                    })?;
                let mut reservation = self.state.reserve_spawn_slot(/*max_threads*/ None)?;
                let metadata = AgentMetadata {
                    agent_id: Some(thread_id),
                    ..self.prepare_agent_metadata(
                        &mut reservation,
                        config,
                        stored_agent_path.or_else(|| stored_thread.source.get_agent_path()),
                        stored_thread
                            .agent_role
                            .or_else(|| stored_thread.source.get_agent_role()),
                        stored_thread
                            .agent_nickname
                            .or_else(|| stored_thread.source.get_nickname()),
                    )?
                };
                reservation.commit(metadata);
                Ok::<(), CodexErr>(())
            })();
            if let Err(err) = restore_result {
                warn!("failed to restore V2 agent metadata for {thread_id}: {err}");
            }
        }
    }
}
