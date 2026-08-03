use super::*;
use codex_protocol::AgentPath;
use codex_protocol::protocol::Op;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;

fn test_launch(
    temp_dir: &TempDir,
    backend: ExternalCommandAgentBackendConfig,
    is_read_only: bool,
) -> ExternalAgentLaunch {
    ExternalAgentLaunch {
        thread_id: ThreadId::new(),
        parent_thread_id: ThreadId::new(),
        author: AgentPath::root(),
        recipient: AgentPath::try_from("/root/external").expect("agent path"),
        role: Some("external".to_string()),
        task_name: Some("external".to_string()),
        initial_operation: Op::UserInput {
            items: vec![UserInput::Text {
                text: "inspect this repo".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        },
        backend,
        cwd: temp_dir.path().to_path_buf(),
        cancellation_token: CancellationToken::new(),
        is_read_only,
        preflight_completed: false,
        resolved_command: None,
        hide_provider_metadata: false,
    }
}

#[test]
fn bounds_model_visible_external_agent_results() {
    let message = format!(
        "{}tail-marker",
        "x".repeat(MAX_MODEL_VISIBLE_EXTERNAL_AGENT_BYTES)
    );

    let bounded = bound_external_agent_message(&message);

    assert!(bounded.len() <= MAX_MODEL_VISIBLE_EXTERNAL_AGENT_BYTES);
    assert!(bounded.starts_with(EXTERNAL_AGENT_MESSAGE_TRUNCATED_MARKER));
    assert!(bounded.ends_with("tail-marker"));
}

#[cfg(unix)]
#[tokio::test]
async fn failed_json_response_is_bounded_before_status_and_parent_context() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::Json,
            args: vec![
                "-c".to_string(),
                r#"cat > /dev/null
printf '{"status":"failed","final_message":"'
i=0
while [ $i -lt 900 ]; do printf '0123456789'; i=$((i+1)); done
printf 'tail-marker"}'
"#
                .to_string(),
            ],
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let response = run_external_agent_inner(&launch)
        .await
        .expect("failed json response should parse");
    let final_message = response.final_message.expect("failed json final message");

    assert_eq!(response.status, ExternalAgentResponseStatus::Failed);
    assert!(
        final_message.len() <= MAX_MODEL_VISIBLE_EXTERNAL_AGENT_BYTES,
        "failed message was {} bytes",
        final_message.len()
    );
    assert!(final_message.starts_with(EXTERNAL_AGENT_MESSAGE_TRUNCATED_MARKER));
    assert!(final_message.ends_with("tail-marker"));
}

#[cfg(unix)]
#[tokio::test]
async fn failed_json_response_without_message_stays_absent() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::Json,
            args: vec![
                "-c".to_string(),
                "cat > /dev/null; printf '{\"status\":\"failed\",\"final_message\":null}'"
                    .to_string(),
            ],
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let response = run_external_agent_inner(&launch)
        .await
        .expect("failed json response should parse");

    assert_eq!(response.status, ExternalAgentResponseStatus::Failed);
    assert_eq!(response.final_message, None);
}

#[tokio::test]
async fn pre_cancelled_external_agent_does_not_launch_subprocess() {
    let temp_dir = TempDir::new().expect("tempdir");
    let marker_path = temp_dir.path().join("launched");
    let cancellation_token = CancellationToken::new();
    cancellation_token.cancel();

    let launch = ExternalAgentLaunch {
        thread_id: ThreadId::new(),
        parent_thread_id: ThreadId::new(),
        author: AgentPath::root(),
        recipient: AgentPath::try_from("/root/external").expect("agent path"),
        role: Some("external".to_string()),
        task_name: Some("external".to_string()),
        initial_operation: Op::UserInput {
            items: Vec::new(),
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        },
        backend: ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                format!("touch '{}'", marker_path.display()),
            ],
            timeout_ms: 5_000,
            ..Default::default()
        },
        cwd: temp_dir.path().to_path_buf(),
        cancellation_token,
        is_read_only: false,
        preflight_completed: false,
        resolved_command: None,
        hide_provider_metadata: false,
    };

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("pre-cancelled external agent should fail before launch");
    assert!(err.to_string().contains("cancelled before launch"));
    assert!(!marker_path.exists(), "subprocess should not launch");
}

