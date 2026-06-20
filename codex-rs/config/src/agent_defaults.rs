//! Built-in external agent selector defaults.
//!
//! This catalog mirrors Every Code's canonical selector names, aliases, and
//! launch argument defaults. Execution wiring is intentionally separate: this
//! module only defines the stable selector metadata that config, tools, and UI
//! surfaces can share.

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;

const CLAUDE_ALLOWED_TOOLS: &str = "Bash(ls:*), Bash(cat:*), Bash(grep:*), Bash(git status:*), Bash(git log:*), Bash(find:*), Read, Grep, Glob, LS, WebFetch, TodoRead, TodoWrite, WebSearch";
const CLOUD_MODEL_ENV_FLAG: &str = "CODE_ENABLE_CLOUD_AGENT_MODEL";

const CODE_GPT5_CODEX_READ_ONLY: &[&str] = &["-s", "read-only", "exec", "--skip-git-repo-check"];
const CODE_GPT5_CODEX_WRITE: &[&str] = &[
    "-s",
    "workspace-write",
    "--dangerously-bypass-approvals-and-sandbox",
    "exec",
    "--skip-git-repo-check",
];
const CODE_GPT5_READ_ONLY: &[&str] = &["-s", "read-only", "exec", "--skip-git-repo-check"];
const CODE_GPT5_WRITE: &[&str] = &[
    "-s",
    "workspace-write",
    "--dangerously-bypass-approvals-and-sandbox",
    "exec",
    "--skip-git-repo-check",
];
const CLAUDE_SONNET_READ_ONLY: &[&str] = &["--allowedTools", CLAUDE_ALLOWED_TOOLS];
const CLAUDE_SONNET_WRITE: &[&str] = &["--dangerously-skip-permissions"];
const CLAUDE_OPUS_READ_ONLY: &[&str] = &["--allowedTools", CLAUDE_ALLOWED_TOOLS];
const CLAUDE_OPUS_WRITE: &[&str] = &["--dangerously-skip-permissions"];
const CLAUDE_HAIKU_READ_ONLY: &[&str] = &["--allowedTools", CLAUDE_ALLOWED_TOOLS];
const CLAUDE_HAIKU_WRITE: &[&str] = &["--dangerously-skip-permissions"];
const ANTIGRAVITY_READ_ONLY: &[&str] = &[];
const ANTIGRAVITY_WRITE: &[&str] = &["--dangerously-skip-permissions"];
const COPILOT_READ_ONLY: &[&str] = &["--autopilot", "--allow-all-tools", "--no-ask-user", "-s"];
const COPILOT_WRITE: &[&str] = &["--autopilot", "--yolo", "--no-ask-user", "-s"];
const QWEN_3_CODER_READ_ONLY: &[&str] = &[];
const QWEN_3_CODER_WRITE: &[&str] = &["-y"];
const CLOUD_GPT5_CODEX_READ_ONLY: &[&str] = &[];
const CLOUD_GPT5_CODEX_WRITE: &[&str] = &[];

/// Canonical built-in external agent selectors used when no custom selector
/// list is configured. Ordering controls default presentation priority.
pub const DEFAULT_AGENT_NAMES: &[&str] = &[
    "code-gpt-5.5",
    "code-gpt-5.4",
    "claude-opus-4.8",
    "antigravity",
    "code-gpt-5.4-mini",
    "claude-sonnet-4.6",
    "github-copilot",
    "claude-haiku-4.5",
    "qwen3-coder-plus",
    "cloud-gpt-5.1-codex-max",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentModelSpec {
    pub slug: &'static str,
    pub family: &'static str,
    pub cli: &'static str,
    pub read_only_args: &'static [&'static str],
    pub write_args: &'static [&'static str],
    pub model_args: &'static [&'static str],
    pub description: &'static str,
    pub enabled_by_default: bool,
    pub aliases: &'static [&'static str],
    pub gating_env: Option<&'static str>,
    pub is_frontline: bool,
    pub pro_only: bool,
}

