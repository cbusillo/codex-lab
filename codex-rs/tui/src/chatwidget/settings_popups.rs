//! Settings-adjacent popup surfaces for `ChatWidget`.
//!
//! This keeps theme, personality, and experimental-feature UI out of the main
//! orchestration module without changing their event wiring.

use super::*;
use crate::agent_install_helpers::AgentInstallStatus;
use crate::agent_install_helpers::external_agent_install_statuses;
use codex_config::agent_defaults::AgentModelSpec;
use codex_config::agent_defaults::agent_model_specs;

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

    pub(crate) fn open_settings_popup(&mut self, automatic_validation_enabled: bool) {
        let validation_status = if automatic_validation_enabled {
            "Enabled"
        } else {
            "Disabled"
        };
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
                name: "Automatic Validation".to_string(),
                description: Some(format!(
                    "Current: {validation_status}. Configure trusted patch-time and end-of-turn checks."
                )),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenAutomaticValidationSettings);
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

    pub(crate) fn open_automatic_validation_settings_popup(&mut self, enabled: bool) {
        let project_command_note = self
            .config
            .validation
            .project_command
            .as_ref()
            .map(|_| " A configured project command is unchanged by this setting.")
            .unwrap_or_default();
        let items = [
            (
                true,
                "Enabled",
                format!(
                    "Check JSON, TOML, and YAML patches as they are applied, then run applicable bounded Cargo or ShellCheck validation after changed turns, with at most one correction cycle.{project_command_note}"
                ),
            ),
            (
                false,
                "Disabled",
                format!(
                    "Skip built-in patch-time structural checks and end-of-turn Cargo or ShellCheck validation.{project_command_note}"
                ),
            ),
        ]
        .into_iter()
        .map(|(value, name, description)| SelectionItem {
            name: name.to_string(),
            description: Some(description),
            is_current: enabled == value,
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::SetAutomaticValidationEnabled(value));
            })],
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect();

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Automatic Validation".to_string()),
            subtitle: Some(
                "Run trusted checks during patches and after changed turns.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_agents_settings_popup(&mut self) {
        let specs = agent_model_specs()
            .iter()
            .filter(|spec| matches!(spec.family, "antigravity" | "claude" | "copilot" | "qwen"))
            .collect::<Vec<_>>();
        let statuses = external_agent_install_statuses(&specs);
        self.open_agents_settings_popup_with_statuses(&specs, statuses);
    }

    pub(crate) fn open_agents_settings_popup_with_statuses(
        &mut self,
        specs: &[&'static AgentModelSpec],
        statuses: Vec<AgentInstallStatus>,
    ) {
        self.bottom_pane.show_selection_view(agents_settings_params(
            specs,
            statuses,
            &self.config.agent_selector_overrides,
        ));
    }

    pub(crate) fn set_agent_selector_enabled(&mut self, selector: &str, enabled: bool) {
        self.config
            .agent_selector_overrides
            .entry(selector.to_string())
            .or_default()
            .enabled = Some(enabled);
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

fn agents_settings_params(
    specs: &[&'static AgentModelSpec],
    statuses: Vec<AgentInstallStatus>,
    overrides: &std::collections::BTreeMap<String, codex_config::config_toml::AgentSelectorToml>,
) -> SelectionViewParams {
    let items = specs
        .iter()
        .map(|spec| {
            let status = statuses
                .iter()
                .find(|status| status.family == spec.family && status.command == spec.cli);
            let enabled = overrides
                .get(spec.slug)
                .and_then(|override_config| override_config.enabled)
                .unwrap_or_else(|| spec.is_enabled());
            agent_selector_selection_item(spec, status, enabled)
        })
        .collect();
    SelectionViewParams {
        title: Some("Agents".to_string()),
        subtitle: Some(
            "Enable only the external selectors you want spawn_agent to use.".to_string(),
        ),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        ..Default::default()
    }
}

fn agent_selector_selection_item(
    spec: &'static AgentModelSpec,
    status: Option<&AgentInstallStatus>,
    enabled: bool,
) -> SelectionItem {
    let enabled_marker = if enabled { "enabled" } else { "disabled" };
    let install_marker = if status.is_some_and(|status| status.installed) {
        "installed"
    } else {
        "not installed"
    };
    let selector = spec.slug.to_string();
    let next_enabled = !enabled;
    let selected_description = status.map_or_else(
        || Some(spec.description.to_string()),
        |status| {
            let availability = if status.installed {
                format!("`{}` is available on PATH.", status.command)
            } else {
                status.install_hint.clone()
            };
            Some(format!("{} {availability}", spec.description))
        },
    );

    SelectionItem {
        name: format!("{} ({enabled_marker})", spec.slug),
        description: Some(format!("{install_marker} • provider `{}`", spec.family)),
        selected_description,
        search_value: Some(format!(
            "{} {} {}",
            spec.slug, spec.family, spec.description
        )),
        is_current: enabled,
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::SetAgentSelectorEnabled {
                selector: selector.clone(),
                enabled: next_enabled,
            });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}
