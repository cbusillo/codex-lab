use std::borrow::Cow;
#[cfg(not(windows))]
use std::env;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(not(windows))]
use std::path::Path;

use codex_config::agent_defaults::AgentModelSpec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentInstallStatus {
    pub(crate) name: String,
    pub(crate) family: String,
    pub(crate) command: String,
    pub(crate) description: String,
    pub(crate) installed: bool,
    pub(crate) install_hint: String,
}

pub(crate) fn external_agent_install_statuses(
    specs: &[&'static AgentModelSpec],
) -> Vec<AgentInstallStatus> {
    external_agent_install_statuses_with(specs, command_exists_on_path)
}

pub(crate) fn external_agent_install_statuses_with<F>(
    specs: &[&'static AgentModelSpec],
    mut command_exists: F,
) -> Vec<AgentInstallStatus>
where
    F: FnMut(&str) -> bool,
{
    let mut statuses: Vec<AgentInstallStatus> = Vec::new();
    for spec in specs
        .iter()
        .filter(|spec| is_external_agent_family(spec.family))
    {
        if let Some(status) = statuses
            .iter_mut()
            .find(|status| status.family == spec.family && status.command == spec.cli)
        {
            status.description.push_str(", ");
            status.description.push_str(spec.slug);
            continue;
        }

        statuses.push(AgentInstallStatus {
            name: agent_tool_name(spec.family, spec.cli).to_string(),
            family: spec.family.to_string(),
            command: spec.cli.to_string(),
            description: format!("Enabled selectors: {}.", spec.slug),
            installed: command_exists(spec.cli),
            install_hint: install_hint_for_command(spec.cli),
        });
    }

    statuses.sort_by(|a, b| a.family.cmp(&b.family).then_with(|| a.name.cmp(&b.name)));
    statuses
}

fn is_external_agent_family(family: &str) -> bool {
    matches!(family, "antigravity" | "claude" | "copilot" | "qwen")
}

fn agent_tool_name(family: &str, command: &str) -> &'static str {
    match family {
        "antigravity" => "Antigravity CLI",
        "claude" => "Claude Code",
        "copilot" => "GitHub Copilot CLI",
        "qwen" => "Qwen Code",
        _ => {
            if command.is_empty() {
                "Third-party agent CLI"
            } else {
                "External agent CLI"
            }
        }
    }
}

fn command_exists_on_path(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }

    #[cfg(windows)]
    {
        return which::which(command).is_ok();
    }

    #[cfg(not(windows))]
    {
        command_exists_on_unix_path(command)
    }
}

#[cfg(not(windows))]
fn command_exists_on_unix_path(command: &str) -> bool {
    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return is_executable_file(command_path);
    }

    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&path_var).any(|dir| is_executable_file(&dir.join(command)))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(all(not(unix), not(windows)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

pub(crate) fn install_hint_for_command(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return "Install the CLI and make sure it is on PATH.".to_string();
    }

    #[cfg(target_os = "macos")]
    {
        let formula = macos_brew_formula_for_command(trimmed);
        format!("Install {formula} and make sure `{trimmed}` is on PATH.")
    }

    #[cfg(not(target_os = "macos"))]
    {
        format!("Install `{trimmed}` and make sure it is on PATH.")
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn macos_brew_formula_for_command(command: &str) -> Cow<'_, str> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Cow::Borrowed(trimmed);
    }
    if trimmed.contains('/') || trimmed.contains(char::is_whitespace) {
        return Cow::Borrowed(trimmed);
    }
    if trimmed.eq_ignore_ascii_case("claude") {
        return Cow::Borrowed("claude-code");
    }
    if trimmed.eq_ignore_ascii_case("agy") || trimmed.eq_ignore_ascii_case("antigravity") {
        return Cow::Borrowed("Antigravity CLI");
    }
    if trimmed.eq_ignore_ascii_case("qwen") {
        return Cow::Borrowed("qwen-code");
    }
    Cow::Borrowed(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_formula_maps_known_agent_commands() {
        assert_eq!(macos_brew_formula_for_command("claude"), "claude-code");
        assert_eq!(macos_brew_formula_for_command("agy"), "Antigravity CLI");
        assert_eq!(macos_brew_formula_for_command("qwen"), "qwen-code");
        assert_eq!(macos_brew_formula_for_command("copilot"), "copilot");
        assert_eq!(
            macos_brew_formula_for_command("/opt/bin/claude"),
            "/opt/bin/claude"
        );
    }

    #[test]
    fn external_agent_statuses_use_enabled_third_party_specs() {
        let specs = codex_config::agent_defaults::enabled_agent_model_specs();
        let statuses = external_agent_install_statuses_with(&specs, |command| command == "claude");

        assert_eq!(
            statuses
                .iter()
                .filter(|status| status.command == "claude")
                .count(),
            1
        );
        assert!(statuses.iter().any(|status| {
            status.family == "claude" && status.command == "claude" && status.installed
        }));
        let claude = statuses
            .iter()
            .find(|status| status.family == "claude")
            .expect("claude status");
        assert_eq!(claude.name, "Claude Code");
        assert!(claude.description.contains("claude-opus-4.8"));
        assert!(claude.description.contains("claude-sonnet-4.6"));
        assert!(claude.description.contains("claude-haiku-4.5"));
        assert!(statuses.iter().any(|status| {
            status.family == "antigravity" && status.command == "agy" && !status.installed
        }));
        assert!(statuses.iter().any(|status| {
            status.family == "copilot" && status.command == "copilot" && !status.installed
        }));
        assert!(statuses.iter().any(|status| {
            status.family == "qwen" && status.command == "qwen" && !status.installed
        }));
        assert!(statuses.iter().all(|status| status.family != "code"));
    }
}