impl AgentModelSpec {
    pub fn is_enabled(&self) -> bool {
        if self.enabled_by_default {
            return true;
        }
        if let Some(env) = self.gating_env
            && let Ok(value) = std::env::var(env)
        {
            return matches!(value.as_str(), "1" | "true" | "TRUE" | "True");
        }
        false
    }

    pub fn default_args(&self, read_only: bool) -> &'static [&'static str] {
        if read_only {
            self.read_only_args
        } else {
            self.write_args
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfigDefaults {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub read_only: bool,
    pub enabled: bool,
    pub description: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub args_read_only: Option<Vec<String>>,
    pub args_write: Option<Vec<String>>,
    pub instructions: Option<String>,
}

const AGENT_MODEL_SPECS: &[AgentModelSpec] = &[
    AgentModelSpec {
        slug: "code-gpt-5.5",
        family: "code",
        cli: "coder",
        read_only_args: CODE_GPT5_READ_ONLY,
        write_args: CODE_GPT5_WRITE,
        model_args: &["--model", "gpt-5.5"],
        description: "Default frontier model for complex coding, research, and real-world work.",
        enabled_by_default: true,
        aliases: &[
            "gpt-5.5",
            "code-gpt-5.1-codex-max",
            "code-gpt-5.1-codex",
            "code-gpt-5-codex",
            "gpt-5.1-codex-max",
            "gpt-5.1-codex",
            "gpt-5-codex",
            "coder",
            "code",
            "codex",
        ],
        gating_env: None,
        is_frontline: true,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "code-gpt-5.4",
        family: "code",
        cli: "coder",
        read_only_args: CODE_GPT5_READ_ONLY,
        write_args: CODE_GPT5_WRITE,
        model_args: &["--model", "gpt-5.4"],
        description: "Highest-capacity GPT option for tricky reasoning and large-context work. In Every Code, GPT-5.4 defaults to the expensive 1m context path, so use when correctness or history preservation is worth the added cost.",
        enabled_by_default: true,
        aliases: &[
            "gpt-5.4",
            "code-gpt-5.1",
            "code-gpt-5",
            "gpt-5.1",
            "gpt-5",
            "coder-gpt-5",
        ],
        gating_env: None,
        is_frontline: true,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "code-gpt-5.4-mini",
        family: "code",
        cli: "coder",
        read_only_args: CODE_GPT5_CODEX_READ_ONLY,
        write_args: CODE_GPT5_CODEX_WRITE,
        model_args: &["--model", "gpt-5.4-mini"],
        description: "Budget coding agent for small changes and quick refactors; use when speed and cost matter.",
        enabled_by_default: true,
        aliases: &[
            "gpt-5.4-mini",
            "code-gpt-5.1-codex-mini",
            "code-gpt-5-codex-mini",
            "gpt-5.1-codex-mini",
            "gpt-5-codex-mini",
            "codex-mini",
            "coder-mini",
        ],
        gating_env: None,
        is_frontline: false,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "claude-opus-4.8",
        family: "claude",
        cli: "claude",
        read_only_args: CLAUDE_OPUS_READ_ONLY,
        write_args: CLAUDE_OPUS_WRITE,
        model_args: &["--model", "claude-opus-4-8"],
        description: "Higher-capacity Claude model for complex reasoning; use when you want the strongest Claude.",
        enabled_by_default: true,
        aliases: &[
            "claude-opus",
            "claude-opus-4.1",
            "claude-opus-4.5",
            "claude-opus-4.6",
        ],
        gating_env: None,
        is_frontline: true,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "claude-sonnet-4.6",
        family: "claude",
        cli: "claude",
        read_only_args: CLAUDE_SONNET_READ_ONLY,
        write_args: CLAUDE_SONNET_WRITE,
        model_args: &["--model", "claude-sonnet-4-6"],
        description: "Balanced Claude model for implementation and debugging; a solid default when you want Claude.",
        enabled_by_default: true,
        aliases: &["claude", "claude-sonnet", "claude-sonnet-4.5"],
        gating_env: None,
        is_frontline: false,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "claude-haiku-4.5",
        family: "claude",
        cli: "claude",
        read_only_args: CLAUDE_HAIKU_READ_ONLY,
        write_args: CLAUDE_HAIKU_WRITE,
        model_args: &["--model", "claude-haiku-4-5"],
        description: "Fast Claude model for simple tasks, drafts, and quick iterations; pick when latency matters.",
        enabled_by_default: true,
        aliases: &["claude-haiku"],
        gating_env: None,
        is_frontline: false,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "antigravity",
        family: "antigravity",
        cli: "agy",
        read_only_args: ANTIGRAVITY_READ_ONLY,
        write_args: ANTIGRAVITY_WRITE,
        model_args: &[],
        description: "Google/Gemini-family agent via Antigravity CLI; use for Google perspective after consumer Gemini CLI retirement. AGY uses its configured model, not per-run Gemini Pro/Flash flags.",
        enabled_by_default: true,
        aliases: &[
            "agy",
            "google",
            "gemini",
            "gemini-agent",
            "gemini-perspective",
            "google-antigravity",
        ],
        gating_env: None,
        is_frontline: true,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "github-copilot",
        family: "copilot",
        cli: "copilot",
        read_only_args: COPILOT_READ_ONLY,
        write_args: COPILOT_WRITE,
        model_args: &[],
        description: "GitHub Copilot CLI agent; uses your signed-in Copilot account and configured default model.",
        enabled_by_default: true,
        aliases: &["copilot", "github-copilot-cli"],
        gating_env: None,
        is_frontline: false,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "qwen3-coder-plus",
        family: "qwen",
        cli: "qwen",
        read_only_args: QWEN_3_CODER_READ_ONLY,
        write_args: QWEN_3_CODER_WRITE,
        model_args: &["-m", "qwen3-coder-plus"],
        description: "Fast and capable alternative; useful as a second opinion or for cross-checking.",
        enabled_by_default: true,
        aliases: &["qwen", "qwen3", "qwen-3-coder"],
        gating_env: None,
        is_frontline: false,
        pro_only: false,
    },
    AgentModelSpec {
        slug: "cloud-gpt-5.1-codex-max",
        family: "cloud",
        cli: "cloud",
        read_only_args: CLOUD_GPT5_CODEX_READ_ONLY,
        write_args: CLOUD_GPT5_CODEX_WRITE,
        model_args: &["--model", "gpt-5.1-codex-max"],
        description: "Cloud-hosted gpt-5.1-codex-max agent; use for remote runs when enabled via CODE_ENABLE_CLOUD_AGENT_MODEL.",
        enabled_by_default: false,
        aliases: &["cloud-gpt-5.1-codex", "cloud-gpt-5-codex", "cloud"],
        gating_env: Some(CLOUD_MODEL_ENV_FLAG),
        is_frontline: false,
        pro_only: false,
    },
];

static ALL_AGENT_MODEL_SPECS: LazyLock<Vec<AgentModelSpec>> =
    LazyLock::new(|| AGENT_MODEL_SPECS.to_vec());

pub fn agent_model_specs() -> &'static [AgentModelSpec] {
    ALL_AGENT_MODEL_SPECS.as_slice()
}

pub fn enabled_agent_model_specs() -> Vec<&'static AgentModelSpec> {
    agent_model_specs()
        .iter()
        .filter(|spec| spec.is_enabled())
        .collect()
}

