//! Session, resume, fork, and subagent selection lifecycle for the TUI app.
//!
//! This module owns the high-level transitions between app-server threads: starting fresh sessions,
//! resuming/forking saved sessions, replacing ChatWidget instances, and maintaining the agent picker
//! cache used for multi-agent navigation.

use super::*;
use crate::app::PendingDirectLoginAddAccount;
use crate::app_event::AuthAccountSelection;
use crate::app_event::AuthProfileSelection;
use crate::bottom_pane::LoginAddAccountState;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::CancelLoginAccountParams;
use codex_app_server_protocol::CancelLoginAccountResponse;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::LoginAccountParams;
use codex_app_server_protocol::LoginAccountResponse;
use codex_cloud_config::cloud_config_bundle_loader_for_storage;
use codex_config::CloudConfigBundleLoader;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::CLIENT_ID;
use codex_login::ServerOptions;

struct AuthSwitchRollback {
    codex_home: PathBuf,
    previous_auth: Option<codex_login::AuthDotJson>,
    previous_active_account_id: Option<String>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    armed: bool,
}

impl AuthSwitchRollback {
    fn new(
        codex_home: PathBuf,
        previous_auth: Option<codex_login::AuthDotJson>,
        previous_active_account_id: Option<String>,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        Self {
            codex_home,
            previous_auth,
            previous_active_account_id,
            auth_credentials_store_mode,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }

    fn restore_now(&mut self) -> std::io::Result<()> {
        self.restore_auth()?;
        self.armed = false;
        Ok(())
    }

    fn restore_auth(&self) -> std::io::Result<()> {
        if let Some(previous_auth) = &self.previous_auth {
            codex_login::save_auth(
                &self.codex_home,
                previous_auth,
                self.auth_credentials_store_mode,
            )?;
        } else {
            codex_login::delete_auth(&self.codex_home, self.auth_credentials_store_mode)
                .map(|_| ())?;
        }
        codex_login::set_active_account_id(
            &self.codex_home,
            self.previous_active_account_id.clone(),
        )?;
        Ok(())
    }
}

impl Drop for AuthSwitchRollback {
    fn drop(&mut self) {
        if self.armed
            && let Err(err) = self.restore_auth()
        {
            tracing::warn!("failed to roll back stored account auth switch: {err}");
        }
    }
}

impl App {
    async fn config_for_auth_profile_switch(
        &self,
        auth_home: AbsolutePathBuf,
        cloud_config_bundle: CloudConfigBundleLoader,
    ) -> std::io::Result<Config> {
        ConfigBuilder::default()
            .cli_overrides(self.cli_kv_overrides.clone())
            .harness_overrides(ConfigOverrides {
                cwd: Some(self.config.cwd.to_path_buf()),
                approval_policy: Some(*self.config.permissions.approval_policy.get()),
                codex_self_exe: self.config.codex_self_exe.clone(),
                codex_linux_sandbox_exe: self.config.codex_linux_sandbox_exe.clone(),
                main_execve_wrapper_exe: self.config.main_execve_wrapper_exe.clone(),
                show_raw_agent_reasoning: Some(self.config.show_raw_agent_reasoning),
                workspace_roots: Some(self.config.workspace_roots.clone()),
                ..Default::default()
            })
            .loader_overrides(self.loader_overrides.clone())
            .strict_config(self.strict_config)
            .cloud_config_bundle(cloud_config_bundle)
            .auth_home(auth_home.to_path_buf())
            .build()
            .await
            .map_err(std::io::Error::other)
    }

