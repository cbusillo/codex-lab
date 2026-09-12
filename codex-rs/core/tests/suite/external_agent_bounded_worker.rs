use super::*;
use pretty_assertions::assert_eq;

struct WorkerOutput {
    spawn: String,
    agents: Value,
    model_request: String,
}

#[derive(Clone, Copy)]
enum Completion {
    Wait,
    Cancel,
}

fn worker_arguments(context: Value) -> Value {
    json!({
        "message": AGENT_MESSAGE, "task_name": "external_probe", "agent_type": ROLE,
        "fork_turns": "none",
        "bounded_worker": {
            "timeout_ms": 5000, "max_input_bytes": 8192, "max_result_bytes": 256,
            "context": context
        }
    })
}

async fn run_worker(
    backend: ExternalCommandAgentBackendConfig,
    arguments: Value,
    workspace: &std::path::Path,
    completion: Completion,
) -> Result<WorkerOutput> {
    let server = start_mock_server().await;
    let mut calls = vec![(SPAWN_CALL_ID, "spawn_agent", arguments)];
    if matches!(completion, Completion::Cancel) {
        calls.push((
            "cancel-worker",
            "interrupt_agent",
            json!({"target": "/root/external_probe"}),
        ));
    }
    calls.push((WAIT_CALL_ID, "wait_agent", json!({"timeout_ms": 5000})));
    calls.push((
        LIST_CALL_ID,
        "list_agents",
        json!({"path_prefix":"/root/external_probe"}),
    ));
    let mut previous = None;
    for (call_id, tool, arguments) in calls {
        responses::mount_sse_once_match(
            &server,
            move |request: &wiremock::Request| {
                previous.is_none_or(|id| body_contains(request, id))
                    && !body_contains(request, call_id)
            },
            sse(vec![
                ev_response_created(call_id),
                ev_function_call_with_namespace(
                    call_id,
                    COLLABORATION_NAMESPACE,
                    tool,
                    &arguments.to_string(),
                ),
                ev_completed(call_id),
            ]),
        )
        .await;
        previous = Some(call_id);
    }
    let final_response = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, LIST_CALL_ID),
        sse(vec![
            ev_response_created("done"),
            ev_assistant_message("done-message", "done"),
            ev_completed("done"),
        ]),
    )
    .await;
    let workspace = codex_utils_absolute_path::AbsolutePathBuf::try_from(workspace.to_path_buf())?;
    let mut builder = builder_with_external_role(backend).with_config(move |config| {
        config.cwd = workspace;
        config
            .agent_roles
            .insert("antigravity".to_string(), AgentRoleConfig::default());
    });
    let test = builder.build(&server).await?;
    test.submit_turn(PROMPT).await?;
    let request = final_response.single_request();
    Ok(WorkerOutput {
        model_request: request.body_json().to_string(),
        spawn: request.function_call_output(SPAWN_CALL_ID)["output"]
            .as_str()
            .expect("spawn output")
            .to_string(),
        agents: serde_json::from_str(
            request.function_call_output(LIST_CALL_ID)["output"]
                .as_str()
                .expect("list output"),
        )?,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_worker_preserves_complete_instructions_and_retains_bounded_result() -> Result<()> {
    let dir = TempDir::new()?;
    std::fs::create_dir(dir.path().join(".git"))?;
    std::fs::write(dir.path().join("AGENTS.md"), "Keep the widget blue.")?;
    std::fs::write(dir.path().join("widget.rs"), "")?;
    let received = dir.path().join("received");
    let mut backend = stub_cli(
        &dir,
        "worker.sh",
        &format!(
            "case \"$1\" in --version) echo 'Claude Code 2.1.220'; exit;; --help) echo '--effort'; exit;; auth) echo '{{\"loggedIn\":true}}'; exit;; esac\nprintf '%s' \"$*\" > '{}'\nfor i in $(seq 1 100); do printf 'result-text-'; done\n",
            received.display()
        ),
    );
    backend.launch_family = Some("claude".to_string());
    let output = run_worker(
        backend,
        worker_arguments(json!({"type":"code", "paths":["widget.rs"]})),
        dir.path(),
        Completion::Wait,
    )
    .await?;
    let mut spawn: Value = serde_json::from_str(&output.spawn)?;
    spawn["nickname"] = json!("[generated nickname]");
    insta::assert_snapshot!(
        "bounded_worker_accepted",
        serde_json::to_string_pretty(&spawn)?
    );
    let message = std::fs::read_to_string(received)?;
    assert!(message.contains("Keep the widget blue."));
    assert!(message.ends_with(AGENT_MESSAGE));
    let agent = &output.agents["agents"][0];
    let result = agent["agent_status"]["completed"]
        .as_str()
        .expect("completed result");
    assert!(result.len() <= 256, "{result}");
    assert!(result.starts_with("[bounded worker result truncated]"));
    assert_eq!(agent["provider"]["cli_version"], "Claude Code 2.1.220");
    assert_eq!(agent["provider"]["capability_source"], "static_catalog");
    assert!(agent["provider"]["model"].is_null());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_worker_refuses_missing_nested_or_oversized_instructions() -> Result<()> {
    let dir = TempDir::new()?;
    std::fs::create_dir(dir.path().join(".git"))?;
    std::fs::write(dir.path().join("AGENTS.md"), "Root instructions.")?;
    std::fs::create_dir(dir.path().join("nested"))?;
    std::fs::write(dir.path().join("nested/AGENTS.md"), "Nested instructions.")?;
    let marker = dir.path().join("launched");
    let backend = stub_cli(
        &dir,
        "worker.sh",
        &format!("touch '{}'\necho done\n", marker.display()),
    );
    let output = run_worker(
        backend.clone(),
        worker_arguments(json!({"type":"code", "paths":["nested/new.rs"]})),
        dir.path(),
        Completion::Wait,
    )
    .await?;
    assert!(
        output.spawn.contains("missing applicable instructions"),
        "{}",
        output.spawn
    );
    assert!(!marker.exists());
    std::fs::write(
        dir.path().join("AGENTS.md"),
        "complete instructions ".repeat(/*n*/ 2000),
    )?;
    let output = run_worker(
        backend,
        worker_arguments(json!({"type":"code", "paths":["new.rs"]})),
        dir.path(),
        Completion::Wait,
    )
    .await?;
    assert!(
        output.spawn.contains("incomplete") || output.spawn.contains("exceed max_input_bytes"),
        "{}",
        output.spawn
    );
    assert!(!marker.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_text_worker_sends_only_task_and_requires_explicit_supported_selection()
-> Result<()> {
    let dir = TempDir::new()?;
    std::fs::write(
        dir.path().join("AGENTS.md"),
        "Instructions for unrelated code work.",
    )?;
    let received = dir.path().join("received");
    let backend = stub_cli(
        &dir,
        "worker.sh",
        &format!("printf '%s' \"$*\" > '{}'\necho done\n", received.display()),
    );
    let args = worker_arguments(json!({"type":"text"}));
    let output = run_worker(backend.clone(), args.clone(), dir.path(), Completion::Wait).await?;
    assert!(
        serde_json::from_str::<Value>(&output.spawn).is_ok(),
        "{}",
        output.spawn
    );
    assert_eq!(
        std::fs::read_to_string(&received)?,
        format!(
            "Message Type: NEW_TASK\nTask name: /root/external_probe\nSender: /root\nPayload:\n{AGENT_MESSAGE}"
        )
    );
    std::fs::remove_file(&received)?;
    for protocol in [
        ExternalCommandProtocol::RawCli,
        ExternalCommandProtocol::Json,
    ] {
        let mut limited = args.clone();
        limited["bounded_worker"]["max_input_bytes"] = json!(AGENT_MESSAGE.len());
        let mut limited_backend = backend.clone();
        limited_backend.protocol = protocol;
        if protocol == ExternalCommandProtocol::Json {
            limited_backend.command = "/bin/sh".to_string();
            limited_backend.args = vec![dir.path().join("worker.sh").display().to_string()];
        }
        let output = run_worker(limited_backend, limited, dir.path(), Completion::Wait).await?;
        assert!(
            output.agents["agents"][0]["agent_status"]["errored"]
                .as_str()
                .expect("bounded refusal")
                .contains("exceeds max_input_bytes")
        );
        assert!(!received.exists());
    }
    for patch in [
        json!({"agent_type":null}),
        json!({"service_tier":"priority"}),
        json!({"fork_turns":"all"}),
    ] {
        let mut request = args.clone();
        request
            .as_object_mut()
            .expect("args")
            .extend(patch.as_object().expect("patch").clone());
        let output = run_worker(backend.clone(), request, dir.path(), Completion::Wait).await?;
        assert!(
            serde_json::from_str::<Value>(&output.spawn).is_err(),
            "{}",
            output.spawn
        );
        assert!(!received.exists());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_worker_deadline_includes_preflight_and_does_not_launch_task() -> Result<()> {
    let dir = TempDir::new()?;
    let preflight = dir.path().join("preflight");
    let task = dir.path().join("task");
    let mut backend = stub_cli(
        &dir,
        "worker.sh",
        &format!(
            "if [ \"$1\" = --version ]; then touch '{}'; sleep 10; exit 0; fi\ntouch '{}'\necho done\n",
            preflight.display(),
            task.display()
        ),
    );
    backend.launch_family = Some("claude".to_string());
    let mut arguments = worker_arguments(json!({"type":"text"}));
    arguments["bounded_worker"]["timeout_ms"] = json!(1000);
    let output = run_worker(backend, arguments, dir.path(), Completion::Wait).await?;
    assert!(preflight.exists());
    assert!(!task.exists());
    let agent = &output.agents["agents"][0];
    assert_eq!(agent["failure"]["kind"], "timed_out");
    assert!(
        agent["agent_status"]["errored"]
            .as_str()
            .expect("error")
            .contains("including preflight")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_worker_targeted_cancellation_has_truthful_terminal_state() -> Result<()> {
    let dir = TempDir::new()?;
    let backend = stub_cli(&dir, "worker.sh", "sleep 10\necho too-late\n");
    let output = run_worker(
        backend,
        worker_arguments(json!({"type":"text"})),
        dir.path(),
        Completion::Cancel,
    )
    .await?;
    assert!(
        serde_json::from_str::<Value>(&output.spawn).is_ok(),
        "{}",
        output.spawn
    );
    assert_eq!(output.agents["agents"][0]["agent_status"], "shutdown");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_worker_caps_provider_failure_messages_after_quota_decoding() -> Result<()> {
    let dir = TempDir::new()?;
    let quota = json!({"type":"rate_limit_event", "rate_limit_info":{
        "status":"rejected", "rateLimitType":"five_hour", "resetsAt":1783830000,
        "overageStatus":"rejected", "overageDisabledReason":"x".repeat(/*n*/ 128),
        "isUsingOverage":false
    }});
    let result =
        json!({"type":"result", "is_error":true, "result":"provider failure ".repeat(/*n*/ 100)});
    let mut backend = stub_cli(
        &dir,
        "quota.sh",
        &format!(
            "case \"$1\" in --version) echo 'Claude Code 2.1.220'; exit;; --help) echo '--verbose --output-format'; exit;; auth) echo '{{\"loggedIn\":true}}'; exit;; esac\nprintf '%s\\n' '{quota}' '{result}'\n",
        ),
    );
    backend.launch_family = Some("claude".to_string());
    let output = run_worker(
        backend,
        worker_arguments(json!({"type":"text"})),
        dir.path(),
        Completion::Wait,
    )
    .await?;
    let agent = &output.agents["agents"][0];
    let status = agent["agent_status"]["errored"]
        .as_str()
        .expect("errored status");
    assert!(status.len() <= 256);
    assert!(status.starts_with("[bounded worker result truncated]"));
    assert!(
        !output
            .model_request
            .contains(&"provider failure ".repeat(/*n*/ 32))
    );
    let failure = &agent["failure"];
    assert_eq!(failure["kind"], "quota_or_rate_limited");
    let message = failure["message"].as_str().expect("failure message");
    assert!(message.len() <= 256);
    assert!(message.starts_with("[bounded worker result truncated]"));
    Ok(())
}
