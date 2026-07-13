use super::*;

impl AgentControl {
    /// Restore persisted V2 agent identities without reopening their runtimes.
    pub(crate) async fn restore_v2_agent_metadata(
        &self,
        config: &Config,
        root_thread_id: ThreadId,
    ) {
        self.state.register_root_thread(root_thread_id);

        let Ok(state) = self.upgrade() else {
            return;
        };
        let Some(state_db) = state.state_db() else {
            return;
        };
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
                let mut metadata = self.prepare_agent_metadata(
                    &mut reservation,
                    config,
                    stored_agent_path.or_else(|| stored_thread.source.get_agent_path()),
                    stored_thread
                        .agent_role
                        .or_else(|| stored_thread.source.get_agent_role()),
                    stored_thread
                        .agent_nickname
                        .or_else(|| stored_thread.source.get_nickname()),
                )?;
                metadata.agent_id = Some(thread_id);
                reservation.commit(metadata);
                Ok::<(), CodexErr>(())
            })();
            if let Err(err) = restore_result {
                warn!("failed to restore V2 agent metadata for {thread_id}: {err}");
            }
        }
    }
}