    pub(super) async fn open_agent_picker(&mut self, app_server: &mut AppServerSession) {
        let mut thread_ids = self.agent_navigation.tracked_thread_ids();
        for thread_id in self.thread_event_channels.keys().copied() {
            if !thread_ids.contains(&thread_id) {
                thread_ids.push(thread_id);
            }
        }
        for thread_id in thread_ids {
            if self.side_threads.contains_key(&thread_id) {
                continue;
            }
            if !self
                .refresh_agent_picker_thread_liveness(app_server, thread_id)
                .await
            {
                continue;
            }
        }

        let has_non_primary_agent_thread = self
            .agent_navigation
            .has_non_primary_thread(self.primary_thread_id);
        if !self.config.features.enabled(Feature::Collab) && !has_non_primary_agent_thread {
            self.chat_widget.open_multi_agent_enable_prompt();
            return;
        }

        if self.agent_navigation.is_empty() {
            self.chat_widget
                .add_info_message("No agents available yet.".to_string(), /*hint*/ None);
            return;
        }

        let mut initial_selected_idx = None;
        let items: Vec<SelectionItem> = self
            .agent_navigation
            .ordered_threads()
            .iter()
            .enumerate()
            .map(|(idx, (thread_id, entry))| {
                if self.active_thread_id == Some(*thread_id) {
                    initial_selected_idx = Some(idx);
                }
                let id = *thread_id;
                let is_primary = self.primary_thread_id == Some(*thread_id);
                let name = format_agent_picker_item_name(
                    entry.agent_nickname.as_deref(),
                    entry.agent_role.as_deref(),
                    is_primary,
                );
                let uuid = thread_id.to_string();
                SelectionItem {
                    name: name.clone(),
                    name_prefix_spans: agent_picker_status_dot_spans(entry.is_closed),
                    description: Some(uuid.clone()),
                    is_current: self.active_thread_id == Some(*thread_id),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SelectAgentThread(id));
                    })],
                    dismiss_on_select: true,
                    search_value: Some(format!("{name} {uuid}")),
                    ..Default::default()
                }
            })
            .collect();

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Subagents".to_string()),
            subtitle: Some(AgentNavigationState::picker_subtitle()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    pub(super) fn is_terminal_thread_read_error(err: &color_eyre::Report) -> bool {
        err.chain()
            .any(|cause| cause.to_string().contains("thread not loaded:"))
    }

    pub(super) fn closed_state_for_thread_read_error(
        err: &color_eyre::Report,
        existing_is_closed: Option<bool>,
    ) -> bool {
        Self::is_terminal_thread_read_error(err) || existing_is_closed.unwrap_or(false)
    }

    pub(super) fn can_fallback_from_include_turns_error(err: &color_eyre::Report) -> bool {
        err.chain().any(|cause| {
            let message = cause.to_string();
            message.contains("includeTurns is unavailable before first user message")
                || message.contains("ephemeral threads do not support includeTurns")
        })
    }

    /// Updates cached picker metadata and then mirrors any visible-label change into the footer.
    ///
    /// These two writes stay paired so the picker rows and contextual footer continue to describe
    /// the same displayed thread after nickname or role updates.
    pub(super) fn upsert_agent_picker_thread(
        &mut self,
        thread_id: ThreadId,
        agent_nickname: Option<String>,
        agent_role: Option<String>,
        is_closed: bool,
    ) {
        self.chat_widget.set_collab_agent_metadata(
            thread_id,
            agent_nickname.clone(),
            agent_role.clone(),
        );
        self.agent_navigation
            .upsert(thread_id, agent_nickname, agent_role, is_closed);
        self.sync_active_agent_label();
    }

    /// Marks a cached picker thread closed and recomputes the contextual footer label.
    ///
    /// Closing a thread is not the same as removing it: users can still inspect finished agent
    /// transcripts, and the stable next/previous traversal order should not collapse around them.
    pub(super) fn mark_agent_picker_thread_closed(&mut self, thread_id: ThreadId) {
        self.agent_navigation.mark_closed(thread_id);
        self.sync_active_agent_label();
    }

    pub(super) async fn refresh_agent_picker_thread_liveness(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> bool {
        let existing_entry = self.agent_navigation.get(&thread_id).cloned();
        let has_replay_channel = self.thread_event_channels.contains_key(&thread_id);
        match app_server
            .thread_read(thread_id, /*include_turns*/ false)
            .await
        {
            Ok(thread) => {
                self.upsert_agent_picker_thread(
                    thread_id,
                    thread.agent_nickname.or_else(|| {
                        existing_entry
                            .as_ref()
                            .and_then(|entry| entry.agent_nickname.clone())
                    }),
                    thread.agent_role.or_else(|| {
                        existing_entry
                            .as_ref()
                            .and_then(|entry| entry.agent_role.clone())
                    }),
                    matches!(
                        thread.status,
                        codex_app_server_protocol::ThreadStatus::NotLoaded
                    ),
                );
                true
            }
            Err(err) => {
                if Self::is_terminal_thread_read_error(&err) && !has_replay_channel {
                    self.agent_navigation.remove(thread_id);
                    return false;
                }
                let is_closed = Self::closed_state_for_thread_read_error(
                    &err,
                    existing_entry.as_ref().map(|entry| entry.is_closed),
                );
                if let Some(entry) = existing_entry {
                    self.upsert_agent_picker_thread(
                        thread_id,
                        entry.agent_nickname,
                        entry.agent_role,
                        is_closed,
                    );
                } else {
                    self.upsert_agent_picker_thread(
                        thread_id, /*agent_nickname*/ None, /*agent_role*/ None,
                        is_closed,
                    );
                }
                true
            }
        }
    }

    /// Materializes a live thread into local replay state when the picker knows about it but the
    /// TUI has not cached a local event channel yet.
    ///
    /// Resume-time backfill intentionally avoids creating empty placeholder channels, because those
    /// placeholders make stale `/agent` entries open blank transcripts. When a user later selects a
    /// still-live discovered thread, attach it on demand with a real resumed snapshot.
    pub(super) async fn attach_live_thread_for_selection(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<bool> {
        if self.thread_event_channels.contains_key(&thread_id) {
            return Ok(true);
        }

        let (session, turns, live_attached) = match app_server
            .resume_thread(self.config.clone(), thread_id)
            .await
        {
            Ok(started) => (started.session, started.turns, true),
            Err(resume_err) => {
                tracing::warn!(
                    thread_id = %thread_id,
                    error = %resume_err,
                    "failed to resume live thread for selection; falling back to thread/read"
                );
                let (thread, turns) = match app_server
                    .thread_read(thread_id, /*include_turns*/ true)
                    .await
                {
                    Ok(thread) => {
                        let turns = thread.turns.clone();
                        (thread, turns)
                    }
                    Err(err) if Self::can_fallback_from_include_turns_error(&err) => {
                        let thread = app_server
                            .thread_read(thread_id, /*include_turns*/ false)
                            .await?;
                        (thread, Vec::new())
                    }
                    Err(err) => return Err(err),
                };
                if turns.is_empty() {
                    // A `thread/read` fallback without turns would create a blank local replay
                    // channel with no live listener attached, which blocks later real re-attach.
                    return Err(color_eyre::eyre::eyre!(
                        "Agent thread {thread_id} is not yet available for replay or live attach."
                    ));
                }
                let mut session = self.session_state_for_thread_read(thread_id, &thread).await;
                // `thread/read` can seed replay state, but it does not attach the app-server
                // listener that `thread/resume` establishes, so treat this path as replay-only.
                session.model.clear();
                (session, turns, false)
            }
        };
        let channel = self.ensure_thread_channel(thread_id);
        let mut store = channel.store.lock().await;
        store.set_session(session, turns);
        Ok(live_attached)
    }

    /// Replaces the chat widget and re-seeds the new widget's collab metadata from the navigation
    /// cache.
    ///
    /// Thread switches reconstruct the `ChatWidget`, which loses the `collab_agent_metadata` map.
    /// This helper copies every known nickname/role from `AgentNavigationState` into the
    /// replacement widget so that replayed collab items render agent names immediately.
    pub(super) fn replace_chat_widget(&mut self, mut chat_widget: ChatWidget) {
        // Transfer the last-written terminal title to the replacement widget
        // so it knows what OSC title is currently displayed. Without this, the
        // new widget would redundantly clear and rewrite the same title, causing
        // a visible flicker in some terminals.
        let previous_terminal_title = self.chat_widget.last_terminal_title.take();
        if chat_widget.last_terminal_title.is_none() {
            chat_widget.last_terminal_title = previous_terminal_title;
        }
        chat_widget.remote_connection = self.chat_widget.remote_connection.clone();
        for (thread_id, entry) in self.agent_navigation.ordered_threads() {
            chat_widget.set_collab_agent_metadata(
                thread_id,
                entry.agent_nickname.clone(),
                entry.agent_role.clone(),
            );
        }
        self.chat_widget = chat_widget;
        self.sync_active_agent_label();
    }

    pub(super) async fn select_agent_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<()> {
        if self.active_thread_id == Some(thread_id) {
            return Ok(());
        }

        if !self
            .refresh_agent_picker_thread_liveness(app_server, thread_id)
            .await
        {
            self.chat_widget
                .add_error_message(format!("Agent thread {thread_id} is no longer available."));
            return Ok(());
        }

        let mut is_replay_only = self
            .agent_navigation
            .get(&thread_id)
            .is_some_and(|entry| entry.is_closed);
        let mut attached_replay_only = false;
        if self.should_attach_live_thread_for_selection(thread_id) {
            match self
                .attach_live_thread_for_selection(app_server, thread_id)
                .await
            {
                Ok(live_attached) => {
                    attached_replay_only = !live_attached;
                    if attached_replay_only {
                        is_replay_only = true;
                    }
                }
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to attach to agent thread {thread_id}: {err}"
                    ));
                    return Ok(());
                }
            }
        } else if !self.thread_event_channels.contains_key(&thread_id) && is_replay_only {
            self.chat_widget
                .add_error_message(format!("Agent thread {thread_id} is no longer available."));
            return Ok(());
        }

        let previous_thread_id = self.active_thread_id;
        self.store_active_thread_receiver().await;
        self.active_thread_id = None;
        let Some((receiver, mut snapshot)) = self.activate_thread_for_replay(thread_id).await
        else {
            self.chat_widget
                .add_error_message(format!("Agent thread {thread_id} is already active."));
            if let Some(previous_thread_id) = previous_thread_id {
                self.activate_thread_channel(previous_thread_id).await;
            }
            return Ok(());
        };

        self.refresh_snapshot_session_if_needed(
            app_server,
            thread_id,
            is_replay_only,
            &mut snapshot,
        )
        .await;

        self.active_thread_id = Some(thread_id);
        self.active_thread_rx = Some(receiver);

        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            /*initial_user_message*/ None,
        );
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));

        self.reset_for_thread_switch(tui)?;
        self.replay_thread_snapshot(snapshot, !is_replay_only);
        if is_replay_only {
            let message = if attached_replay_only {
                format!(
                    "Agent thread {thread_id} could not be resumed live. Replaying saved transcript."
                )
            } else {
                format!("Agent thread {thread_id} is closed. Replaying saved transcript.")
            };
            self.chat_widget.add_info_message(message, /*hint*/ None);
        }
        self.drain_active_thread_events(tui).await?;
        self.refresh_pending_thread_approvals().await;

        Ok(())
    }

    pub(super) fn should_attach_live_thread_for_selection(&self, thread_id: ThreadId) -> bool {
        !self.thread_event_channels.contains_key(&thread_id)
            && self
                .agent_navigation
                .get(&thread_id)
                .is_none_or(|entry| !entry.is_closed)
    }

    pub(super) fn reset_for_thread_switch(&mut self, tui: &mut tui::Tui) -> Result<()> {
        self.reset_transcript_state_after_clear();
        tui.clear_pending_history_lines();
        Self::clear_terminal_for_thread_switch(&mut tui.terminal)?;
        Ok(())
    }

    pub(super) fn clear_terminal_for_thread_switch<B>(
        terminal: &mut crate::custom_terminal::Terminal<B>,
    ) -> Result<()>
    where
        B: Backend + Write,
    {
        terminal.clear_scrollback_and_visible_screen_ansi()?;
        let mut area = terminal.viewport_area;
        if area.y > 0 {
            area.y = 0;
            terminal.set_viewport_area(area);
        }
        Ok(())
    }

    pub(super) fn reset_thread_event_state(&mut self) {
        self.abort_all_thread_event_listeners();
        self.thread_event_channels.clear();
        self.agent_navigation.clear();
        self.side_threads.clear();
        self.active_thread_id = None;
        self.active_thread_rx = None;
        self.primary_thread_id = None;
        self.last_subagent_backfill_attempt = None;
        self.primary_session_configured = None;
        self.pending_primary_events.clear();
        self.pending_app_server_requests.clear();
        self.pending_startup_thread_start = false;
        self.chat_widget.set_pending_thread_approvals(Vec::new());
        self.sync_active_agent_label();
    }

    pub(super) async fn handle_startup_thread_started(
        &mut self,
        app_server: &mut AppServerSession,
        result: Result<AppServerStartedThread, String>,
    ) -> Result<()> {
        if !self.pending_startup_thread_start {
            if let Ok(started) = result {
                let thread_id = started.session.thread_id;
                if let Err(err) = app_server.thread_unsubscribe(thread_id).await {
                    tracing::warn!(
                        thread_id = %thread_id,
                        "failed to unsubscribe stale startup thread: {err}"
                    );
                }
                self.discard_thread_local_state(thread_id).await;
            }
            return Ok(());
        }

        self.pending_startup_thread_start = false;
        self.chat_widget
            .set_queue_submissions_until_session_configured(/*queue*/ false);
        match result {
            Ok(started) => {
                let replay_result = self
                    .enqueue_primary_thread_session(started.session, started.turns)
                    .await;
                self.chat_widget.maybe_send_next_queued_input();
                replay_result?;
            }
            Err(err) => {
                return Err(color_eyre::eyre::eyre!(
                    "Failed to start a fresh session through the app server: {err}"
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn start_fresh_session_with_summary_hint(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        session_start_source: Option<ThreadStartSource>,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
    ) {
        // Start a fresh in-memory session while preserving resumability via persisted rollout
        // history. If an initial message is provided, `enqueue_primary_thread_session` suppresses it
        // until the new session is configured and any replayed turns have been rendered.
        self.refresh_in_memory_config_from_disk_best_effort("starting a new thread")
            .await;
        let config = self.fresh_session_config();
        self.start_fresh_session_with_config(
            tui,
            app_server,
            config,
            session_start_source,
            initial_user_message,
            /*cleanup_current_thread*/ true,
            "To continue this session, run ",
            "Failed to attach to fresh app-server thread",
            "Failed to start a fresh session through the app server",
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_fresh_session_with_config(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        config: Config,
        session_start_source: Option<ThreadStartSource>,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
        cleanup_current_thread: bool,
        resume_hint_prefix: &'static str,
        attach_error_prefix: &'static str,
        start_error_prefix: &'static str,
    ) -> bool {
        let model = self.chat_widget.current_model().to_string();
        let summary = session_summary(
            self.chat_widget.token_usage(),
            self.chat_widget.thread_id(),
            self.chat_widget.thread_name(),
            self.chat_widget.rollout_path().as_deref(),
        );
        if cleanup_current_thread {
            self.shutdown_current_thread(app_server).await;
            let tracked_thread_ids: Vec<ThreadId> =
                self.thread_event_channels.keys().copied().collect();
            for thread_id in tracked_thread_ids {
                if let Err(err) = app_server.thread_unsubscribe(thread_id).await {
                    tracing::warn!("failed to unsubscribe tracked thread {thread_id}: {err}");
                }
            }
        }
        self.config = config.clone();
        match app_server
            .start_thread_with_session_start_source(&config, session_start_source)
            .await
        {
            Ok(started) => {
                match self
                    .replace_chat_widget_with_app_server_thread(
                        tui,
                        app_server,
                        started,
                        initial_user_message,
                    )
                    .await
                {
                    Ok(()) => {
                        if let Some(summary) = summary {
                            let mut lines: Vec<Line<'static>> = Vec::new();
                            if let Some(usage_line) = summary.usage_line {
                                lines.push(usage_line.into());
                            }
                            if let Some(command) = summary.resume_hint {
                                let spans = vec![resume_hint_prefix.into(), command.cyan()];
                                lines.push(spans.into());
                            }
                            self.chat_widget.add_plain_history_lines(lines);
                        }
                        tui.frame_requester().schedule_frame();
                        true
                    }
                    Err(err) => {
                        self.chat_widget
                            .add_error_message(format!("{attach_error_prefix}: {err}"));
                        tui.frame_requester().schedule_frame();
                        false
                    }
                }
            }
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("{start_error_prefix}: {err}"));
                self.config.model = Some(model);
                tui.frame_requester().schedule_frame();
                false
            }
        }
    }

    pub(super) async fn replace_chat_widget_with_app_server_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        started: AppServerStartedThread,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
    ) -> Result<()> {
        // Initial messages are for freshly attached primary threads only. Thread switches and
        // resume/fork flows pass `None` so they cannot replay old history and then auto-submit a new
        // user turn by accident.
        self.reset_thread_event_state();
        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            initial_user_message,
        );
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        self.enqueue_primary_thread_session(started.session, started.turns)
            .await?;
        self.backfill_loaded_subagent_threads(app_server).await;
        Ok(())
    }

    /// Fetches all loaded threads from the app server and registers descendants of the primary
    /// thread in the navigation cache and chat widget metadata.
    ///
    /// Called after `replace_chat_widget_with_app_server_thread` during resume, fork, and new
    /// thread creation so that the `/agent` picker and keyboard navigation are pre-populated even
    /// if the TUI did not witness the original spawn events.
    ///
    /// The loaded-thread list is fetched in full (no pagination) and the spawn tree is walked
    /// by `find_loaded_subagent_threads_for_primary`. Each discovered subagent is registered via
    /// `upsert_agent_picker_thread`, which writes to both `AgentNavigationState` and the
    /// `ChatWidget` metadata map.
    pub(super) async fn backfill_loaded_subagent_threads(
        &mut self,
        app_server: &mut AppServerSession,
    ) -> bool {
        let Some(primary_thread_id) = self.primary_thread_id else {
            return false;
        };

        let loaded_thread_ids = match app_server
            .thread_loaded_list(ThreadLoadedListParams {
                cursor: None,
                limit: None,
            })
            .await
        {
            Ok(response) => response.data,
            Err(err) => {
                tracing::warn!(%err, "failed to list loaded threads for subagent backfill");
                return false;
            }
        };

        let mut threads = Vec::new();
        let mut had_read_error = false;
        for thread_id in loaded_thread_ids {
            let Ok(thread_id) = ThreadId::from_string(&thread_id) else {
                tracing::warn!("ignoring loaded thread with invalid id during subagent backfill");
                continue;
            };

            if thread_id == primary_thread_id {
                continue;
            }

            match app_server
                .thread_read(thread_id, /*include_turns*/ false)
                .await
            {
                Ok(thread) => threads.push(thread),
                Err(err) => {
                    had_read_error = true;
                    tracing::warn!(thread_id = %thread_id, %err, "failed to read loaded thread");
                }
            }
        }

        for thread in find_loaded_subagent_threads_for_primary(threads, primary_thread_id) {
            self.upsert_agent_picker_thread(
                thread.thread_id,
                thread.agent_nickname,
                thread.agent_role,
                /*is_closed*/ false,
            );
        }

        !had_read_error
    }

    /// Returns the adjacent thread id for keyboard navigation, backfilling from the server if the
    /// local cache has no neighbor.
    ///
    /// Tries the fast path first: ask `AgentNavigationState` directly. If it returns `None` (no
    /// adjacent entry exists, typically because the cache was never populated with remote
    /// subagents), performs a full `backfill_loaded_subagent_threads` and retries. This ensures the
    /// first next/previous keypress in a resumed remote session discovers subagents on demand
    /// without requiring the user to wait for a proactive fetch.
    pub(super) async fn adjacent_thread_id_with_backfill(
        &mut self,
        app_server: &mut AppServerSession,
        direction: AgentNavigationDirection,
    ) -> Option<ThreadId> {
        let current_thread = self.current_displayed_thread_id();
        if let Some(thread_id) = self
            .agent_navigation
            .adjacent_thread_id(current_thread, direction)
        {
            return Some(thread_id);
        }

        let primary_thread_id = self.primary_thread_id?;
        if self.last_subagent_backfill_attempt == Some(primary_thread_id) {
            return None;
        }

        if self.backfill_loaded_subagent_threads(app_server).await {
            self.last_subagent_backfill_attempt = Some(primary_thread_id);
        }
        self.agent_navigation
            .adjacent_thread_id(self.current_displayed_thread_id(), direction)
    }

    pub(super) fn fresh_session_config(&self) -> Config {
        let mut config = self.config.clone();
        config.service_tier = self.chat_widget.configured_service_tier();
        config
    }

    pub(super) async fn switch_auth_profile(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        selection: AuthProfileSelection,
    ) {
        self.pending_auth_profile_login = None;

        if !matches!(self.app_server_target, crate::AppServerTarget::Embedded) {
            self.chat_widget.add_error_message(
                "/login profile switching requires an embedded Codex Lab app server.".to_string(),
            );
            self.chat_widget.add_info_message(
                "Restart Codex Lab with `--auth-profile <name>` to use a profile with a shared or remote app server."
                    .to_string(),
                /*hint*/ None,
            );
            return;
        }

        let (auth_home, profile_name, profile_label) = match selection {
            AuthProfileSelection::Default => (
                self.config.codex_home.to_path_buf(),
                None,
                "default".to_string(),
            ),
            AuthProfileSelection::Named {
                ref profile_name, ..
            } => {
                let auth_home =
                    match codex_login::profile_home(&self.config.codex_home, profile_name.as_str())
                    {
                        Ok(auth_home) => auth_home,
                        Err(err) => {
                            self.chat_widget.add_error_message(format!(
                                "Invalid auth profile {profile_name:?}: {err}"
                            ));
                            return;
                        }
                    };
                (
                    auth_home,
                    Some(profile_name.clone()),
                    format!("`{profile_name}`"),
                )
            }
        };

        let login_after_switch = matches!(
            &selection,
            AuthProfileSelection::Named {
                login_after_switch: true,
                ..
            }
        );

        if auth_home == self.config.auth_home.as_path() {
            self.chat_widget.add_info_message(
                format!("Auth profile {profile_label} is already active."),
                /*hint*/ None,
            );
            if login_after_switch {
                self.start_auth_profile_login(app_server, profile_name.as_deref(), &profile_label)
                    .await;
            }
            return;
        }

        let auth_home = match AbsolutePathBuf::from_absolute_path(auth_home) {
            Ok(auth_home) => auth_home,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Invalid auth profile home for {profile_label}: {err}"
                ));
                return;
            }
        };
        let replacement_cloud_config_bundle = cloud_config_bundle_loader_for_storage(
            self.config.codex_home.to_path_buf(),
            auth_home.to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            self.config.cli_auth_credentials_store_mode,
            self.config.chatgpt_base_url.clone(),
        )
        .await;
        let config = match self
            .config_for_auth_profile_switch(
                auth_home.clone(),
                replacement_cloud_config_bundle.clone(),
            )
            .await
        {
            Ok(config) => config,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to load config for auth profile {profile_label}: {err}"
                ));
                return;
            }
        };

        let replacement_client = match crate::start_embedded_app_server(
            self.arg0_paths.clone(),
            config.clone(),
            self.cli_kv_overrides.clone(),
            self.loader_overrides.clone(),
            self.strict_config,
            replacement_cloud_config_bundle.clone(),
            self.feedback.clone(),
            /*log_db*/ None,
            self.state_db.clone(),
            self.environment_manager.clone(),
        )
        .await
        {
            Ok(client) => AppServerClient::InProcess(client),
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to start app server for auth profile {profile_label}: {err}"
                ));
                return;
            }
        };

        let mut replacement_session =
            AppServerSession::new(replacement_client, app_server.thread_params_mode())
                .with_remote_cwd_override(app_server.remote_cwd_override().map(PathBuf::from));
        let started = match replacement_session
            .start_thread_with_session_start_source(&config, Some(ThreadStartSource::Clear))
            .await
        {
            Ok(started) => started,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to start auth profile session for {profile_label}: {err}"
                ));
                if let Err(shutdown_err) = replacement_session.shutdown().await {
                    tracing::warn!(
                        "failed to shut down replacement app server after thread start failure: {shutdown_err}"
                    );
                }
                return;
            }
        };

        self.shutdown_current_thread(app_server).await;
        let tracked_thread_ids: Vec<ThreadId> =
            self.thread_event_channels.keys().copied().collect();
        for thread_id in tracked_thread_ids {
            if let Err(err) = app_server.thread_unsubscribe(thread_id).await {
                tracing::warn!("failed to unsubscribe tracked thread {thread_id}: {err}");
            }
        }

        let old_client = app_server.swap_client(replacement_session.into_client());
        self.config = config.clone();
        self.cloud_config_bundle = replacement_cloud_config_bundle;
        if let Err(err) = old_client.shutdown().await {
            tracing::warn!(
                "failed to shut down previous app server after auth profile switch: {err}"
            );
        }

        let switched = match self
            .replace_chat_widget_with_app_server_thread(
                tui, app_server, started, /*initial_user_message*/ None,
            )
            .await
        {
            Ok(()) => {
                tui.frame_requester().schedule_frame();
                true
            }
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to attach to auth profile session: {err}"));
                false
            }
        };
        if switched {
            self.chat_widget.add_info_message(
                format!("Using auth profile {profile_label} for this session."),
                Some("The previous session remains resumable.".to_string()),
            );
            if login_after_switch {
                self.start_auth_profile_login(app_server, profile_name.as_deref(), &profile_label)
                    .await;
            }
        }
    }

    pub(super) async fn switch_auth_account(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        selection: AuthAccountSelection,
    ) {
        self.pending_auth_profile_login = None;

        if !matches!(self.app_server_target, crate::AppServerTarget::Embedded) {
            self.chat_widget.add_error_message(
                "/login account switching requires an embedded Codex Lab app server.".to_string(),
            );
            self.chat_widget.add_info_message(
                "Use an embedded Codex Lab app server to activate stored accounts from /login."
                    .to_string(),
                /*hint*/ None,
            );
            return;
        }

        if !self.pending_primary_events.is_empty() {
            self.chat_widget.add_error_message(
                "Cannot switch stored accounts while the primary thread is still attaching."
                    .to_string(),
            );
            return;
        }

        let (_account, selected_auth) =
            match codex_login::auth_for_account(&self.config.codex_home, &selection.account_id) {
                Ok(auth) => auth,
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to load stored account {}: {err}",
                        selection.label
                    ));
                    return;
                }
            };
        let previous_auth = match codex_login::load_auth_dot_json(
            &self.config.codex_home,
            self.config.cli_auth_credentials_store_mode,
        ) {
            Ok(previous_auth) => previous_auth,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to read current auth before activating stored account {}: {err}",
                    selection.label
                ));
                return;
            }
        };
        let previous_active_account_id =
            match codex_login::get_active_account_id(&self.config.codex_home) {
                Ok(previous_active_account_id) => previous_active_account_id,
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Failed to read active account before activating stored account {}: {err}",
                        selection.label
                    ));
                    return;
                }
            };
        if let Err(err) = codex_login::save_auth(
            &self.config.codex_home,
            &selected_auth,
            self.config.cli_auth_credentials_store_mode,
        ) {
            self.chat_widget.add_error_message(format!(
                "Failed to prepare stored account {}: {err}",
                selection.label
            ));
            return;
        }
        let mut rollback = AuthSwitchRollback::new(
            self.config.codex_home.to_path_buf(),
            previous_auth,
            previous_active_account_id,
            self.config.cli_auth_credentials_store_mode,
        );

        match codex_login::set_active_account_id(
            &self.config.codex_home,
            Some(selection.account_id.clone()),
        ) {
            Ok(Some(_activated)) => {}
            Ok(None) => {
                if let Err(rollback_err) = rollback.restore_now() {
                    tracing::warn!(
                        "failed to restore auth after missing stored account activation: {rollback_err}"
                    );
                }
                self.chat_widget.add_error_message(format!(
                    "Failed to activate stored account {}: account disappeared before activation",
                    selection.label
                ));
                return;
            }
            Err(err) => {
                if let Err(rollback_err) = rollback.restore_now() {
                    tracing::warn!(
                        "failed to restore auth after stored account activation error: {rollback_err}"
                    );
                }
                self.chat_widget.add_error_message(format!(
                    "Failed to activate stored account {}: {err}",
                    selection.label
                ));
                return;
            }
        }

        let default_auth_home = self.config.codex_home.clone();
        let replacement_cloud_config_bundle = cloud_config_bundle_loader_for_storage(
            self.config.codex_home.to_path_buf(),
            default_auth_home.to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            self.config.cli_auth_credentials_store_mode,
            self.config.chatgpt_base_url.clone(),
        )
        .await;
        let config = match self
            .config_for_auth_profile_switch(
                default_auth_home,
                replacement_cloud_config_bundle.clone(),
            )
            .await
        {
            Ok(config) => config,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to load config for stored account {}: {err}",
                    selection.label
                ));
                return;
            }
        };

        let replacement_client = match crate::start_embedded_app_server(
            self.arg0_paths.clone(),
            config.clone(),
            self.cli_kv_overrides.clone(),
            self.loader_overrides.clone(),
            self.strict_config,
            replacement_cloud_config_bundle.clone(),
            self.feedback.clone(),
            /*log_db*/ None,
            self.state_db.clone(),
            self.environment_manager.clone(),
        )
        .await
        {
            Ok(client) => AppServerClient::InProcess(client),
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to start app server for stored account {}: {err}",
                    selection.label
                ));
                return;
            }
        };

        let mut replacement_session =
            AppServerSession::new(replacement_client, app_server.thread_params_mode())
                .with_remote_cwd_override(app_server.remote_cwd_override().map(PathBuf::from));
        let started = match replacement_session
            .start_thread_with_session_start_source(&config, Some(ThreadStartSource::Clear))
            .await
        {
            Ok(started) => started,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to start stored account session for {}: {err}",
                    selection.label
                ));
                if let Err(shutdown_err) = replacement_session.shutdown().await {
                    tracing::warn!(
                        "failed to shut down replacement app server after account start failure: {shutdown_err}"
                    );
                }
                return;
            }
        };

        rollback.disarm();

        self.shutdown_current_thread(app_server).await;
        let tracked_thread_ids: Vec<ThreadId> =
            self.thread_event_channels.keys().copied().collect();
        for thread_id in tracked_thread_ids {
            if let Err(err) = app_server.thread_unsubscribe(thread_id).await {
                tracing::warn!("failed to unsubscribe tracked thread {thread_id}: {err}");
            }
        }

        let old_client = app_server.swap_client(replacement_session.into_client());
        self.config = config;
        self.cloud_config_bundle = replacement_cloud_config_bundle;
        if let Err(err) = old_client.shutdown().await {
            tracing::warn!("failed to shut down previous app server after account switch: {err}");
        }

        match self
            .replace_chat_widget_with_app_server_thread(
                tui, app_server, started, /*initial_user_message*/ None,
            )
            .await
        {
            Ok(()) => {
                tui.frame_requester().schedule_frame();
                self.chat_widget.add_info_message(
                    format!("Using stored account {} for this session.", selection.label),
                    Some("The previous session remains resumable.".to_string()),
                );
            }
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to attach to stored account session: {err}"
                ));
            }
        }
    }

    pub(super) async fn start_login_add_account_chatgpt(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::Starting)
        {
            return;
        }

        if self.config.auth_home != self.config.codex_home {
            self.start_default_store_login_add_account_chatgpt().await;
            return;
        }

        let request_handle = app_server.request_handle();
        let response = request_handle
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id: app_server.next_request_id(),
                params: LoginAccountParams::Chatgpt {
                    codex_streamlined_login: false,
                },
            })
            .await;

        match response {
            Ok(LoginAccountResponse::Chatgpt { login_id, auth_url }) => {
                self.pending_login_add_account_id = Some(login_id.clone());
                self.completed_login_add_account_id = None;
                self.open_local_auth_url_in_browser(&request_handle, &auth_url);
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Waiting {
                        login_id,
                        auth_url,
                    });
            }
            Ok(other) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Failed(format!(
                        "Unexpected login response: {other:?}"
                    )));
            }
            Err(err) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Failed(format!(
                        "Failed to start ChatGPT login: {err}"
                    )));
            }
        }
    }

    pub(super) async fn cancel_login_add_account_chatgpt(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        if let Some(pending) = self.pending_direct_login_add_account.take() {
            pending.shutdown.shutdown();
            return;
        }

        let Some(login_id) = self.pending_login_add_account_id.take() else {
            return;
        };
        self.completed_login_add_account_id = None;
        let request_handle = app_server.request_handle();
        if let Err(err) = request_handle
            .request_typed::<CancelLoginAccountResponse>(ClientRequest::CancelLoginAccount {
                request_id: app_server.next_request_id(),
                params: CancelLoginAccountParams { login_id },
            })
            .await
        {
            tracing::warn!("failed to cancel add-account ChatGPT login: {err}");
        }
    }

    async fn start_default_store_login_add_account_chatgpt(&mut self) {
        let opts = ServerOptions {
            open_browser: false,
            ..ServerOptions::new(
                self.config.codex_home.to_path_buf(),
                CLIENT_ID.to_string(),
                self.config.forced_chatgpt_workspace_id.clone(),
                self.config.cli_auth_credentials_store_mode,
            )
        };

        let server = match codex_login::run_login_server(opts) {
            Ok(server) => server,
            Err(err) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Failed(format!(
                        "Failed to start ChatGPT login: {err}"
                    )));
                return;
            }
        };

        let auth_url = server.auth_url.clone();
        let shutdown = server.cancel_handle();
        let completion_tx = self.app_event_tx.clone();
        self.direct_login_add_account_attempt_id =
            self.direct_login_add_account_attempt_id.wrapping_add(1);
        let attempt_id = self.direct_login_add_account_attempt_id;
        tokio::spawn(async move {
            let result = server
                .block_until_done()
                .await
                .map_err(|err| err.to_string());
            completion_tx.send(AppEvent::LoginAddAccountChatGptCompleted { attempt_id, result });
        });

        self.pending_direct_login_add_account = Some(PendingDirectLoginAddAccount {
            attempt_id,
            shutdown,
        });
        self.open_url_in_browser(auth_url.clone());
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::Waiting {
                login_id: "default-store".to_string(),
                auth_url,
            })
        {
            if let Some(pending) = self.pending_direct_login_add_account.take() {
                pending.shutdown.shutdown();
            }
        }
    }

    pub(super) async fn complete_login_add_account_chatgpt(
        &mut self,
        attempt_id: u64,
        result: Result<(), String>,
    ) {
        if self
            .pending_direct_login_add_account
            .as_ref()
            .map(|pending| pending.attempt_id)
            != Some(attempt_id)
        {
            return;
        }

        self.pending_direct_login_add_account = None;
        match result {
            Ok(()) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Complete);
            }
            Err(err) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Failed(format!(
                        "ChatGPT login did not complete: {err}"
                    )));
            }
        }
    }

    fn open_local_auth_url_in_browser(
        &mut self,
        request_handle: &AppServerRequestHandle,
        url: &str,
    ) {
        if matches!(request_handle, AppServerRequestHandle::InProcess(_)) {
            self.open_url_in_browser(url.to_string());
        }
    }

    async fn start_auth_profile_login(
        &mut self,
        app_server: &mut AppServerSession,
        profile_name: Option<&str>,
        profile_label: &str,
    ) {
        let request_handle = app_server.request_handle();
        let response = request_handle
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id: app_server.next_request_id(),
                params: LoginAccountParams::Chatgpt {
                    codex_streamlined_login: false,
                },
            })
            .await;

        match response {
            Ok(LoginAccountResponse::Chatgpt { login_id, auth_url }) => {
                if let Some(profile_name) = profile_name {
                    self.pending_auth_profile_login = Some(PendingAuthProfileLogin {
                        login_id,
                        profile_name: profile_name.to_string(),
                        profile_label: profile_label.to_string(),
                    });
                }
                self.open_url_in_browser(auth_url.clone());
                self.chat_widget.add_info_message(
                    format!("Started ChatGPT login for auth profile {profile_label}."),
                    Some(format!("If your browser did not open, visit {auth_url}")),
                );
            }
            Ok(LoginAccountResponse::ApiKey {}) => {
                self.chat_widget.add_info_message(
                    format!("API key login configured for auth profile {profile_label}."),
                    /*hint*/ None,
                );
            }
            Ok(LoginAccountResponse::ChatgptDeviceCode {
                verification_url,
                user_code,
                ..
            }) => {
                self.chat_widget.add_info_message(
                    format!("Started device-code login for auth profile {profile_label}."),
                    Some(format!("Visit {verification_url} and enter {user_code}.")),
                );
            }
            Ok(LoginAccountResponse::ChatgptAuthTokens {}) => {
                self.chat_widget.add_info_message(
                    format!("ChatGPT tokens configured for auth profile {profile_label}."),
                    /*hint*/ None,
                );
            }
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to start login for auth profile {profile_label}: {err}"
                ));
            }
        }
    }

    pub(super) async fn resume_target_session(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        target_session: SessionTarget,
    ) -> Result<AppRunControl> {
        if self.ignore_same_thread_resume(&target_session) {
            tui.frame_requester().schedule_frame();
            return Ok(AppRunControl::Continue);
        }

        let current_cwd = self.config.cwd.to_path_buf();
        let resume_cwd = if self.app_server_target.uses_remote_workspace() {
            current_cwd.clone()
        } else {
            match crate::session_resume::resolve_cwd_for_resume_or_fork(
                tui,
                self.state_db.as_deref(),
                &current_cwd,
                target_session.thread_id,
                target_session.path.as_deref(),
                CwdPromptAction::Resume,
                /*allow_prompt*/ true,
            )
            .await?
            {
                crate::session_resume::ResolveCwdOutcome::Continue(Some(cwd)) => cwd,
                crate::session_resume::ResolveCwdOutcome::Continue(None) => current_cwd.clone(),
                crate::session_resume::ResolveCwdOutcome::Exit => {
                    return Ok(AppRunControl::Exit(ExitReason::UserRequested));
                }
            }
        };

        let mut resume_config = match self
            .rebuild_config_for_resume_or_fallback(&current_cwd, resume_cwd)
            .await
        {
            Ok(cfg) => cfg,
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to rebuild configuration for resume: {err}"
                ));
                return Ok(AppRunControl::Continue);
            }
        };
        self.apply_runtime_policy_overrides(&mut resume_config);

        let summary = session_summary(
            self.chat_widget.token_usage(),
            self.chat_widget.thread_id(),
            self.chat_widget.thread_name(),
            self.chat_widget.rollout_path().as_deref(),
        );
        match app_server
            .resume_thread(resume_config.clone(), target_session.thread_id)
            .await
        {
            Ok(resumed) => {
                let resumed_thread_id = resumed.session.thread_id;
                self.shutdown_current_thread(app_server).await;
                self.config = resume_config;
                tui.set_notification_settings(
                    self.config.tui_notifications.method,
                    self.config.tui_notifications.condition,
                );
                self.file_search
                    .update_search_dir(self.config.cwd.to_path_buf());
                match self
                    .replace_chat_widget_with_app_server_thread(
                        tui, app_server, resumed, /*initial_user_message*/ None,
                    )
                    .await
                {
                    Ok(()) => {
                        if let Some(summary) = summary {
                            let mut lines: Vec<Line<'static>> = Vec::new();
                            if let Some(usage_line) = summary.usage_line {
                                lines.push(usage_line.into());
                            }
                            if let Some(command) = summary.resume_hint {
                                let spans =
                                    vec!["To continue this session, run ".into(), command.cyan()];
                                lines.push(spans.into());
                            }
                            self.chat_widget.add_plain_history_lines(lines);
                        }
                        self.maybe_prompt_resume_paused_goal_after_resume(
                            app_server,
                            resumed_thread_id,
                        )
                        .await;
                    }
                    Err(err) => {
                        self.chat_widget.add_error_message(format!(
                            "Failed to attach to resumed app-server thread: {err}"
                        ));
                    }
                }
            }
            Err(err) => {
                let path_display = target_session.display_label();
                self.chat_widget.add_error_message(format!(
                    "Failed to resume session from {path_display}: {err}"
                ));
            }
        }

        Ok(AppRunControl::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_thread_read_error_detection_matches_not_loaded_errors() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread not loaded: thr_123"
        );

        assert!(App::is_terminal_thread_read_error(&err));
    }

    #[test]
    fn terminal_thread_read_error_detection_ignores_transient_failures() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read transport error: broken pipe"
        );

        assert!(!App::is_terminal_thread_read_error(&err));
    }

    #[test]
    fn closed_state_for_thread_read_error_preserves_live_state_without_cache_on_transient_error() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read transport error: broken pipe"
        );

        assert!(!App::closed_state_for_thread_read_error(
            &err, /*existing_is_closed*/ None
        ));
    }

    #[test]
    fn closed_state_for_thread_read_error_marks_terminal_uncached_threads_closed() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread not loaded: thr_123"
        );

        assert!(App::closed_state_for_thread_read_error(
            &err, /*existing_is_closed*/ None
        ));
    }

    #[test]
    fn include_turns_fallback_detection_handles_unmaterialized_and_ephemeral_threads() {
        let unmaterialized = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread thr_123 is not materialized yet; includeTurns is unavailable before first user message"
        );
        let ephemeral = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: ephemeral threads do not support includeTurns"
        );

        assert!(App::can_fallback_from_include_turns_error(&unmaterialized));
        assert!(App::can_fallback_from_include_turns_error(&ephemeral));
    }
}