#[test]
fn raw_cli_invocation_appends_mode_args_and_prompt() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/echo --base".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: vec!["--shared".to_string()],
            args_read_only: vec!["--readonly".to_string()],
            args_write: vec!["--write".to_string()],
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let invocation = build_external_agent_invocation(&launch, "inspect this repo")
        .expect("raw cli invocation should build");

    assert_eq!(invocation.command, PathBuf::from("/bin/echo"));
    assert_eq!(
        invocation.args,
        vec![
            "--base".to_string(),
            "--shared".to_string(),
            "--readonly".to_string(),
            "inspect this repo".to_string(),
        ]
    );
}

#[test]
fn antigravity_invocation_adds_repo_dir_and_prompt_flag() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "agy".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args_write: vec!["--dangerously-skip-permissions".to_string()],
            launch_family: Some("antigravity".to_string()),
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );

    let invocation = build_external_agent_invocation(&launch, "inspect this repo")
        .expect("antigravity invocation should build");

    assert_eq!(invocation.command, PathBuf::from("agy"));
    assert_eq!(
        invocation.args,
        vec![
            "--dangerously-skip-permissions".to_string(),
            "--add-dir".to_string(),
            temp_dir.path().display().to_string(),
            "-p".to_string(),
            "inspect this repo".to_string(),
        ]
    );
}

#[test]
fn third_party_cli_families_use_prompt_flag() {
    for (launch_family, command, mode_args) in [
        (
            "claude",
            "claude",
            vec!["--dangerously-skip-permissions".to_string()],
        ),
        (
            "copilot",
            "copilot",
            vec![
                "--autopilot".to_string(),
                "--yolo".to_string(),
                "--no-ask-user".to_string(),
                "-s".to_string(),
            ],
        ),
        ("gemini", "gemini", Vec::new()),
        ("qwen", "qwen", vec!["-y".to_string()]),
    ] {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: command.to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args_write: mode_args.clone(),
                launch_family: Some(launch_family.to_string()),
                timeout_ms: 5_000,
                ..Default::default()
            },
            /*is_read_only*/ false,
        );

        let invocation = build_external_agent_invocation(&launch, "inspect this repo")
            .expect("third-party invocation should build");

        let mut expected_args = mode_args;
        expected_args.extend(["-p".to_string(), "inspect this repo".to_string()]);
        assert_eq!(invocation.command, PathBuf::from(command));
        assert_eq!(invocation.args, expected_args, "family {launch_family}");
    }
}

#[test]
fn positional_prompt_families_keep_bare_prompt() {
    for launch_family in ["code", "codex", "cloud"] {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "coder".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec!["--model".to_string(), "gpt-5.5".to_string()],
                args_write: vec![
                    "-s".to_string(),
                    "workspace-write".to_string(),
                    "exec".to_string(),
                    "--skip-git-repo-check".to_string(),
                ],
                launch_family: Some(launch_family.to_string()),
                timeout_ms: 5_000,
                ..Default::default()
            },
            /*is_read_only*/ false,
        );

        let invocation = build_external_agent_invocation(&launch, "inspect this repo")
            .expect("code-family invocation should build");

        assert_eq!(invocation.command, PathBuf::from("coder"));
        assert_eq!(
            invocation.args,
            vec![
                "--model".to_string(),
                "gpt-5.5".to_string(),
                "-s".to_string(),
                "workspace-write".to_string(),
                "exec".to_string(),
                "--skip-git-repo-check".to_string(),
                "inspect this repo".to_string(),
            ],
            "family {launch_family}"
        );
        assert!(
            !invocation.args.iter().any(|arg| arg == "-p"),
            "family {launch_family} should not use prompt flag"
        );
    }
}

