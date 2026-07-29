//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme, personality, and experimental-feature UI out of the main
//! orchestration module without changing their event wiring.

use super::*;
use crate::agent_install_helpers::AgentInstallStatus;
use crate::agent_install_helpers::external_agent_install_statuses;
use codex_config::agent_defaults::enabled_agent_model_specs;

impl ChatWidget {
    pub(super) fn open_theme_picker(&mut self) {
        let codex_home = codex_utils_home_dir::find_codex_home().ok();
        let terminal_width = self
            .last_rendered_width
            .get()
            .and_then(|width| u16::try_from(width).ok());
        let params = crate::theme_picker::build_theme_picker_params(
            self.config.tui_theme.as_deref(),
            codex_home.as_deref(),
            terminal_width,
        );
        self.bottom_pane.show_selection_view(params);
    }

    pub(crate) fn open_personality_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Personality selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }
        if !self.current_model_supports_personality() {
            let current_model = self.current_model();
            self.add_error_message(format!(
                "Current model ({current_model}) doesn't support personalities. Try /model to pick a different model."
            ));
            return;
        }
        self.open_personality_popup_for_current_model();
    }

    fn open_personality_popup_for_current_model(&mut self) {
        let current_personality = self.config.personality.unwrap_or(Personality::Friendly);
        let personalities = [Personality::Friendly, Personality::Pragmatic];
        let supports_personality = self.current_model_supports_personality();

        let items: Vec<SelectionItem> = personalities
            .into_iter()
            .map(|personality| {
                let name = Self::personality_label(personality).to_string();
                let description = Some(Self::personality_description(personality).to_string());
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::CodexOp(AppCommand::override_turn_context(
                        /*cwd*/ None,
                        /*approval_policy*/ None,
                        /*approvals_reviewer*/ None,
                        /*permission_profile*/ None,
                        /*active_permission_profile*/ None,
                        /*windows_sandbox_level*/ None,
                        /*model*/ None,
                        /*effort*/ None,
                        /*summary*/ None,
                        /*service_tier*/ None,
                        /*collaboration_mode*/ None,
                        Some(personality),
                    )));
                    tx.send(AppEvent::UpdatePersonality(personality));
                    tx.send(AppEvent::PersistPersonalitySelection { personality });
                })];
                SelectionItem {
                    name,
                    description,
                    is_current: current_personality == personality,
                    is_disabled: !supports_personality,
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let mut header = ColumnRenderable::new();
        header.push(Line::from("Select Personality".bold()));
        header.push(Line::from("Choose a communication style for Codex.".dim()));

        self.bottom_pane.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_settings_popup(&mut self) {
        let items = vec![
            SelectionItem {
                name: "Manage accounts".to_string(),
                description: Some("Add, switch, or disconnect stored accounts.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::ShowLoginAccounts);
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Account switching".to_string(),
                description: Some("Configure automatic account switching.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenAccountSwitchSettings);
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Agents".to_string(),
                description: Some("Check third-party agent CLIs used by spawn_agent.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenAgentsSettings);
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        ];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Settings".to_string()),
            subtitle: Some("Configure settings for Codex.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_agents_settings_popup(&mut self) {
        let specs = enabled_agent_model_specs();
        let statuses = external_agent_install_statuses(&specs);
        self.open_agents_settings_popup_with_statuses(statuses);
    }

    pub(crate) fn open_agents_settings_popup_with_statuses(
        &mut self,
        statuses: Vec<AgentInstallStatus>,
    ) {
        self.bottom_pane
            .show_selection_view(agents_settings_params(statuses));
    }

    pub(crate) fn open_experimental_popup(&mut self) {
        let features: Vec<ExperimentalFeatureItem> = FEATURES
            .iter()
            .filter_map(|spec| {
                let name = spec.stage.experimental_menu_name()?;
                let description = spec.stage.experimental_menu_description()?;
                Some(ExperimentalFeatureItem {
                    feature: spec.id,
                    name: name.to_string(),
                    description: description.to_string(),
                    enabled: self.config.features.enabled(spec.id),
                })
            })
            .collect();

        let view = ExperimentalFeaturesView::new(
            features,
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    fn personality_label(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "None",
            Personality::Friendly => "Friendly",
            Personality::Pragmatic => "Pragmatic",
        }
    }

    fn personality_description(personality: Personality) -> &'static str {
        match personality {
            Personality::None => "No personality instructions.",
            Personality::Friendly => "Warm, collaborative, and helpful.",
            Personality::Pragmatic => "Concise, task-focused, and direct.",
        }
    }
}

fn agents_settings_params(statuses: Vec<AgentInstallStatus>) -> SelectionViewParams {
    let items = statuses
        .into_iter()
        .map(agent_status_selection_item)
        .collect();
    SelectionViewParams {
        title: Some("Agents".to_string()),
        subtitle: Some("Third-party agent CLI status for spawn_agent.".to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        ..Default::default()
    }
}

fn agent_status_selection_item(status: AgentInstallStatus) -> SelectionItem {
    let marker = if status.installed {
        "installed"
    } else {
        "not installed"
    };
    let description = if status.installed {
        format!("{marker} - `{}` is on PATH", status.command)
    } else {
        format!("{marker} - {}", status.install_hint)
    };
    let selected_description = if status.installed {
        Some(format!(
            "{} Command: `{}` is available on PATH.",
            status.description, status.command
        ))
    } else {
        Some(format!(
            "{} Command: `{}`. {}",
            status.description, status.command, status.install_hint
        ))
    };

    SelectionItem {
        name: format!("{} ({marker})", status.name),
        description: Some(description),
        selected_description,
        search_value: Some(format!(
            "{} {} {} {}",
            status.name, status.family, status.command, status.description
        )),
        dismiss_on_select: true,
        ..Default::default()
    }
}
