use super::*;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_code_worker_explicit_budget_delivers_complete_large_instructions() -> Result<()> {
    let dir = TempDir::new()?;
    std::fs::create_dir(dir.path().join(".git"))?;
    // Public-safe, portable instructions with the same combined size as the
    // qualified 26,821-byte project + 3,825-byte home + 23-byte separator case.
    let mut instructions =
        "Keep the widget blue. Preserve all applicable instructions.\n".repeat(/*n*/ 520);
    instructions.truncate(/*new_len*/ 30_630);
    instructions
        .push_str(&" ".repeat(30_669 - instructions.len() - "END OF COMPLETE INSTRUCTIONS".len()));
    instructions.push_str("END OF COMPLETE INSTRUCTIONS");
    std::fs::write(dir.path().join("AGENTS.md"), &instructions)?;
    std::fs::write(dir.path().join("widget.rs"), "")?;
    let received = dir.path().join("received");
    let backend = stub_cli(
        &dir,
        "worker.sh",
        &format!("printf '%s' \"$*\" > '{}'\necho done\n", received.display()),
    );
    let mut args = worker_arguments(json!({"type":"code", "paths":["widget.rs"]}));
    let refused = run_worker(backend.clone(), args.clone(), dir.path(), Completion::Wait).await?;
    assert!(
        refused.spawn.contains("exceeds max_input_bytes"),
        "{}",
        refused.spawn
    );
    assert_eq!(refused.agents, json!({"agents":[]}));
    assert!(!received.exists());

    args["bounded_worker"]["max_input_bytes"] = json!(32_768);
    let accepted = run_worker(backend.clone(), args.clone(), dir.path(), Completion::Wait).await?;
    let spawn: Value = serde_json::from_str(&accepted.spawn)?;
    assert_eq!(
        spawn["bounded_worker"],
        json!({"max_input_bytes":32_768, "max_result_bytes":256, "timeout_ms":5000})
    );
    let expected = format!(
        "Message Type: NEW_TASK\nTask name: /root/external_probe\nSender: /root\nPayload:\n<bounded_worker_instructions>Work only on these declared files: widget.rs. Do not delegate this task.\n\nApplicable instructions:\n{instructions}\n\nRole instructions:\n\n</bounded_worker_instructions>\n\n{AGENT_MESSAGE}"
    );
    assert_eq!(std::fs::read_to_string(&received)?, expected);
    assert_eq!(
        accepted.agents["agents"][0]["agent_status"],
        json!({"completed":"done"})
    );
    std::fs::remove_file(&received)?;

    // The assembled message fits; the final RawCli routing envelope does not.
    args["bounded_worker"]["max_input_bytes"] = json!(expected.len() - 1);
    let refused = run_worker(backend.clone(), args.clone(), dir.path(), Completion::Wait).await?;
    assert!(
        refused.agents["agents"][0]["agent_status"]["errored"]
            .as_str()
            .expect("envelope refusal")
            .contains("exceeds max_input_bytes")
    );
    assert!(!received.exists());

    // The entire raw prompt fits; the serialized JSON envelope and newline do not.
    args["bounded_worker"]["max_input_bytes"] = json!(expected.len());
    let mut json_backend = backend;
    json_backend.protocol = ExternalCommandProtocol::Json;
    json_backend.command = "/bin/sh".to_string();
    json_backend.args = vec![dir.path().join("worker.sh").display().to_string()];
    let refused = run_worker(json_backend, args, dir.path(), Completion::Wait).await?;
    assert!(
        refused.agents["agents"][0]["agent_status"]["errored"]
            .as_str()
            .expect("JSON envelope refusal")
            .contains("exceeds max_input_bytes")
    );
    assert!(!received.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_code_worker_refuses_token_dense_instructions_below_byte_ceiling() -> Result<()> {
    let dir = TempDir::new()?;
    std::fs::create_dir(dir.path().join(".git"))?;
    std::fs::write(dir.path().join("AGENTS.md"), " a".repeat(/*n*/ 10_000))?;
    let received = dir.path().join("received");
    let backend = stub_cli(
        &dir,
        "worker.sh",
        &format!("touch '{}'\necho done\n", received.display()),
    );
    let mut args = worker_arguments(json!({"type":"code", "paths":["widget.rs"]}));
    args["bounded_worker"]["max_input_bytes"] = json!(32_768);
    let refused = run_worker(backend, args, dir.path(), Completion::Wait).await?;
    assert!(
        refused.spawn.contains("exceeds 10000 o200k_base tokens"),
        "{}",
        refused.spawn
    );
    assert_eq!(refused.agents, json!({"agents":[]}));
    assert!(!received.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_code_worker_counts_final_raw_and_json_context_tokens() -> Result<()> {
    let dir = TempDir::new()?;
    std::fs::create_dir(dir.path().join(".git"))?;
    std::fs::write(dir.path().join("AGENTS.md"), "Keep the widget blue.")?;
    let received = dir.path().join("received");
    let backend = stub_cli(
        &dir,
        "worker.sh",
        &format!("printf '%s' \"$*\" > '{}'\necho done\n", received.display()),
    );
    let mut args = worker_arguments(json!({"type":"code", "paths":["widget.rs"]}));
    args["bounded_worker"]["max_input_bytes"] = json!(32_768);
    run_worker(backend.clone(), args.clone(), dir.path(), Completion::Wait).await?;
    let baseline = std::fs::read_to_string(&received)?;
    let prefix = baseline.strip_suffix(AGENT_MESSAGE).expect("complete task");
    let tokenizer = tiktoken_rs::o200k_base()?;
    let mut task = " a".repeat(10_000 - tokenizer.count_ordinary(prefix));
    while tokenizer.count_ordinary(&format!("{prefix}{task}")) < 10_000 {
        task.push_str(" a");
    }
    let at_limit = format!("{prefix}{task}");
    assert_eq!(tokenizer.count_ordinary(&at_limit), 10_000);
    assert!(at_limit.len() < 32_768);
    std::fs::remove_file(&received)?;
    args["message"] = json!(task);
    let accepted = run_worker(backend.clone(), args.clone(), dir.path(), Completion::Wait).await?;
    assert_eq!(std::fs::read_to_string(&received)?, at_limit);
    assert_eq!(
        accepted.agents["agents"][0]["agent_status"],
        json!({"completed":"done"})
    );
    std::fs::remove_file(&received)?;

    for protocol in [
        ExternalCommandProtocol::RawCli,
        ExternalCommandProtocol::Json,
    ] {
        let mut limited_backend = backend.clone();
        limited_backend.protocol = protocol;
        let mut limited = args.clone();
        if protocol == ExternalCommandProtocol::Json {
            limited_backend.command = "/bin/sh".to_string();
            limited_backend.args = vec![dir.path().join("worker.sh").display().to_string()];
        } else {
            limited["message"] = json!(format!("{task} a"));
        }
        let refused = run_worker(limited_backend, limited, dir.path(), Completion::Wait).await?;
        // Registration proves the assembled item passed; only the final envelope exceeds the cap.
        assert!(
            refused.agents["agents"][0]["agent_status"]["errored"]
                .as_str()
                .expect("final context refusal")
                .contains("exceeds 10000 o200k_base tokens")
        );
        assert!(!received.exists());
    }
    Ok(())
}