#[tokio::test]
async fn missing_builtin_third_party_cli_reports_install_hint() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "definitely-missing-claude-code-test-command".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("claude".to_string()),
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );
    let err = preflight_external_agent_backend(
        launch.role.as_deref(),
        &launch.backend,
        &launch.cwd,
        launch.is_read_only,
    )
    .await
    .expect_err("missing built-in third-party CLI should fail preflight");

    assert_eq!(err.kind, ExternalAgentFailureKind::CommandMissing);
    let message = err.to_string();
    assert!(message.contains("Claude Code command"), "{message}");
    assert!(
        message.contains("definitely-missing-claude-code-test-command"),
        "{message}"
    );
    assert!(
        message.contains("Install claude-code") && message.contains("on PATH"),
        "{message}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn antigravity_preflight_classifies_missing_authentication() {
    let temp_dir = TempDir::new().expect("tempdir");
    let script_path = temp_dir.path().join("fake-agy.sh");
    std::fs::write(
        &script_path,
        r#"if [ "$1" = "--version" ]; then
  echo "Antigravity CLI 1.2.3"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo "Authentication required. Please sign in." >&2
  exit 1
fi
exit 2
"#,
    )
    .expect("write fake Antigravity CLI");
    let backend = ExternalCommandAgentBackendConfig {
        command: format!("/bin/sh {}", script_path.display()),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("antigravity".to_string()),
        timeout_ms: 5_000,
        ..Default::default()
    };

    let err = preflight_external_agent_backend(
        Some("antigravity"),
        &backend,
        temp_dir.path(),
        /*is_read_only*/ true,
    )
    .await
    .expect_err("signed-out Antigravity CLI should fail preflight");

    assert_eq!(err.kind, ExternalAgentFailureKind::AuthenticationRequired);
    assert!(err.to_string().contains("Authentication required"));
}

#[cfg(unix)]
#[tokio::test]
async fn antigravity_preflight_records_cli_version() {
    let temp_dir = TempDir::new().expect("tempdir");
    let script_path = temp_dir.path().join("fake-agy.sh");
    std::fs::write(
        &script_path,
        r#"if [ "$1" = "--version" ]; then
  echo "Antigravity CLI 1.2.3"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo "Gemini 3.1 Pro"
  exit 0
fi
exit 2
"#,
    )
    .expect("write fake Antigravity CLI");
    let backend = ExternalCommandAgentBackendConfig {
        command: format!("/bin/sh {}", script_path.display()),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("antigravity".to_string()),
        timeout_ms: 5_000,
        ..Default::default()
    };

    let provenance = preflight_external_agent_backend(
        Some("antigravity"),
        &backend,
        temp_dir.path(),
        /*is_read_only*/ true,
    )
    .await
    .expect("authenticated Antigravity CLI should pass preflight");

    assert_eq!(
        provenance.cli_version.as_deref(),
        Some("Antigravity CLI 1.2.3")
    );
    assert_eq!(provenance.provider_family.as_deref(), Some("antigravity"));
}

