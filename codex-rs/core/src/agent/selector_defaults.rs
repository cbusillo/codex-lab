use crate::agent::external_capabilities::looks_like_antigravity_selector;
use crate::config::Config;
use codex_config::config_toml::AgentSelectorToml;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SelectorDefaults {
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
}

pub(crate) fn selector_defaults(config: &Config, selector: &str) -> SelectorDefaults {
    let configured = selector_override(config, selector);
    SelectorDefaults {
        model: configured.and_then(|value| value.model.clone()),
        effort: configured.and_then(|value| value.effort.clone()),
    }
}

pub(crate) fn resolve_effective_selector(config: &Config, selector: &str) -> String {
    if selector != "antigravity" {
        return selector.to_string();
    }
    selector_defaults(config, selector).model.map_or_else(
        || selector.to_string(),
        |model| format!("antigravity-{model}"),
    )
}

pub(crate) fn resolve_effort(
    config: &Config,
    selector: &str,
    explicit_effort: Option<&str>,
) -> Option<String> {
    explicit_effort
        .map(str::to_string)
        .or_else(|| selector_defaults(config, selector).effort)
}

pub(crate) fn install_configured_provider_selectors(config: &mut Config) -> Result<(), String> {
    let configured_selectors = config
        .agent_selector_overrides
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for selector in configured_selectors {
        if !crate::agent::role::agent_selector_enabled(config, &selector) {
            continue;
        }
        let defaults = selector_defaults(config, &selector);
        let effective_selector = resolve_effective_selector(config, &selector);
        if (effective_selector == "antigravity"
            || looks_like_antigravity_selector(&effective_selector))
            && (defaults.model.is_some() || defaults.effort.is_some())
        {
            if let Some(rejection) =
                crate::agent::role::antigravity_selector_rejection(config, &effective_selector)
            {
                tracing::warn!("{rejection}");
                continue;
            }
            crate::agent::role::install_configured_antigravity_role(
                config,
                &effective_selector,
                defaults.model.as_deref(),
                defaults.effort.as_deref(),
            )?;
        } else if (defaults.model.is_some() || defaults.effort.is_some())
            && crate::agent::role::external_agent_backend_for_selector(config, &effective_selector)
                .is_some()
        {
            crate::agent::role::install_external_role_defaults(
                config,
                &effective_selector,
                defaults.model.as_deref(),
                defaults.effort.as_deref(),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn install_selected_provider_defaults(
    config: &mut Config,
    selector: &str,
    explicit_effort: Option<&str>,
) -> Result<bool, String> {
    let effort = resolve_effort(config, selector, explicit_effort);
    if selector == "antigravity" || looks_like_antigravity_selector(selector) {
        if let Some(rejection) =
            crate::agent::role::antigravity_selector_rejection(config, selector)
        {
            return Err(rejection);
        }
        crate::agent::role::install_dynamic_antigravity_role(config, selector, effort.as_deref())?;
        return Ok(true);
    }
    if crate::agent::role::external_agent_backend_for_selector(config, selector).is_none() {
        return Ok(false);
    }

    let defaults = selector_defaults(config, selector);
    if defaults.model.is_some() || effort.is_some() {
        crate::agent::role::install_external_role_defaults(
            config,
            selector,
            defaults.model.as_deref(),
            effort.as_deref(),
        )?;
    }
    Ok(true)
}

fn selector_override<'a>(config: &'a Config, selector: &str) -> Option<&'a AgentSelectorToml> {
    config
        .agent_selector_overrides
        .get(selector)
        .or_else(|| {
            codex_config::agent_defaults::agent_model_spec(selector)
                .and_then(|spec| config.agent_selector_overrides.get(spec.slug))
        })
        .or_else(|| {
            looks_like_antigravity_selector(selector)
                .then(|| config.agent_selector_overrides.get("antigravity"))
                .flatten()
        })
}

#[cfg(test)]
#[path = "selector_defaults_tests.rs"]
mod tests;
