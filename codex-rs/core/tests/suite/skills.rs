#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use anyhow::Context;
use anyhow::Result;
use codex_exec_server::CreateDirectoryOptions;
use codex_exec_server::ExecutorFileSystem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::hooks::trust_discovered_hooks;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use std::fs;
use std::path::Path;
use std::sync::Arc;

const HOOK_ADDED_CONTEXT: &str = "hook-added context for skill boundary";

async fn write_repo_skill(
    cwd: AbsolutePathBuf,
    fs: Arc<dyn ExecutorFileSystem>,
    name: &str,
    description: &str,
    body: &str,
) -> Result<()> {
    let skill_dir = cwd.join(".agents").join("skills").join(name);
    fs.create_directory(
        &skill_dir,
        CreateDirectoryOptions { recursive: true },
        /*sandbox*/ None,
    )
    .await?;
    let contents = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    let path = skill_dir.join("SKILL.md");
    fs.write_file(&path, contents.into_bytes(), /*sandbox*/ None)
        .await?;
    Ok(())
}

fn shell_quote(path: &Path) -> String {
    let path = path.to_string_lossy();
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn write_user_prompt_submit_logging_hook(home: &Path) -> Result<()> {
    let script_path = home.join("user_prompt_submit_skill_boundary_hook.sh");
    let log_path = home.join("user_prompt_submit_skill_boundary_hook_log.jsonl");
    let context_json =
        serde_json::to_string(HOOK_ADDED_CONTEXT).context("serialize hook added context")?;
    fs::write(
        &script_path,
        format!(
            r#"#!/bin/sh
payload=$(cat)
printf '%s\n' "$payload" >> "$1"
printf '%s\n' '{{"hookSpecificOutput":{{"hookEventName":"UserPromptSubmit","additionalContext":{context_json}}}}}'
"#
        ),
    )
    .context("write user prompt submit hook script")?;

    let hooks = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [{
                "hooks": [{
                    "type": "command",
                    "command": format!(
                        "sh {} {}",
                        shell_quote(&script_path),
                        shell_quote(&log_path),
                    ),
                    "statusMessage": "recording user prompt submit hook",
                }]
            }]
        }
    });

    fs::write(home.join("hooks.json"), hooks.to_string()).context("write hooks.json")?;
    Ok(())
}

fn read_user_prompt_submit_hook_inputs(home: &Path) -> Result<Vec<serde_json::Value>> {
    let contents =
        fs::read_to_string(home.join("user_prompt_submit_skill_boundary_hook_log.jsonl"))
            .context("read user prompt submit hook log")?;
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("parse user prompt submit hook log line"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_turn_includes_skill_instructions_and_hook_context() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let skill_body = "skill body";
    let mut builder = test_codex()
        .with_workspace_setup(move |cwd, fs| async move {
            write_repo_skill(cwd, fs, "demo", "demo skill", skill_body).await
        })
        .with_pre_build_hook(|home| {
            if let Err(error) = write_user_prompt_submit_logging_hook(home) {
                panic!("failed to write user prompt submit hook test fixture: {error}");
            }
        })
        .with_config(trust_discovered_hooks);
    let test = builder.build_with_remote_env(&server).await?;

    let skill_path = test
        .config
        .cwd
        .join(".agents/skills/demo/SKILL.md")
        .canonicalize()
        .unwrap_or_else(|_| test.config.cwd.join(".agents/skills/demo/SKILL.md"))
        .to_path_buf();

    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let session_model = test.session_configured.model.clone();
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.config.cwd.as_path());
    test.codex
        .submit(Op::UserInput {
            items: vec![
                UserInput::Text {
                    text: "please use $demo".to_string(),
                    text_elements: Vec::new(),
                },
                UserInput::Skill {
                    name: "demo".to_string(),
                    path: skill_path.clone(),
                },
            ],
            environments: None,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
                cwd: Some(test.config.cwd.clone()),
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                    mode: codex_protocol::config_types::ModeKind::Default,
                    settings: codex_protocol::config_types::Settings {
                        model: session_model,
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            },
        })
        .await?;

    core_test_support::wait_for_event(test.codex.as_ref(), |event| {
        matches!(event, codex_protocol::protocol::EventMsg::TurnComplete(_))
    })
    .await;

    let request = mock.single_request();
    let user_texts = request.message_input_texts("user");
    let skill_path_str = skill_path.to_string_lossy();
    assert!(
        user_texts.iter().any(|text| {
            text.contains("<skill>\n<name>demo</name>")
                && text.contains("<path>")
                && text.contains(skill_body)
                && text.contains(skill_path_str.as_ref())
        }),
        "expected skill instructions in user input, got {user_texts:?}"
    );
    let developer_texts = request.message_input_texts("developer");
    assert!(
        developer_texts
            .iter()
            .any(|text| text.contains(HOOK_ADDED_CONTEXT)),
        "expected hook-added context in developer input, got {developer_texts:?}"
    );

    let hook_inputs = read_user_prompt_submit_hook_inputs(test.codex_home_path())?;
    assert_eq!(hook_inputs.len(), 1);
    let hook_prompt = hook_inputs[0]
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .expect("hook input should include prompt");
    assert_eq!(hook_prompt, "please use $demo");

    Ok(())
}