#[cfg(unix)]
#[tokio::test]
async fn completed_preflight_is_not_repeated_during_launch() {
    let temp_dir = TempDir::new().expect("tempdir");
    let marker_path = temp_dir.path().join("preflight-reran");
    let script_path = temp_dir.path().join("fake-agy.sh");
    std::fs::write(
        &script_path,
        format!(
            r#"if [ "$1" = "--version" ] || [ "$1" = "models" ]; then
  : > '{}'
  exit 1
fi
echo "RUNTIME_OK"
"#,
            marker_path.display()
        ),
    )
    .expect("write fake Antigravity CLI");
    let mut launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: format!("/bin/sh {}", script_path.display()),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("antigravity".to_string()),
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );
    launch.preflight_completed = true;

    let response = run_external_agent_inner(&launch)
        .await
        .expect("completed preflight should not run again");

    assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
    assert_eq!(response.final_message.as_deref(), Some("RUNTIME_OK"));
    assert!(!marker_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn preflight_resolves_backend_path_and_reuses_exact_command() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().expect("tempdir");
    let bin_dir = temp_dir.path().join("bin");
    tokio::fs::create_dir_all(&bin_dir)
        .await
        .expect("bin dir should be created");
    let command_path = bin_dir.join("fake-claude");
    tokio::fs::write(
        &command_path,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "Claude Code 2.1.212"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  echo '{"loggedIn":true,"authMethod":"test"}'
  exit 0
fi
if [ "$1" = "-p" ]; then
  echo "PATH_COMMAND_OK"
  exit 0
fi
exit 2
"#,
    )
    .await
    .expect("fake Claude CLI should be written");
    let mut permissions = std::fs::metadata(&command_path)
        .expect("fake Claude CLI metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&command_path, permissions)
        .expect("fake Claude CLI should be executable");
    let backend = ExternalCommandAgentBackendConfig {
        command: "fake-claude".to_string(),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("claude".to_string()),
        env: HashMap::from([("PATH".to_string(), bin_dir.display().to_string())]),
        timeout_ms: 5_000,
        ..Default::default()
    };

    let provider = preflight_external_agent_backend(
        Some("claude"),
        &backend,
        temp_dir.path(),
        /*is_read_only*/ true,
    )
    .await
    .expect("backend PATH command should pass preflight");
    assert_eq!(provider.resolved_command(), Some(command_path.as_path()));

    let mut launch = test_launch(&temp_dir, backend, /*is_read_only*/ true);
    launch.preflight_completed = true;
    launch.resolved_command = provider
        .resolved_command()
        .map(std::path::Path::to_path_buf);
    let response = run_external_agent_inner(&launch)
        .await
        .expect("resolved command should launch successfully");
    assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
    assert_eq!(response.final_message.as_deref(), Some("PATH_COMMAND_OK"));
}

#[cfg(unix)]
#[tokio::test]
async fn timed_out_preflight_kills_process_group() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().expect("tempdir");
    let pid_path = temp_dir.path().join("preflight.pid");
    let command_path = temp_dir.path().join("hanging-provider");
    tokio::fs::write(
        &command_path,
        format!("#!/bin/sh\necho $$ > '{}'\nsleep 30\n", pid_path.display()),
    )
    .await
    .expect("hanging provider should be written");
    let mut permissions = std::fs::metadata(&command_path)
        .expect("hanging provider metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&command_path, permissions)
        .expect("hanging provider should be executable");
    let backend = ExternalCommandAgentBackendConfig::default();

    let error = run_external_agent_preflight_command_with_timeout(
        &backend,
        &command_path,
        &[],
        temp_dir.path(),
        &["--version"],
        "version",
        ExternalAgentPreflightOutputLimit::Diagnostic,
        Duration::from_millis(500),
    )
    .await
    .expect_err("hanging preflight should time out");
    assert_eq!(error.kind, ExternalAgentFailureKind::TimedOut);

    let pid: i32 = tokio::fs::read_to_string(&pid_path)
        .await
        .expect("preflight should record its pid")
        .trim()
        .parse()
        .expect("pid should parse");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .expect("kill probe should run");
            if !status.success() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timed-out preflight process should be killed");
}

#[tokio::test]
async fn first_party_and_custom_raw_cli_commands_require_executable_commands() {
    for launch_family in [Some("code"), Some("codex"), Some("cloud"), None] {
        let temp_dir = TempDir::new().expect("tempdir");
        let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "definitely-missing-custom-agent-test-command".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                launch_family: launch_family.map(str::to_string),
                timeout_ms: 5_000,
                ..Default::default()
            },
            /*is_read_only*/ false,
        );
        let err = preflight_external_agent_backend(
            launch.role.as_deref(),
            &launch.backend,
            &launch.cwd,
            launch.is_read_only,
        )
        .await
        .expect_err("missing external command should fail preflight");
        assert_eq!(err.kind, ExternalAgentFailureKind::CommandMissing);
    }
}

#[tokio::test]
async fn non_github_copilot_command_is_rejected() {
    let temp_dir = TempDir::new().expect("tempdir");
    let command = std::env::current_exe().expect("current test executable");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: command.display().to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("copilot".to_string()),
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );
    let err = preflight_external_agent_backend(
        launch.role.as_deref(),
        &launch.backend,
        &launch.cwd,
        launch.is_read_only,
    )
    .await
    .expect_err("non-GitHub copilot executable should fail preflight");

    assert_eq!(err.kind, ExternalAgentFailureKind::LaunchFailed);
    let message = err.to_string();
    assert!(message.contains("resolved to a different `copilot` executable"));
    assert!(message.contains("Install GitHub Copilot CLI"));
}

#[cfg(unix)]
#[tokio::test]
async fn raw_cli_auth_failure_is_classified() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: vec![
                "-c".to_string(),
                "echo 'Authentication required. Please sign in.'; exit 1".to_string(),
            ],
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("authentication failure should fail the external agent");

    assert_eq!(
        err.detail.kind,
        ExternalAgentFailureKind::AuthenticationRequired
    );
}

#[cfg(unix)]
#[tokio::test]
async fn raw_cli_rate_limit_failure_is_classified() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: vec![
                "-c".to_string(),
                "echo 'HTTP 429: quota exceeded' >&2; exit 1".to_string(),
            ],
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("rate limit should fail the external agent");

    assert_eq!(
        err.detail.kind,
        ExternalAgentFailureKind::QuotaOrRateLimited
    );
}

#[cfg(unix)]
#[tokio::test]
async fn raw_cli_empty_output_is_classified() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "true".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("empty output should fail the external agent");

    assert_eq!(err.detail.kind, ExternalAgentFailureKind::EmptyOutput);
}

