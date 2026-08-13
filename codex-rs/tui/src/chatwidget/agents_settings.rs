use super::*;
use codex_app_server_protocol::ConfigLayerSource;
use codex_app_server_protocol::ExternalAgentAuthState;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshStatus;
use codex_app_server_protocol::ExternalAgentCapabilityFailureKind;
use codex_app_server_protocol::ExternalAgentCapabilityFreshness;
use codex_app_server_protocol::ExternalAgentInstallState;
use codex_app_server_protocol::ExternalAgentProviderCapabilities;
use codex_app_server_protocol::ExternalAgentSelectorState;
use std::collections::HashSet;

impl ChatWidget {
    pub(crate) fn open_agents_settings_popup(&mut self) {
        self.open_agents_capabilities_popup(&[], true, false, false, None);
    }

    pub(crate) fn set_agent_selector_enabled(&mut self, selector: &str, enabled: bool) {
        self.config
            .agent_selector_overrides
            .entry(selector.to_string())
            .or_default()
            .enabled = Some(enabled);
    }

    pub(crate) fn open_agents_capabilities_popup(
        &mut self,
        providers: &[ExternalAgentProviderCapabilities],
        loading: bool,
        refreshing: bool,
        cancelling: bool,
        refresh_status: Option<ExternalAgentCapabilitiesRefreshStatus>,
    ) {
        let params =
            agents_settings_params(providers, loading, refreshing, cancelling, refresh_status);
        if !self
            .bottom_pane
            .replace_selection_view_if_present("agents-settings", params)
        {
            self.bottom_pane.show_selection_view(agents_settings_params(
                providers,
                loading,
                refreshing,
                cancelling,
                refresh_status,
            ));
        }
    }

    pub(crate) fn open_agent_selector_model_picker(
        &mut self,
        selector: String,
        models: Vec<String>,
        current: Option<String>,
    ) {
        let mut items = vec![selector_setting_item(
            "Provider default",
            "Let the provider choose its model.",
            current.is_none(),
            Box::new({
                let selector = selector.clone();
                move |tx| {
                    tx.send(AppEvent::SetAgentSelectorModel {
                        selector: selector.clone(),
                        model: None,
                    });
                }
            }),
        )];
        items.extend(models.into_iter().map(|model| {
            let is_current = current.as_deref() == Some(&model);
            selector_setting_item(
                &model,
                "Use this provider model for the bare selector.",
                is_current,
                Box::new({
                    let selector = selector.clone();
                    let model = model.clone();
                    move |tx| {
                        tx.send(AppEvent::SetAgentSelectorModel {
                            selector: selector.clone(),
                            model: Some(model.clone()),
                        });
                    }
                }),
            )
        }));
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("Model · {selector}")),
            subtitle: Some("Choose or clear the provider model default.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            on_cancel: Some(Box::new(|tx| tx.send(AppEvent::OpenAgentsSettings))),
            ..Default::default()
        });
    }

    pub(crate) fn open_agent_selector_effort_picker(
        &mut self,
        selector: String,
        efforts: Vec<String>,
        current: Option<String>,
    ) {
        let mut items = vec![selector_setting_item(
            "Provider default",
            "Let the provider choose its reasoning effort.",
            current.is_none(),
            Box::new({
                let selector = selector.clone();
                move |tx| {
                    tx.send(AppEvent::SetAgentSelectorEffort {
                        selector: selector.clone(),
                        effort: None,
                    });
                }
            }),
        )];
        items.extend(efforts.into_iter().map(|effort| {
            let is_current = current.as_deref() == Some(&effort);
            selector_setting_item(
                &effort,
                "Use this reasoning effort when the spawn call does not choose one.",
                is_current,
                Box::new({
                    let selector = selector.clone();
                    let effort = effort.clone();
                    move |tx| {
                        tx.send(AppEvent::SetAgentSelectorEffort {
                            selector: selector.clone(),
                            effort: Some(effort.clone()),
                        });
                    }
                }),
            )
        }));
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some(format!("Effort · {selector}")),
            subtitle: Some("Choose or clear the default reasoning effort.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            on_cancel: Some(Box::new(|tx| tx.send(AppEvent::OpenAgentsSettings))),
            ..Default::default()
        });
    }

    pub(crate) fn set_agent_selector_model(&mut self, selector: &str, model: Option<String>) {
        self.config
            .agent_selector_overrides
            .entry(selector.to_string())
            .or_default()
            .model = model;
    }

    pub(crate) fn set_agent_selector_effort(&mut self, selector: &str, effort: Option<String>) {
        self.config
            .agent_selector_overrides
            .entry(selector.to_string())
            .or_default()
            .effort = effort;
    }
}