pub fn agent_model_spec(identifier: &str) -> Option<&'static AgentModelSpec> {
    let lower = identifier.to_ascii_lowercase();
    agent_model_specs()
        .iter()
        .find(|spec| spec.slug.eq_ignore_ascii_case(&lower))
        .or_else(|| {
            agent_model_specs().iter().find(|spec| {
                spec.aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(&lower))
            })
        })
}

pub fn default_agent_configs() -> Vec<AgentConfigDefaults> {
    enabled_agent_model_specs()
        .into_iter()
        .map(agent_config_from_spec)
        .collect()
}

pub fn agent_config_from_spec(spec: &AgentModelSpec) -> AgentConfigDefaults {
    AgentConfigDefaults {
        name: spec.slug.to_string(),
        command: spec.cli.to_string(),
        args: spec
            .model_args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect(),
        read_only: false,
        enabled: spec.is_enabled(),
        description: None,
        env: None,
        args_read_only: some_args(spec.read_only_args),
        args_write: some_args(spec.write_args),
        instructions: None,
    }
}

fn some_args(args: &[&str]) -> Option<Vec<String>> {
    if args.is_empty() {
        None
    } else {
        Some(args.iter().map(|arg| (*arg).to_string()).collect())
    }
}

pub fn default_params_for(name: &str, read_only: bool) -> Vec<String> {
    agent_model_spec(name)
        .map(|spec| {
            spec.model_args
                .iter()
                .chain(spec.default_args(read_only).iter())
                .map(|arg| (*arg).to_string())
                .collect()
        })
        .unwrap_or_default()
}