#[cfg(unix)]
#[tokio::test]
async fn malformed_json_output_is_classified() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/cat".to_string(),
            protocol: ExternalCommandProtocol::Json,
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("request echo should not parse as an external response");

    assert_eq!(err.detail.kind, ExternalAgentFailureKind::MalformedOutput);
}

#[test]
fn split_command_and_args_preserves_absolute_windows_paths() {
    let (command, args) =
        split_command_and_args(r"C:\Program Files\GitHub Copilot\copilot.exe").expect("split");
    assert_eq!(command, r"C:\Program Files\GitHub Copilot\copilot.exe");
    assert!(args.is_empty());

    let (command, args) = split_command_and_args(r"D:\tools\claude.exe").expect("split");
    assert_eq!(command, r"D:\tools\claude.exe");
    assert!(args.is_empty());

    let (command, args) =
        split_command_and_args("C:/Program Files/GitHub Copilot/copilot.exe").expect("split");
    assert_eq!(command, "C:/Program Files/GitHub Copilot/copilot.exe");
    assert!(args.is_empty());

    let (command, args) = split_command_and_args(r"\\build\share\agents\qwen.exe").expect("split");
    assert_eq!(command, r"\\build\share\agents\qwen.exe");
    assert!(args.is_empty());
}

#[test]
fn split_command_and_args_splits_windows_paths_with_inline_args() {
    let (command, args) =
        split_command_and_args("C:/tools/copilot.exe --model fast").expect("split");
    assert_eq!(command, "C:/tools/copilot.exe");
    assert_eq!(args, vec!["--model".to_string(), "fast".to_string()]);

    let (command, args) =
        split_command_and_args(r"C:\Program Files\GitHub Copilot\copilot.exe --model fast")
            .expect("split");
    assert_eq!(command, r"C:\Program Files\GitHub Copilot\copilot.exe");
    assert_eq!(args, vec!["--model".to_string(), "fast".to_string()]);

    let (command, args) =
        split_command_and_args(r#""C:\Program Files\GitHub Copilot\copilot.exe" --model fast"#)
            .expect("split");

    assert_eq!(command, r"C:\Program Files\GitHub Copilot\copilot.exe");
    assert_eq!(args, vec!["--model".to_string(), "fast".to_string()]);

    let (command, args) =
        split_command_and_args(r#""\\build\share\GitHub Copilot\copilot.exe" --model fast"#)
            .expect("split");
    assert_eq!(command, r"\\build\share\GitHub Copilot\copilot.exe");
    assert_eq!(args, vec!["--model".to_string(), "fast".to_string()]);

    let (command, args) =
        split_command_and_args(r"C:\Windows\System32\cmd.exe /c C:\tools\build.bat")
            .expect("split");
    assert_eq!(command, r"C:\Windows\System32\cmd.exe");
    assert_eq!(
        args,
        vec!["/c".to_string(), r"C:\tools\build.bat".to_string()]
    );

    let (command, args) =
        split_command_and_args(r"C:\tools\Copilot.EXE --config C:\cfg\app.json").expect("split");
    assert_eq!(command, r"C:\tools\Copilot.EXE");
    assert_eq!(
        args,
        vec!["--config".to_string(), r"C:\cfg\app.json".to_string()]
    );

    let (command, args) =
        split_command_and_args(r#"C:\tools\Copilot.exe --config "C:\Program Files\config.json""#)
            .expect("split");
    assert_eq!(command, r"C:\tools\Copilot.exe");
    assert_eq!(
        args,
        vec![
            "--config".to_string(),
            r"C:\Program Files\config.json".to_string(),
        ]
    );
}

#[test]
fn split_command_and_args_preserves_current_exe_path() {
    let current_exe = std::env::current_exe().expect("current test executable");
    let current_exe_text = current_exe.to_string_lossy();
    let rendered = shlex::try_quote(current_exe_text.as_ref()).expect("quote");

    let (command, args) = split_command_and_args(rendered.as_ref()).expect("split");

    assert_eq!(PathBuf::from(&command), current_exe);
    assert!(args.is_empty());
}

#[test]
fn split_command_and_args_still_splits_posix_commands() {
    let (command, args) =
        split_command_and_args("npx -y @openai/codex 'hello world'").expect("split");

    assert_eq!(command, "npx");
    assert_eq!(
        args,
        vec![
            "-y".to_string(),
            "@openai/codex".to_string(),
            "hello world".to_string(),
        ]
    );
}

#[test]
fn github_copilot_version_output_accepts_official_banner() {
    assert!(github_copilot_version_output(
        b"GitHub Copilot CLI 1.0.71.\n",
        b""
    ));
    assert!(github_copilot_version_output(
        b"",
        b"notice: GitHub Copilot CLI 1.0.71 is ready\n"
    ));
    assert!(!github_copilot_version_output(
        b"copilot version: 1.34.1\n",
        b""
    ));
}

#[test]
fn bounded_preflight_output_preserves_utf8_boundaries() {
    let mut output = vec![b'x'];
    output.extend_from_slice("é".as_bytes());
    output.resize(MAX_PREFLIGHT_MESSAGE_BYTES + 2, b'y');

    let output = bounded_preflight_output(&output, b"");

    assert!(!output.contains('\u{fffd}'));
    assert!(output.len() <= MAX_PREFLIGHT_MESSAGE_BYTES);
}

#[test]
fn antigravity_launch_cwd_uses_private_cache_dir() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            launch_family: Some("antigravity".to_string()),
            ..Default::default()
        },
        /*is_read_only*/ false,
    );

    let launch_cwd = external_agent_launch_cwd(&launch);

    assert!(launch_cwd.ends_with("agent-cache/antigravity"));
    assert_ne!(launch_cwd, launch.cwd);
}