fn agents_settings_params(
    providers: &[ExternalAgentProviderCapabilities],
    loading: bool,
    refreshing: bool,
    cancelling: bool,
    refresh_status: Option<ExternalAgentCapabilitiesRefreshStatus>,
) -> SelectionViewParams {
    let mut items = vec![refresh_item(loading, refreshing, cancelling)];
    if providers.is_empty() && !loading {
        items.push(empty_provider_item());
    }
    for provider in providers {
        items.push(provider_header(provider));
        items.extend(provider_items(provider));
    }
    SelectionViewParams {
        view_id: Some("agents-settings"),
        title: Some("Agents".to_string()),
        subtitle: Some(
            settings_subtitle(loading, refreshing, cancelling, refresh_status).to_string(),
        ),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        on_cancel: Some(Box::new(|tx| tx.send(AppEvent::CloseAgentsSettings))),
        ..Default::default()
    }
}

fn empty_provider_item() -> SelectionItem {
    SelectionItem {
        name: "No provider capabilities available".to_string(),
        description: Some("Refresh to probe installed external-agent CLIs.".to_string()),
        is_disabled: true,
        disabled_gutter_marker: Some("•"),
        ..Default::default()
    }
}

fn settings_subtitle(
    loading: bool,
    refreshing: bool,
    cancelling: bool,
    refresh_status: Option<ExternalAgentCapabilitiesRefreshStatus>,
) -> &'static str {
    if loading {
        return "Loading cached capabilities…";
    }
    if cancelling {
        return "Cancelling capability refresh…";
    }
    if refreshing {
        return "Refreshing local capabilities in the background…";
    }
    match refresh_status {
        Some(ExternalAgentCapabilitiesRefreshStatus::Completed) => "Capabilities refreshed.",
        Some(ExternalAgentCapabilitiesRefreshStatus::Cancelled) => {
            "Refresh cancelled; showing cached capabilities."
        }
        Some(ExternalAgentCapabilitiesRefreshStatus::DeadlineExceeded) => {
            "Refresh timed out; showing partial cached capabilities."
        }
        None => "Configure the external selectors available to spawned agents.",
    }
}

fn refresh_item(loading: bool, refreshing: bool, cancelling: bool) -> SelectionItem {
    if loading {
        return SelectionItem {
            name: "Reading cached provider state…".to_string(),
            description: Some(
                "Opening Agents settings never waits for local CLI probes.".to_string(),
            ),
            is_disabled: true,
            disabled_gutter_marker: Some("•"),
            ..Default::default()
        };
    }
    if cancelling {
        return SelectionItem {
            name: "Cancelling capability refresh…".to_string(),
            description: Some("Waiting for the terminal provider snapshot.".to_string()),
            is_disabled: true,
            disabled_gutter_marker: Some("•"),
            ..Default::default()
        };
    }
    if refreshing {
        SelectionItem {
            name: "Cancel capability refresh".to_string(),
            description: Some("Stop the bounded local provider probes.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::CancelAgentCapabilitiesRefresh);
            })],
            dismiss_on_select: false,
            ..Default::default()
        }
    } else {
        SelectionItem {
            name: "Refresh capabilities".to_string(),
            description: Some(
                "Re-check install, sign-in, version, models, and effort support.".to_string(),
            ),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::RefreshAgentCapabilities);
            })],
            dismiss_on_select: false,
            ..Default::default()
        }
    }
}