pub fn build_model_guide_description(active_agents: &[String]) -> String {
    let mut description = model_guide_intro(active_agents);
    let canonical = active_agent_keys(active_agents);
    let lines: Vec<String> = agent_model_specs()
        .iter()
        .filter(|spec| canonical.contains(&spec.slug.to_ascii_lowercase()))
        .map(model_guide_line)
        .collect();

    if lines.is_empty() {
        description.push('\n');
        description.push_str("- No model guides available for the current configuration.");
    } else {
        for line in lines {
            description.push('\n');
            description.push_str(&line);
        }
    }

    description
}

pub fn model_guide_markdown() -> String {
    agent_model_specs()
        .iter()
        .filter(|spec| spec.is_enabled())
        .map(model_guide_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn model_guide_markdown_with_custom(
    configured_agents: &[AgentConfigDefaults],
) -> Option<String> {
    let mut lines = Vec::new();
    let mut positions = HashMap::new();

    for spec in agent_model_specs().iter().filter(|spec| spec.is_enabled()) {
        let idx = lines.len();
        positions.insert(spec.slug.to_ascii_lowercase(), idx);
        lines.push(model_guide_line(spec));
    }

    let mut saw_custom = false;
    for agent in configured_agents {
        if !agent.enabled {
            continue;
        }
        let Some(description) = agent.description.as_deref() else {
            continue;
        };
        let trimmed = description.trim();
        if trimmed.is_empty() {
            continue;
        }
        let slug = agent.name.trim();
        if slug.is_empty() {
            continue;
        }

        saw_custom = true;
        let line = custom_model_guide_line(slug, trimmed);
        let key = agent_model_spec(slug)
            .map(|spec| spec.slug.to_ascii_lowercase())
            .unwrap_or_else(|| slug.to_ascii_lowercase());
        if let Some(idx) = positions.get(&key).copied() {
            lines[idx] = line;
        } else {
            positions.insert(key, lines.len());
            lines.push(line);
        }
    }

    saw_custom.then(|| lines.join("\n"))
}

fn model_guide_intro(active_agents: &[String]) -> String {
    let mut present_frontline: Vec<String> = active_agents
        .iter()
        .filter_map(|id| {
            agent_model_spec(id)
                .filter(|spec| spec.is_frontline)
                .map(|spec| spec.slug.to_string())
        })
        .collect();

    if present_frontline.is_empty() {
        present_frontline.push("code-gpt-5.4".to_string());
    }
    let frontline_str = present_frontline.join(", ");

    format!(
        "Preferred agent models: use {frontline_str} for challenging coding/agentic work. For explicit multi-agent or dissent requests, prefer diverse model families when useful and budget allows. For multi-agent release/workflow, infrastructure, security, or product-risk work, proactively include `antigravity` for Google/Gemini-family perspective unless there is a clear reason to skip it."
    )
}

fn active_agent_keys(active_agents: &[String]) -> HashSet<String> {
    active_agents
        .iter()
        .filter_map(|name| {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(
                    agent_model_spec(trimmed)
                        .map(|spec| spec.slug)
                        .unwrap_or(trimmed)
                        .to_ascii_lowercase(),
                )
            }
        })
        .collect()
}

fn model_guide_line(spec: &AgentModelSpec) -> String {
    format!("- `{}`: {}", spec.slug, spec.description)
}

fn custom_model_guide_line(name: &str, description: &str) -> String {
    format!("- `{name}`: {description}")
}

#[cfg(test)]
#[path = "agent_defaults_tests.rs"]
mod tests;
