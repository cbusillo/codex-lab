//! Session, resume, fork, and subagent selection lifecycle for the TUI app.
//!
//! This module owns the high-level transitions between app-server threads: starting fresh sessions,
//! resuming/forking saved sessions, replacing ChatWidget instances, and maintaining the agent picker
//! cache used for multi-agent navigation.

use super::*;
use crate::app::PendingDirectLoginAddAccount;
use crate::app::PendingDirectLoginAddAccountCancellation;
use crate::app::PendingDirectLoginAddAccountKind;
use crate::app_event::AuthAccountSelection;
use crate::app_event::AuthProfileSelection;
use crate::app_event::RemoveAuthAccountSelection;
use crate::bottom_pane::LoginAccountsFeedback;
use crate::bottom_pane::LoginAddAccountState;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::CancelLoginAccountParams;
use codex_app_server_protocol::CancelLoginAccountResponse;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::LoginAccountParams;
use codex_app_server_protocol::LoginAccountResponse;
use codex_cloud_config::cloud_config_bundle_loader_for_storage;
use codex_config::CloudConfigBundleLoader;
use codex_login::CLIENT_ID;
use codex_login::ServerOptions;
use tokio_util::sync::CancellationToken;

struct AppServerThreadUiSnapshot {
    chat_widget: ChatWidget,
    thread_event_channels: HashMap<ThreadId, ThreadEventChannel>,
    thread_event_listener_tasks: HashMap<ThreadId, JoinHandle<()>>,
    agent_navigation: AgentNavigationState,
    side_threads: HashMap<ThreadId, SideThreadState>,
    active_thread_id: Option<ThreadId>,
    active_thread_rx: Option<mpsc::Receiver<ThreadBufferedEvent>>,
    primary_thread_id: Option<ThreadId>,
    last_subagent_backfill_attempt: Option<ThreadId>,
    primary_session_configured: Option<ThreadSessionState>,
    pending_primary_events: VecDeque<ThreadBufferedEvent>,
    pending_app_server_requests: PendingAppServerRequests,
    pending_startup_thread_start: bool,
}

impl AppServerThreadUiSnapshot {
    fn take_from(app: &mut App, replacement_chat_widget: ChatWidget) -> Self {
        Self {
            chat_widget: std::mem::replace(&mut app.chat_widget, replacement_chat_widget),
            thread_event_channels: std::mem::take(&mut app.thread_event_channels),
            thread_event_listener_tasks: std::mem::take(&mut app.thread_event_listener_tasks),
            agent_navigation: std::mem::take(&mut app.agent_navigation),
            side_threads: std::mem::take(&mut app.side_threads),
            active_thread_id: app.active_thread_id.take(),
            active_thread_rx: app.active_thread_rx.take(),
            primary_thread_id: app.primary_thread_id.take(),
            last_subagent_backfill_attempt: app.last_subagent_backfill_attempt.take(),
            primary_session_configured: app.primary_session_configured.take(),
            pending_primary_events: std::mem::take(&mut app.pending_primary_events),
            pending_app_server_requests: std::mem::take(&mut app.pending_app_server_requests),
            pending_startup_thread_start: std::mem::take(&mut app.pending_startup_thread_start),
        }
    }

    fn restore(self, app: &mut App) {
        app.abort_all_thread_event_listeners();
        app.chat_widget = self.chat_widget;
        app.thread_event_channels = self.thread_event_channels;
        app.thread_event_listener_tasks = self.thread_event_listener_tasks;
        app.agent_navigation = self.agent_navigation;
        app.side_threads = self.side_threads;
        app.active_thread_id = self.active_thread_id;
        app.active_thread_rx = self.active_thread_rx;
        app.primary_thread_id = self.primary_thread_id;
        app.last_subagent_backfill_attempt = self.last_subagent_backfill_attempt;
        app.primary_session_configured = self.primary_session_configured;
        app.pending_primary_events = self.pending_primary_events;
        app.pending_app_server_requests = self.pending_app_server_requests;
        app.pending_startup_thread_start = self.pending_startup_thread_start;
        app.sync_active_agent_label();
    }