fn provider_header(provider: &ExternalAgentProviderCapabilities) -> SelectionItem {
    let version = provider.cli_version.as_deref().unwrap_or("version unknown");
    let freshness = match provider.freshness {
        ExternalAgentCapabilityFreshness::Fresh => "fresh",
        ExternalAgentCapabilityFreshness::Cached => "cached",
        ExternalAgentCapabilityFreshness::Unknown => "not checked",
    };
    let mut description = format!(
        "{} • {} • {version} • {freshness}",
        install_label(provider.install),
        auth_label(provider.auth),
    );
    if let Some(status) = provider
        .failure
        .as_ref()
        .and_then(|failure| failure_status_label(failure.kind))
    {
        description.push_str(" • ");
        description.push_str(status);
    }
    SelectionItem {
        name: provider_label(&provider.family).to_string(),
        description: Some(description),
        selected_description: provider
            .failure
            .as_ref()
            .map(|failure| failure.message.clone()),
        is_disabled: true,
        disabled_gutter_marker: Some("•"),
        search_value: Some(format!(
            "{} {} {}",
            provider.family, provider.command, version
        )),
        ..Default::default()
    }
}

fn provider_items(provider: &ExternalAgentProviderCapabilities) -> Vec<SelectionItem> {
    let mut items = Vec::new();
    let discovered_models = provider
        .selectors
        .iter()
        .filter_map(|selector| selector.model.clone())
        .collect::<HashSet<_>>();
    let mut discovered_models = discovered_models.into_iter().collect::<Vec<_>>();
    discovered_models.sort();
    for selector in &provider.selectors {
        items.push(selector_toggle(provider, selector));
        if selector.selector == "antigravity"
            && (!discovered_models.is_empty() || selector.default_model.is_some())
        {
            items.push(model_default_item(selector, &discovered_models));
        }
        if (selector.selector == "antigravity" || selector.selector.starts_with("antigravity-"))
            && (!provider.effort_levels.is_empty() || selector.default_effort.is_some())
        {
            items.push(effort_default_item(selector, &provider.effort_levels));
        }
    }
    items
}

fn selector_toggle(
    provider: &ExternalAgentProviderCapabilities,
    selector: &ExternalAgentSelectorState,
) -> SelectionItem {
    let enabled = selector.enabled;
    let selector_name = selector.selector.clone();
    let read_only = selector
        .enabled_origin
        .as_ref()
        .is_some_and(|origin| origin.read_only);
    let kind = if selector.discovered {
        "discovered model"
    } else if selector.model.is_none() {
        "provider default"
    } else {
        "catalog model"
    };
    let mut details = vec![kind.to_string(), format!("provider `{}`", provider.family)];
    if selector.explicit_only {
        details.push("explicit only".to_string());
    }
    SelectionItem {
        name: format!(
            "{} ({})",
            selector.selector,
            if enabled { "enabled" } else { "disabled" }
        ),
        description: Some(details.join(" • ")),
        selected_description: selector
            .enabled_origin
            .as_ref()
            .map(origin_description)
            .or_else(|| {
                provider
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.clone())
            })
            .or_else(|| Some(format!("Command: `{}`", provider.command))),
        is_current: enabled,
        is_disabled: read_only,
        disabled_reason: read_only
            .then(|| "A higher-precedence setting controls this selector.".to_string()),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::SetAgentSelectorEnabled {
                selector: selector_name.clone(),
                enabled: !enabled,
            });
        })],
        dismiss_on_select: false,
        search_value: Some(format!("{} {}", selector.selector, provider.family)),
        ..Default::default()
    }
}

