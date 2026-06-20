use super::*;
use pretty_assertions::assert_eq;

#[test]
fn cloud_defaults_are_empty_both_modes() {
    assert_eq!(
        default_params_for("cloud", /*read_only*/ true),
        vec!["--model", "gpt-5.1-codex-max"]
    );
    assert_eq!(
        default_params_for("cloud", /*read_only*/ false),
        vec!["--model", "gpt-5.1-codex-max"]
    );
}

#[test]
fn default_params_include_model_selection_for_builtin_agents() {
    assert_eq!(
        default_params_for("claude-opus-4.8", /*read_only*/ true),
        vec![
            "--model",
            "claude-opus-4-8",
            "--allowedTools",
            CLAUDE_ALLOWED_TOOLS,
        ]
    );
    assert_eq!(
        default_params_for("claude-opus-4.8", /*read_only*/ false),
        vec![
            "--model",
            "claude-opus-4-8",
            "--dangerously-skip-permissions",
        ]
    );
    assert_eq!(
        default_params_for("qwen3-coder-plus", /*read_only*/ false),
        vec!["-m", "qwen3-coder-plus", "-y"]
    );
}

#[test]
fn github_copilot_defaults_match_cli_contract() {
    assert_eq!(
        default_params_for("github-copilot", /*read_only*/ true),
        vec!["--autopilot", "--allow-all-tools", "--no-ask-user", "-s"]
    );
    assert_eq!(
        default_params_for("github-copilot", /*read_only*/ false),
        vec!["--autopilot", "--yolo", "--no-ask-user", "-s"]
    );

    let spec = agent_model_spec("copilot").expect("copilot alias should resolve");
    assert_eq!(spec.slug, "github-copilot");
}

#[test]
fn gpt_codex_aliases_resolve() {
    let codex = agent_model_spec("gpt-5.1-codex").expect("alias for codex present");
    assert_eq!(codex.slug, "code-gpt-5.5");

    let codex_direct = agent_model_spec("gpt-5.1-codex-max").expect("codex present");
    assert_eq!(codex_direct.slug, "code-gpt-5.5");

    let mini = agent_model_spec("gpt-5.1-codex-mini").expect("mini alias present");
    assert_eq!(mini.slug, "code-gpt-5.4-mini");

    let mini_direct = agent_model_spec("gpt-5.4-mini").expect("mini direct alias present");
    assert_eq!(mini_direct.slug, "code-gpt-5.4-mini");

    let mid = agent_model_spec("gpt-5.1").expect("mid alias present");
    assert_eq!(mid.slug, "code-gpt-5.4");

    assert!(agent_model_spec("gpt-5.2-codex").is_none());
    assert!(agent_model_spec("code-gpt-5.2-codex").is_none());
    assert!(agent_model_spec("gpt-5.3-codex-spark").is_none());
    assert!(agent_model_spec("code-gpt-5.2").is_none());
}

#[test]
fn claude_opus_aliases_resolve_to_current_opus() {
    let opus = agent_model_spec("claude-opus").expect("opus alias present");
    assert_eq!(opus.slug, "claude-opus-4.8");
    assert_eq!(opus.model_args, &["--model", "claude-opus-4-8"]);

    let legacy = agent_model_spec("claude-opus-4.6").expect("legacy opus alias present");
    assert_eq!(legacy.slug, "claude-opus-4.8");
}

#[test]
fn google_intent_aliases_resolve_to_antigravity() {
    for alias in ["google", "gemini", "gemini-agent", "gemini-perspective"] {
        let spec = agent_model_spec(alias).expect("google-family alias should resolve");
        assert_eq!(spec.slug, "antigravity");
        assert!(spec.model_args.is_empty());
    }
}

#[test]
fn default_configs_use_canonical_selector_commands() {
    let configs = default_agent_configs();
    let antigravity = configs
        .iter()
        .find(|config| config.name == "antigravity")
        .expect("antigravity should be enabled by default");
    assert_eq!(antigravity.command, "agy");
    assert_eq!(
        antigravity.args_write,
        Some(vec!["--dangerously-skip-permissions".to_string()])
    );

    let copilot = configs
        .iter()
        .find(|config| config.name == "github-copilot")
        .expect("github-copilot should be enabled by default");
    assert_eq!(copilot.command, "copilot");
    assert_eq!(
        copilot.args,
        Vec::<String>::new(),
        "copilot keeps its model choice in the CLI default rather than args"
    );

    let opus = configs
        .iter()
        .find(|config| config.name == "claude-opus-4.8")
        .expect("claude-opus-4.8 should be enabled by default");
    assert_eq!(
        opus.args,
        vec!["--model".to_string(), "claude-opus-4-8".to_string()]
    );
}

#[test]
fn model_guide_describes_antigravity_as_google_family_lane() {
    let guide = build_model_guide_description(&[
        "code-gpt-5.5".to_string(),
        "claude-sonnet-4.6".to_string(),
        "antigravity".to_string(),
    ]);

    assert!(
        guide.contains("proactively include `antigravity` for Google/Gemini-family perspective")
    );
    assert!(guide.contains("release/workflow, infrastructure, security, or product-risk"));
    assert!(guide.contains("unless there is a clear reason to skip it"));
    assert!(guide.contains("AGY uses its configured model"));
    assert!(guide.contains("not per-run Gemini Pro/Flash flags"));
}

#[test]
fn disabled_cloud_selector_is_present_but_not_enabled_by_default() {
    let cloud = agent_model_spec("cloud").expect("cloud alias should resolve");
    assert_eq!(cloud.slug, "cloud-gpt-5.1-codex-max");
    assert!(!cloud.enabled_by_default);
    assert_eq!(cloud.gating_env, Some("CODE_ENABLE_CLOUD_AGENT_MODEL"));
}

#[test]
fn custom_model_guide_lines_override_and_extend_builtins() {
    let markdown = model_guide_markdown_with_custom(&[
        AgentConfigDefaults {
            name: "google".to_string(),
            command: "agy".to_string(),
            args: Vec::new(),
            read_only: false,
            enabled: true,
            description: Some("Custom Google lane.".to_string()),
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        },
        AgentConfigDefaults {
            name: "local-specialist".to_string(),
            command: "local-specialist".to_string(),
            args: Vec::new(),
            read_only: false,
            enabled: true,
            description: Some("Private local agent.".to_string()),
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        },
    ])
    .expect("custom descriptions should produce a guide");

    assert!(markdown.contains("- `google`: Custom Google lane."));
    assert!(markdown.contains("- `local-specialist`: Private local agent."));
    assert!(!markdown.contains("- `antigravity`: Google/Gemini-family agent via Antigravity CLI; use for Google perspective after consumer Gemini CLI retirement. AGY uses its configured model, not per-run Gemini Pro/Flash flags."));
}