#[tokio::test]
async fn antigravity_launch_requires_existing_workspace_dir() {
    let temp_dir = TempDir::new().expect("tempdir");
    let missing_workspace = temp_dir.path().join("missing-workspace");
    let mut launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/echo".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("antigravity".to_string()),
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );
    launch.cwd = missing_workspace.clone();

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("missing antigravity workspace should fail before spawn");

    assert!(
        err.to_string().contains(&format!(
            "antigravity workspace directory does not exist: {}",
            missing_workspace.display()
        )),
        "unexpected error: {err}"
    );
}

#[test]
fn json_invocation_keeps_command_as_literal_path() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/tmp/external agent/helper".to_string(),
            args: vec!["--json".to_string()],
            args_read_only: vec!["--readonly".to_string()],
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ true,
    );

    let invocation = build_external_agent_invocation(&launch, "inspect this repo")
        .expect("json invocation should build");

    assert_eq!(
        invocation.command,
        PathBuf::from("/tmp/external agent/helper")
    );
    assert_eq!(
        invocation.args,
        vec!["--json".to_string(), "--readonly".to_string()]
    );
}

#[test]
fn raw_cli_rejects_invalid_command_quoting() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "coder 'unterminated".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );

    let err = build_external_agent_invocation(&launch, "inspect this repo")
        .expect_err("invalid raw cli command quoting should be rejected");

    assert!(
        err.to_string().contains("invalid shell quoting"),
        "unexpected error: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn raw_cli_uses_argv_prompt_and_configured_env() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: vec![
                "-c".to_string(),
                "printf '%s|%s|%s' \"$1\" \"$2\" \"$EXTERNAL_AGENT_ENV\"".to_string(),
                "external-agent-test".to_string(),
            ],
            args_write: vec!["--write-mode".to_string()],
            env: std::collections::HashMap::from([(
                "EXTERNAL_AGENT_ENV".to_string(),
                "configured".to_string(),
            )]),
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );

    let response = run_external_agent_inner(&launch)
        .await
        .expect("raw cli helper should complete");

    assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
    assert_eq!(
        response.final_message.as_deref(),
        Some("--write-mode|inspect this repo|configured")
    );
}

#[test]
fn external_agent_process_env_sets_artifact_target_scope() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            env: HashMap::from([
                ("EXTERNAL_AGENT_ENV".to_string(), "configured".to_string()),
                (
                    CARGO_TARGET_DIR_ENV_VAR.to_string(),
                    "/tmp/shared-target".to_string(),
                ),
                (
                    CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR.to_string(),
                    "/tmp/explicit-target".to_string(),
                ),
                (
                    CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR.to_string(),
                    "shared".to_string(),
                ),
                (
                    CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR.to_string(),
                    "configured-key".to_string(),
                ),
            ]),
            ..Default::default()
        },
        /*is_read_only*/ false,
    );

    let env = external_agent_process_env(&launch);

    assert_eq!(
        env.get("EXTERNAL_AGENT_ENV"),
        Some(&"configured".to_string())
    );
    assert_eq!(
        env.get(CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR),
        Some(&EXTERNAL_AGENT_CARGO_TARGET_SCOPE_VALUE.to_string())
    );
    assert_eq!(
        env.get(CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR),
        Some(&launch.thread_id.to_string())
    );
    assert_eq!(env.get(CARGO_TARGET_DIR_ENV_VAR), None);
    assert_eq!(env.get(CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR), None);
}