fn model_default_item(selector: &ExternalAgentSelectorState, models: &[String]) -> SelectionItem {
    let selector_name = selector.selector.clone();
    let current = selector.default_model.clone();
    let mut models = models.to_vec();
    if let Some(current) = current.as_ref()
        && !models.contains(current)
    {
        models.push(current.clone());
    }
    let read_only = selector
        .default_model_origin
        .as_ref()
        .is_some_and(|origin| origin.read_only);
    SelectionItem {
        name: format!(
            "  model default: {}",
            current.as_deref().unwrap_or("provider default")
        ),
        description: Some("Bare-selector provider model.".to_string()),
        selected_description: selector
            .default_model_origin
            .as_ref()
            .map(origin_description),
        is_disabled: read_only,
        disabled_reason: read_only
            .then(|| "A higher-precedence setting controls this model.".to_string()),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenAgentSelectorModelPicker {
                selector: selector_name.clone(),
                models: models.clone(),
                current: current.clone(),
            });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn effort_default_item(selector: &ExternalAgentSelectorState, efforts: &[String]) -> SelectionItem {
    let selector_name = selector.selector.clone();
    let current = selector.default_effort.clone();
    let mut efforts = efforts.to_vec();
    if let Some(current) = current.as_ref()
        && !efforts.contains(current)
    {
        efforts.push(current.clone());
    }
    let read_only = selector
        .default_effort_origin
        .as_ref()
        .is_some_and(|origin| origin.read_only);
    SelectionItem {
        name: format!(
            "  effort default: {}",
            current.as_deref().unwrap_or("provider default")
        ),
        description: Some("Used when a spawn call does not choose effort.".to_string()),
        selected_description: selector
            .default_effort_origin
            .as_ref()
            .map(origin_description),
        is_disabled: read_only,
        disabled_reason: read_only
            .then(|| "A higher-precedence setting controls this effort.".to_string()),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenAgentSelectorEffortPicker {
                selector: selector_name.clone(),
                efforts: efforts.clone(),
                current: current.clone(),
            });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn selector_setting_item(
    name: &str,
    description: &str,
    is_current: bool,
    action: SelectionAction,
) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        is_current,
        search_value: Some(name.to_string()),
        actions: vec![action],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn install_label(install: ExternalAgentInstallState) -> &'static str {
    match install {
        ExternalAgentInstallState::Installed => "installed",
        ExternalAgentInstallState::NotInstalled => "not installed",
        ExternalAgentInstallState::Unknown => "install unknown",
    }
}

fn auth_label(auth: ExternalAgentAuthState) -> &'static str {
    match auth {
        ExternalAgentAuthState::Authenticated => "signed in",
        ExternalAgentAuthState::NotAuthenticated => "sign-in required",
        ExternalAgentAuthState::NotProbed => "sign-in not checked",
        ExternalAgentAuthState::Unknown => "sign-in unknown",
    }
}

fn failure_status_label(kind: ExternalAgentCapabilityFailureKind) -> Option<&'static str> {
    match kind {
        ExternalAgentCapabilityFailureKind::TimedOut => Some("probe timed out"),
        ExternalAgentCapabilityFailureKind::MalformedOutput => Some("malformed output"),
        ExternalAgentCapabilityFailureKind::EmptyOutput => Some("empty output"),
        ExternalAgentCapabilityFailureKind::LaunchFailed => Some("launch failed"),
        ExternalAgentCapabilityFailureKind::ProviderFailed => Some("provider failed"),
        ExternalAgentCapabilityFailureKind::UnsupportedMode => Some("unsupported mode"),
        ExternalAgentCapabilityFailureKind::QuotaOrRateLimited => Some("rate limited"),
        ExternalAgentCapabilityFailureKind::PermissionDenied => Some("permission denied"),
        ExternalAgentCapabilityFailureKind::CommandMissing
        | ExternalAgentCapabilityFailureKind::AuthenticationRequired => None,
    }
}

fn provider_label(family: &str) -> &str {
    match family {
        "antigravity" => "Antigravity / Gemini",
        "claude" => "Claude",
        "copilot" => "GitHub Copilot",
        "qwen" => "Qwen",
        family => family,
    }
}

fn origin_description(origin: &codex_app_server_protocol::ExternalAgentSettingOrigin) -> String {
    format!(
        "Effective source: {} · {}{}",
        origin_source_label(&origin.layer.name),
        origin.config_path,
        if origin.read_only {
            " (read-only here)"
        } else {
            ""
        }
    )
}

fn origin_source_label(source: &ConfigLayerSource) -> &'static str {
    match source {
        ConfigLayerSource::User { .. } => "user config",
        ConfigLayerSource::Project { .. } => "project config",
        ConfigLayerSource::SessionFlags => "session override",
        ConfigLayerSource::Mdm { .. }
        | ConfigLayerSource::PackagedDefaults { .. }
        | ConfigLayerSource::System { .. }
        | ConfigLayerSource::EnterpriseManaged { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromMdm => "managed config",
    }
}