    fn previous_thread_ids(&self) -> Vec<ThreadId> {
        let mut thread_ids: Vec<ThreadId> = self.thread_event_channels.keys().copied().collect();
        for thread_id in [
            self.chat_widget.thread_id(),
            self.active_thread_id,
            self.primary_thread_id,
        ]
        .into_iter()
        .flatten()
        {
            if !thread_ids.contains(&thread_id) {
                thread_ids.push(thread_id);
            }
        }
        thread_ids
    }

    async fn discard_previous_threads(
        mut self,
        app_server: &mut AppServerSession,
    ) -> Vec<ThreadId> {
        let thread_ids = self.previous_thread_ids();

        for thread_id in thread_ids.iter().copied() {
            if let Err(err) = app_server.thread_unsubscribe(thread_id).await {
                tracing::warn!("failed to unsubscribe replaced thread {thread_id}: {err}");
            }
        }

        for handle in self
            .thread_event_listener_tasks
            .drain()
            .map(|(_, handle)| handle)
        {
            handle.abort();
        }

        thread_ids
    }
}

impl App {
    pub(super) async fn show_login_accounts_view(&mut self, app_server: &mut AppServerSession) {
        if matches!(self.app_server_target, crate::AppServerTarget::Embedded) {
            self.chat_widget.show_login_accounts_view();
            return;
        }

        match app_server.list_accounts().await {
            Ok(response) => {
                self.chat_widget
                    .show_login_accounts_view_with_loaded_accounts(
                        response.accounts,
                        /*feedback*/ None,
                    );
            }
            Err(err) => {
                self.chat_widget
                    .show_login_accounts_view_with_loaded_accounts(
                        Vec::new(),
                        Some(LoginAccountsFeedback::Error(format!(
                            "Failed to read accounts from app server: {err}"
                        ))),
                    );
            }
        }
    }

    async fn commit_replacement_app_server_thread(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        mut replacement_session: AppServerSession,
        started: AppServerStartedThread,
        config: Config,
        cloud_config_bundle: CloudConfigBundleLoader,
        old_client_shutdown_warning: &str,
    ) -> Result<()> {
        let previous_config = std::mem::replace(&mut self.config, config);
        let previous_cloud_config_bundle =
            std::mem::replace(&mut self.cloud_config_bundle, cloud_config_bundle);
        if let Err(err) = self
            .replace_chat_widget_with_app_server_thread_preserving_previous_ui_on_error(
                tui,
                app_server,
                &mut replacement_session,
                started,
            )
            .await
        {
            if let Err(shutdown_err) = replacement_session.shutdown().await {
                tracing::warn!(
                    "failed to shut down replacement app server after attach failure: {shutdown_err}"
                );
            }
            self.config = previous_config;
            self.cloud_config_bundle = previous_cloud_config_bundle;
            return Err(err);
        }

        let old_client = app_server.swap_client(replacement_session.into_client());
        drop(previous_config);
        drop(previous_cloud_config_bundle);
        if let Err(err) = old_client.shutdown().await {
            tracing::warn!("{old_client_shutdown_warning}: {err}");
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }

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
        self.replace_chat_widget_with_app_server_thread_inner(
            tui,
            app_server,
            started,
            initial_user_message,
            /*preserve_previous_ui_on_error*/ false,
            /*previous_app_server_for_cleanup*/ None,
        )
        .await
    }

    async fn replace_chat_widget_with_app_server_thread_preserving_previous_ui_on_error(
        &mut self,
        tui: &mut tui::Tui,
        previous_app_server: &mut AppServerSession,
        replacement_app_server: &mut AppServerSession,
        started: AppServerStartedThread,
    ) -> Result<()> {
        self.replace_chat_widget_with_app_server_thread_inner(
            tui,
            replacement_app_server,
            started,
            /*initial_user_message*/ None,
            /*preserve_previous_ui_on_error*/ true,
            Some(previous_app_server),
        )
        .await
    }