#[cfg(unix)]
#[tokio::test]
async fn raw_cli_receives_artifact_target_scope_env() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: vec![
                "-c".to_string(),
                format!(
                    "printf '%s|%s' \"${}\" \"${}\"",
                    CODEX_LAB_CARGO_TARGET_SCOPE_ENV_VAR, CODEX_LAB_CARGO_TARGET_KEY_ENV_VAR,
                ),
            ],
            timeout_ms: 5_000,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );
    let expected_thread_id = launch.thread_id.to_string();

    let response = run_external_agent_inner(&launch)
        .await
        .expect("raw cli helper should complete");
    let expected = format!("{EXTERNAL_AGENT_CARGO_TARGET_SCOPE_VALUE}|{expected_thread_id}");

    assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
    assert_eq!(response.final_message.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn oversized_output_is_truncated_instead_of_failing_wrapper() {
    let (mut writer, reader) = tokio::io::duplex(256);
    let payload = b"abcdefghijklmnopqrstuvwx".to_vec();
    let writer_task = tokio::spawn(async move {
        writer
            .write_all(&payload)
            .await
            .expect("write oversized payload");
    });

    let output = read_limited_output(reader, /*limit*/ 8, "stdout")
        .await
        .expect("oversized output should truncate, not fail");
    writer_task.await.expect("writer task should finish");

    assert!(output.starts_with(EXTERNAL_AGENT_TRUNCATED_MARKER));
    assert!(output.ends_with(b"qrstuvwx"));
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_subprocess_stdout_keeps_tail_without_sigpipe_failure() {
    let temp_dir = TempDir::new().expect("tempdir");
    let launch = test_launch(
            &temp_dir,
            ExternalCommandAgentBackendConfig {
                command: "/bin/sh".to_string(),
                protocol: ExternalCommandProtocol::RawCli,
                args: vec![
                    "-c".to_string(),
                    "python3 - <<'PY'\nimport sys\nsys.stdout.write('a' * 70000)\nsys.stdout.write('tail-marker')\nPY"
                        .to_string(),
                ],
                timeout_ms: 5_000,
                ..Default::default()
            },
            /*is_read_only*/ false,
        );

    let response = run_external_agent_inner(&launch)
        .await
        .expect("oversized stdout should truncate without killing the child");

    assert_eq!(response.status, ExternalAgentResponseStatus::Completed);
    let final_message = response.final_message.expect("raw cli final message");
    assert!(final_message.starts_with("[external agent output truncated]"));
    assert!(final_message.ends_with("tail-marker"));
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_external_agent_background_children() {
    let temp_dir = TempDir::new().expect("tempdir");
    let survived_path = temp_dir.path().join("background-child-survived");
    let script = format!("(sleep 1; touch '{}') & wait", survived_path.display());
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: vec!["-c".to_string(), script],
            timeout_ms: 100,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("external agent wrapper should time out");
    assert!(
        err.to_string().contains("timed out"),
        "unexpected error: {err}"
    );

    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(
        !survived_path.exists(),
        "timeout should kill background descendants in the external agent process group"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_background_children_after_wrapper_exits() {
    let temp_dir = TempDir::new().expect("tempdir");
    let survived_path = temp_dir.path().join("background-child-survived");
    let script = format!("(sleep 1; touch '{}') &", survived_path.display());
    let launch = test_launch(
        &temp_dir,
        ExternalCommandAgentBackendConfig {
            command: "/bin/sh".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: vec!["-c".to_string(), script],
            timeout_ms: 100,
            ..Default::default()
        },
        /*is_read_only*/ false,
    );

    let err = run_external_agent_inner(&launch)
        .await
        .expect_err("external agent descendant should hold stdout open until timeout");
    assert!(
        err.to_string().contains("timed out"),
        "unexpected error: {err}"
    );

    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(
        !survived_path.exists(),
        "timeout should kill background descendants after the wrapper exits"
    );
}