    async fn replace_chat_widget_with_app_server_thread_inner(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        started: AppServerStartedThread,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
        preserve_previous_ui_on_error: bool,
        previous_app_server_for_cleanup: Option<&mut AppServerSession>,
    ) -> Result<()> {
        // Initial messages are for freshly attached primary threads only. Thread switches and
        // resume/fork flows pass `None` so they cannot replay old history and then auto-submit a new
        // user turn by accident.
        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            initial_user_message,
        );
        let replacement_chat_widget = ChatWidget::new_with_app_event(init);
        let previous_ui = if preserve_previous_ui_on_error {
            Some(AppServerThreadUiSnapshot::take_from(
                self,
                replacement_chat_widget,
            ))
        } else {
            self.reset_thread_event_state();
            self.replace_chat_widget(replacement_chat_widget);
            None
        };
        let result = self
            .enqueue_primary_thread_session(started.session, started.turns)
            .await;
        self.finish_app_server_thread_replacement(
            app_server,
            previous_ui,
            previous_app_server_for_cleanup,
            result,
        )
        .await
    }

    async fn finish_app_server_thread_replacement(
        &mut self,
        app_server: &mut AppServerSession,
        previous_ui: Option<AppServerThreadUiSnapshot>,
        previous_app_server_for_cleanup: Option<&mut AppServerSession>,
        result: Result<()>,
    ) -> Result<()> {
        if let Err(err) = result {
            if let Some(previous_ui) = previous_ui {
                previous_ui.restore(self);
            }
            return Err(err);
        }
        if let Some(previous_ui) = previous_ui {
            if let Some(previous_app_server) = previous_app_server_for_cleanup {
                previous_ui
                    .discard_previous_threads(previous_app_server)
                    .await;
                self.backtrack.pending_rollback = None;
            }
        }
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

        if !self.pending_primary_events.is_empty() {
            self.chat_widget.add_error_message(
                "Cannot switch auth profiles while the primary thread is still attaching."
                    .to_string(),
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

        let switched = match self
            .commit_replacement_app_server_thread(
                tui,
                app_server,
                replacement_session,
                started,
                config.clone(),
                replacement_cloud_config_bundle,
                "failed to shut down previous app server after auth profile switch",
            )
            .await
        {
            Ok(()) => true,
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
        _tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        selection: AuthAccountSelection,
    ) {
        self.pending_auth_profile_login = None;

        if !self.pending_primary_events.is_empty() {
            self.chat_widget.add_error_message(
                "Cannot switch stored accounts while the primary thread is still attaching."
                    .to_string(),
            );
            return;
        }

        match app_server
            .switch_active_account(selection.account_id.clone())
            .await
        {
            Ok(()) => {
                self.chat_widget.add_info_message(
                    format!("Using stored account {} for this session.", selection.label),
                    Some("The current session will use this account for new turns.".to_string()),
                );
            }
            Err(err) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to activate stored account {}: {err}",
                    selection.label
                ));
            }
        }
    }

    pub(super) async fn remove_auth_account(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        selection: RemoveAuthAccountSelection,
    ) {
        if !self.pending_primary_events.is_empty() {
            self.chat_widget.add_error_message(
                "Cannot disconnect stored accounts while the primary thread is still attaching."
                    .to_string(),
            );
            return;
        }

        let was_default_active = codex_login::get_active_account_id(&self.config.codex_home)
            .ok()
            .flatten()
            .is_some_and(|active_id| active_id == selection.account_id);
        let current_session_uses_default_auth_home =
            self.config.auth_home == self.config.codex_home;
        let needs_session_restart = was_default_active && current_session_uses_default_auth_home;
        if needs_session_restart
            && !matches!(self.app_server_target, crate::AppServerTarget::Embedded)
        {
            self.chat_widget.add_error_message(
                "/login active-account disconnect requires an embedded Codex Lab app server."
                    .to_string(),
            );
            self.chat_widget.add_info_message(
                "Use an embedded Codex Lab app server to disconnect the active stored account from /login."
                    .to_string(),
                /*hint*/ None,
            );
            return;
        }

        let removed =
            match codex_login::remove_account(&self.config.codex_home, &selection.account_id) {
                Ok(removed) => removed,
                Err(err) => {
                    self.chat_widget
                        .show_login_accounts_view_with_feedback(Some(
                            LoginAccountsFeedback::Error(format!(
                                "Failed to disconnect {}: {err}",
                                selection.label
                            )),
                        ));
                    return;
                }
            };

        let Some(_removed) = removed else {
            self.chat_widget
                .show_login_accounts_view_with_feedback(Some(LoginAccountsFeedback::Error(
                    format!("Stored account {} no longer exists.", selection.label),
                )));
            return;
        };

        let mut feedback =
            LoginAccountsFeedback::Info(format!("Disconnected {}.", selection.label));

        if was_default_active {
            match codex_login::get_active_account_id(&self.config.codex_home) {
                Ok(Some(fallback_account_id)) => {
                    if let Err(err) = codex_login::activate_account(
                        &self.config.codex_home,
                        &fallback_account_id,
                        self.config.cli_auth_credentials_store_mode,
                    ) {
                        let clear_result = codex_login::clear_active_account(
                            &self.config.codex_home,
                            self.config.cli_auth_credentials_store_mode,
                        );
                        if let Err(clear_err) = clear_result {
                            self.chat_widget.show_login_accounts_view_with_feedback(Some(
                                LoginAccountsFeedback::Error(format!(
                                    "Disconnected {}, but failed to activate the next account ({err}) or clear active auth ({clear_err}).",
                                    selection.label
                                )),
                            ));
                            return;
                        }
                        feedback = LoginAccountsFeedback::Error(format!(
                            "Disconnected {}, but failed to activate the next account: {err}. Active auth was cleared.",
                            selection.label
                        ));
                    }
                }
                Ok(None) => {
                    if let Err(err) = codex_login::clear_active_account(
                        &self.config.codex_home,
                        self.config.cli_auth_credentials_store_mode,
                    ) {
                        self.chat_widget
                            .show_login_accounts_view_with_feedback(Some(
                                LoginAccountsFeedback::Error(format!(
                                    "Disconnected {}, but failed to clear active auth: {err}",
                                    selection.label
                                )),
                            ));
                        return;
                    }
                }
                Err(err) => {
                    self.chat_widget.show_login_accounts_view_with_feedback(Some(
                        LoginAccountsFeedback::Error(format!(
                            "Disconnected {}, but failed to read the next active account: {err}",
                            selection.label
                        )),
                    ));
                    return;
                }
            }

            if needs_session_restart {
                if let Err(err) = self
                    .restart_default_auth_session_after_account_removal(
                        tui,
                        app_server,
                        &selection.label,
                    )
                    .await
                {
                    self.chat_widget.add_error_message(err);
                    return;
                }
            }
        }

        self.chat_widget
            .show_login_accounts_view_with_feedback(Some(feedback));
    }

    async fn restart_default_auth_session_after_account_removal(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        label: &str,
    ) -> Result<(), String> {
        if !matches!(self.app_server_target, crate::AppServerTarget::Embedded) {
            return Ok(());
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
        let config = self
            .config_for_auth_profile_switch(
                default_auth_home,
                replacement_cloud_config_bundle.clone(),
            )
            .await
            .map_err(|err| format!("Failed to load config after disconnecting {label}: {err}"))?;

        let replacement_client = crate::start_embedded_app_server(
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
        .map(AppServerClient::InProcess)
        .map_err(|err| {
            format!("Failed to restart app server after disconnecting {label}: {err}")
        })?;

        let mut replacement_session =
            AppServerSession::new(replacement_client, app_server.thread_params_mode())
                .with_remote_cwd_override(app_server.remote_cwd_override().map(PathBuf::from));
        let started = match replacement_session
            .start_thread_with_session_start_source(&config, Some(ThreadStartSource::Clear))
            .await
        {
            Ok(started) => started,
            Err(err) => {
                if let Err(shutdown_err) = replacement_session.shutdown().await {
                    tracing::warn!(
                        "failed to shut down replacement app server after account removal start failure: {shutdown_err}"
                    );
                }
                return Err(format!(
                    "Failed to start session after disconnecting {label}: {err}"
                ));
            }
        };

        self.commit_replacement_app_server_thread(
            tui,
            app_server,
            replacement_session,
            started,
            config,
            replacement_cloud_config_bundle,
            "failed to shut down previous app server after account removal",
        )
        .await
        .map_err(|err| format!("Failed to attach after disconnecting {label}: {err}"))?;
        Ok(())
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

    pub(super) async fn save_login_add_account_api_key(
        &mut self,
        app_server: &mut AppServerSession,
        api_key: &str,
    ) {
        let trimmed_key = api_key.trim();
        if trimmed_key.is_empty() {
            self.chat_widget
                .update_login_add_account_view(LoginAddAccountState::ApiKeyFailed(
                    "API key cannot be empty".to_string(),
                ));
            return;
        }

        self.chat_widget
            .update_login_add_account_view(LoginAddAccountState::SavingApiKey);

        if self.config.auth_home != self.config.codex_home {
            // /login manages the default stored-account list; named auth
            // profiles use their dedicated login commands instead.
            match codex_login::login_with_api_key(
                &self.config.codex_home,
                trimmed_key,
                self.config.cli_auth_credentials_store_mode,
            ) {
                Ok(()) => {
                    self.chat_widget
                        .update_login_add_account_view(LoginAddAccountState::Complete);
                }
                Err(err) => {
                    self.chat_widget.update_login_add_account_view(
                        LoginAddAccountState::ApiKeyFailed(format!(
                            "Failed to store API key: {err}"
                        )),
                    );
                }
            }
            return;
        }

        let response = app_server
            .request_handle()
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id: app_server.next_request_id(),
                params: LoginAccountParams::ApiKey {
                    api_key: trimmed_key.to_string(),
                },
            })
            .await;

        match response {
            Ok(LoginAccountResponse::ApiKey {}) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Complete);
            }
            Ok(other) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::ApiKeyFailed(format!(
                        "Unexpected login response: {other:?}"
                    )));
            }
            Err(err) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::ApiKeyFailed(format!(
                        "Failed to store API key: {err}"
                    )));
            }
        }
    }

    pub(super) async fn start_login_add_account_device_code(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        self.cancel_login_add_account_chatgpt(app_server).await;
        if !self
            .chat_widget
            .update_login_add_account_view(LoginAddAccountState::DeviceCodeStarting)
        {
            return;
        }

        if self.config.auth_home != self.config.codex_home {
            self.start_default_store_login_add_account_device_code()
                .await;
            return;
        }

        let response = app_server
            .request_handle()
            .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                request_id: app_server.next_request_id(),
                params: LoginAccountParams::ChatgptDeviceCode,
            })
            .await;

        match response {
            Ok(LoginAccountResponse::ChatgptDeviceCode {
                login_id,
                verification_url,
                user_code,
            }) => {
                self.pending_login_add_account_id = Some(login_id.clone());
                self.completed_login_add_account_id = None;
                if !self.chat_widget.update_login_add_account_view(
                    LoginAddAccountState::DeviceCodeWaiting {
                        login_id: login_id.clone(),
                        verification_url,
                        user_code,
                    },
                ) {
                    self.pending_login_add_account_id = None;
                    self.completed_login_add_account_id = None;
                    let request_handle = app_server.request_handle();
                    if let Err(err) = request_handle
                        .request_typed::<CancelLoginAccountResponse>(
                            ClientRequest::CancelLoginAccount {
                                request_id: app_server.next_request_id(),
                                params: CancelLoginAccountParams { login_id },
                            },
                        )
                        .await
                    {
                        tracing::warn!("failed to cancel add-account device-code login: {err}");
                    }
                }
            }
            Ok(other) => {
                self.chat_widget.update_login_add_account_view(
                    LoginAddAccountState::DeviceCodeFailed(format!(
                        "Unexpected login response: {other:?}"
                    )),
                );
            }
            Err(err) => {
                self.chat_widget.update_login_add_account_view(
                    LoginAddAccountState::DeviceCodeFailed(format!(
                        "Failed to start code login: {err}"
                    )),
                );
            }
        }
    }

    async fn start_default_store_login_add_account_device_code(&mut self) {
        let opts = ServerOptions::new(
            self.config.codex_home.to_path_buf(),
            CLIENT_ID.to_string(),
            self.config.forced_chatgpt_workspace_id.clone(),
            self.config.cli_auth_credentials_store_mode,
        );

        let device_code = match codex_login::request_device_code(&opts).await {
            Ok(device_code) => device_code,
            Err(err) => {
                self.chat_widget.update_login_add_account_view(
                    LoginAddAccountState::DeviceCodeFailed(format!(
                        "Failed to start code login: {err}"
                    )),
                );
                return;
            }
        };

        self.direct_login_add_account_attempt_id =
            self.direct_login_add_account_attempt_id.wrapping_add(1);
        let attempt_id = self.direct_login_add_account_attempt_id;
        let login_id = format!("default-device-code-{attempt_id}");
        let verification_url = device_code.verification_url.clone();
        let user_code = device_code.user_code.clone();
        let completion_tx = self.app_event_tx.clone();
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancel_for_task.cancelled() => Err("Login was not completed".to_string()),
                result = codex_login::complete_device_code_login(opts, device_code) => {
                    result.map_err(|err| err.to_string())
                }
            };
            completion_tx.send(AppEvent::LoginAddAccountChatGptCompleted { attempt_id, result });
        });

        self.pending_direct_login_add_account = Some(PendingDirectLoginAddAccount {
            attempt_id,
            cancellation: PendingDirectLoginAddAccountCancellation::DeviceCode(cancel),
        });
        if !self.chat_widget.update_login_add_account_view(
            LoginAddAccountState::DeviceCodeWaiting {
                login_id,
                verification_url,
                user_code,
            },
        ) {
            if let Some(pending) = self.pending_direct_login_add_account.take() {
                pending.cancellation.cancel();
            }
        }
    }

    pub(super) async fn cancel_login_add_account_chatgpt(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        if let Some(pending) = self.pending_direct_login_add_account.take() {
            pending.cancellation.cancel();
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
            cancellation: PendingDirectLoginAddAccountCancellation::Browser(shutdown),
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
                pending.cancellation.cancel();
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

        let Some(pending) = self.pending_direct_login_add_account.take() else {
            return;
        };
        let login_kind = pending.cancellation.kind();
        match result {
            Ok(()) => {
                self.chat_widget
                    .update_login_add_account_view(LoginAddAccountState::Complete);
            }
            Err(err) => {
                let message = format!("ChatGPT login did not complete: {err}");
                let state = match login_kind {
                    PendingDirectLoginAddAccountKind::Browser => {
                        LoginAddAccountState::Failed(message)
                    }
                    PendingDirectLoginAddAccountKind::DeviceCode => {
                        LoginAddAccountState::DeviceCodeFailed(message)
                    }
                };
                self.chat_widget.update_login_add_account_view(state);
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
    use crate::app::test_support::make_test_app;
    use crate::chatwidget::ChatWidgetInit;
    use crate::session_state::ThreadSessionState;
    use codex_app_server_protocol::AskForApproval;
    use codex_app_server_protocol::ServerNotification;
    use codex_app_server_protocol::WarningNotification;
    use codex_protocol::ThreadId;

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

    #[tokio::test]
    async fn app_server_thread_ui_snapshot_restores_previous_thread_state() {
        let mut app = make_test_app().await;
        let original_thread_id = ThreadId::new();
        let replacement_thread_id = ThreadId::new();
        app.primary_thread_id = Some(original_thread_id);
        app.active_thread_id = Some(original_thread_id);
        app.thread_event_channels.insert(
            original_thread_id,
            ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
        );
        app.agent_navigation.upsert(
            original_thread_id,
            Some("main".to_string()),
            Some("primary".to_string()),
            /*is_closed*/ false,
        );
        app.pending_primary_events
            .push_back(ThreadBufferedEvent::Notification(
                ServerNotification::Warning(WarningNotification {
                    thread_id: Some(original_thread_id.to_string()),
                    message: "pending".to_string(),
                }),
            ));
        let original_config = app.config.clone();
        let replacement_config = {
            let mut config = original_config.clone();
            config.model = Some("replacement-model".to_string());
            config
        };
        let replacement_chat_widget = replacement_chat_widget(&app, replacement_config);

        let snapshot = AppServerThreadUiSnapshot::take_from(&mut app, replacement_chat_widget);
        app.primary_thread_id = Some(replacement_thread_id);
        app.active_thread_id = Some(replacement_thread_id);
        app.thread_event_channels.insert(
            replacement_thread_id,
            ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
        );

        snapshot.restore(&mut app);

        assert_eq!(app.chat_widget.config_ref().model, original_config.model);
        assert_eq!(app.primary_thread_id, Some(original_thread_id));
        assert_eq!(app.active_thread_id, Some(original_thread_id));
        assert!(app.thread_event_channels.contains_key(&original_thread_id));
        assert!(
            !app.thread_event_channels
                .contains_key(&replacement_thread_id)
        );
        assert!(app.agent_navigation.get(&original_thread_id).is_some());
        assert_eq!(app.pending_primary_events.len(), 1);
    }

    #[tokio::test]
    async fn app_server_thread_ui_snapshot_discards_previous_listener_tasks() -> Result<()> {
        struct DropNotify(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for DropNotify {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }

        let mut app = make_test_app().await;
        let original_thread_id = ThreadId::new();
        app.primary_thread_id = Some(original_thread_id);
        app.active_thread_id = Some(original_thread_id);
        app.thread_event_channels.insert(
            original_thread_id,
            ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
        );
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        app.thread_event_listener_tasks.insert(
            original_thread_id,
            tokio::spawn(async move {
                let _notify = DropNotify(Some(dropped_tx));
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            }),
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx)
            .await
            .expect("listener task should start")
            .expect("listener task should notify after starting");

        let replacement_chat_widget = replacement_chat_widget(&app, app.config.clone());
        let snapshot = AppServerThreadUiSnapshot::take_from(&mut app, replacement_chat_widget);
        let mut app_server =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

        let discarded_thread_ids = snapshot.discard_previous_threads(&mut app_server).await;

        assert_eq!(discarded_thread_ids, vec![original_thread_id]);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("listener task should be aborted")
            .expect("listener task should notify on drop");
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn app_server_thread_ui_snapshot_collects_all_previous_thread_ids() {
        let mut app = make_test_app().await;
        let channel_thread_id = ThreadId::new();
        let chat_widget_thread_id = ThreadId::new();
        let active_thread_id = ThreadId::new();
        let primary_thread_id = ThreadId::new();
        app.chat_widget
            .handle_thread_session(test_thread_session(&app, chat_widget_thread_id));
        app.active_thread_id = Some(active_thread_id);
        app.primary_thread_id = Some(primary_thread_id);
        app.thread_event_channels.insert(
            channel_thread_id,
            ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
        );
        let replacement_chat_widget = replacement_chat_widget(&app, app.config.clone());

        let snapshot = AppServerThreadUiSnapshot::take_from(&mut app, replacement_chat_widget);
        let mut thread_ids = snapshot.previous_thread_ids();
        thread_ids.sort_by_key(ToString::to_string);
        let mut expected_thread_ids = vec![
            channel_thread_id,
            chat_widget_thread_id,
            active_thread_id,
            primary_thread_id,
        ];
        expected_thread_ids.sort_by_key(ToString::to_string);

        assert_eq!(thread_ids, expected_thread_ids);
    }

    #[tokio::test]
    async fn app_server_thread_replacement_discards_previous_ui_after_success() -> Result<()> {
        let mut app = make_test_app().await;
        let original_thread_id = ThreadId::new();
        let replacement_thread_id = ThreadId::new();
        app.primary_thread_id = Some(original_thread_id);
        app.active_thread_id = Some(original_thread_id);
        app.thread_event_channels.insert(
            original_thread_id,
            ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
        );
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        app.thread_event_listener_tasks
            .insert(original_thread_id, drop_notify_task(dropped_tx, started_tx));
        wait_for_task_start(started_rx).await;
        let replacement_chat_widget = replacement_chat_widget(&app, app.config.clone());
        let previous_ui = Some(AppServerThreadUiSnapshot::take_from(
            &mut app,
            replacement_chat_widget,
        ));
        app.enqueue_primary_thread_session(
            test_thread_session(&app, replacement_thread_id),
            Vec::new(),
        )
        .await?;
        let mut app_server =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
        let mut previous_app_server =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

        app.finish_app_server_thread_replacement(
            &mut app_server,
            previous_ui,
            Some(&mut previous_app_server),
            Ok(()),
        )
        .await?;

        assert_eq!(app.primary_thread_id, Some(replacement_thread_id));
        assert_eq!(app.active_thread_id, Some(replacement_thread_id));
        assert!(
            app.thread_event_channels
                .contains_key(&replacement_thread_id)
        );
        assert!(!app.thread_event_channels.contains_key(&original_thread_id));
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("previous listener task should be aborted after attach succeeds")
            .expect("previous listener task should notify on drop");
        app_server.shutdown().await?;
        previous_app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn app_server_thread_replacement_restores_previous_ui_on_attach_error() -> Result<()> {
        let mut app = make_test_app().await;
        let original_thread_id = ThreadId::new();
        let replacement_thread_id = ThreadId::new();
        app.primary_thread_id = Some(original_thread_id);
        app.active_thread_id = Some(original_thread_id);
        app.thread_event_channels.insert(
            original_thread_id,
            ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
        );
        let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        app.thread_event_listener_tasks
            .insert(original_thread_id, drop_notify_task(dropped_tx, started_tx));
        wait_for_task_start(started_rx).await;
        let replacement_chat_widget = replacement_chat_widget(&app, app.config.clone());
        let previous_ui = Some(AppServerThreadUiSnapshot::take_from(
            &mut app,
            replacement_chat_widget,
        ));
        app.primary_thread_id = Some(replacement_thread_id);
        app.active_thread_id = Some(replacement_thread_id);
        app.thread_event_channels.insert(
            replacement_thread_id,
            ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
        );
        let mut app_server =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
        let err = color_eyre::eyre::eyre!("forced replacement attach error");

        let result = app
            .finish_app_server_thread_replacement(&mut app_server, previous_ui, None, Err(err))
            .await;

        assert!(result.is_err());
        assert_eq!(app.primary_thread_id, Some(original_thread_id));
        assert_eq!(app.active_thread_id, Some(original_thread_id));
        assert!(app.thread_event_channels.contains_key(&original_thread_id));
        assert!(
            !app.thread_event_channels
                .contains_key(&replacement_thread_id)
        );
        assert!(matches!(
            dropped_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        app.abort_all_thread_event_listeners();
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("restored listener task should be abortable after cleanup")
            .expect("restored listener task should notify on drop");
        app_server.shutdown().await?;
        Ok(())
    }

    fn replacement_chat_widget(app: &App, config: Config) -> ChatWidget {
        ChatWidget::new_with_app_event(ChatWidgetInit {
            config,
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            app_event_tx: app.app_event_tx.clone(),
            workspace_command_runner: None,
            initial_user_message: None,
            enhanced_keys_supported: false,
            has_chatgpt_account: false,
            model_catalog: app.model_catalog.clone(),
            feedback: codex_feedback::CodexFeedback::new(),
            is_first_run: false,
            status_account_display: None,
            runtime_model_provider_base_url: None,
            initial_plan_type: None,
            model: None,
            startup_tooltip_override: None,
            status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
            session_telemetry: app.session_telemetry.clone(),
        })
    }

    fn test_thread_session(app: &App, thread_id: ThreadId) -> ThreadSessionState {
        ThreadSessionState {
            thread_id,
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: app.chat_widget.current_model().to_string(),
            model_provider_id: app.config.model_provider_id.clone(),
            service_tier: app.chat_widget.current_service_tier().map(str::to_string),
            approval_policy: AskForApproval::from(app.config.permissions.approval_policy.value()),
            approvals_reviewer: app.config.approvals_reviewer,
            permission_profile: app.config.permissions.permission_profile().clone(),
            active_permission_profile: app.config.permissions.active_permission_profile(),
            cwd: app.config.cwd.clone(),
            runtime_workspace_roots: app.config.workspace_roots.clone(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: app.chat_widget.current_reasoning_effort(),
            collaboration_mode: None,
            personality: None,
            message_history: None,
            network_proxy: None,
            rollout_path: None,
        }
    }

    struct DropNotify(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropNotify {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    fn drop_notify_task(
        dropped_tx: tokio::sync::oneshot::Sender<()>,
        started_tx: tokio::sync::oneshot::Sender<()>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let _notify = DropNotify(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        })
    }

    async fn wait_for_task_start(started_rx: tokio::sync::oneshot::Receiver<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx)
            .await
            .expect("listener task should start")
            .expect("listener task should notify after starting");
    }
}
