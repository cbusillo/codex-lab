use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_shell_command_sse_response;
use app_test_support::to_response;
use codex_app_server_protocol::AutoReviewDetailKind;
use codex_app_server_protocol::AutoReviewFindingDetailReadParams;
use codex_app_server_protocol::AutoReviewFindingDetailReadResponse;
use codex_app_server_protocol::AutoReviewFreshness as ApiAutoReviewFreshness;
use codex_app_server_protocol::AutoReviewRunSource as ApiAutoReviewRunSource;
use codex_app_server_protocol::AutoReviewSummaryReadParams;
use codex_app_server_protocol::AutoReviewSummaryReadResponse;
use codex_app_server_protocol::BackgroundAutoReviewControlAction;
use codex_app_server_protocol::BackgroundAutoReviewControlParams;
use codex_app_server_protocol::BackgroundAutoReviewControlReason;
use codex_app_server_protocol::BackgroundAutoReviewControlResponse;
use codex_app_server_protocol::BackgroundAutoReviewStatus;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ReviewDelivery;
use codex_app_server_protocol::ReviewStartParams;
use codex_app_server_protocol::ReviewStartResponse;
use codex_app_server_protocol::ReviewStartTarget;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadStartedNotification;
use codex_app_server_protocol::ThreadStatusChangedNotification;
use codex_app_server_protocol::TurnEnvironmentParams;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::ReviewCoordination;
use codex_auto_review::SCHEMA_VERSION;
use codex_git_utils::collect_git_info;
use codex_git_utils::get_git_repo_root;
use codex_git_utils::get_worktree_diff_fingerprint;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget as CoreReviewTarget;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;

#[tokio::test]
async fn review_start_runs_review_turn_and_emits_code_review_item() -> Result<()> {
    let review_payload = json!({
        "findings": [
            {
                "title": "Prefer Stylize helpers",
                "body": "Use .dim()/.bold() chaining instead of manual Style.",
                "confidence_score": 0.9,
                "priority": 1,
                "code_location": {
                    "absolute_file_path": "/tmp/file.rs",
                    "line_range": {"start": 10, "end": 20}
                }
            }
        ],
        "overall_correctness": "good",
        "overall_explanation": "Looks solid overall with minor polish suggested.",
        "overall_confidence_score": 0.75
    })
    .to_string();
    let server = create_mock_responses_server_repeating_assistant(&review_payload).await;

    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_id = start_default_thread(&mut mcp).await?;

    let review_req = mcp
        .send_review_start_request(ReviewStartParams {
            thread_id: thread_id.clone(),
            delivery: Some(ReviewDelivery::Inline),
            target: ReviewStartTarget::Commit {
                sha: "1234567deadbeef".to_string(),
                title: Some("Tidy UI colors".to_string()),
            },
        })
        .await?;
    let review_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(review_req)),
    )
    .await??;
    let ReviewStartResponse {
        turn,
        review_thread_id,
    } = to_response::<ReviewStartResponse>(review_resp)?;
    assert_eq!(review_thread_id, thread_id.clone());
    let turn_id = turn.id.clone();
    assert_eq!(turn.status, TurnStatus::InProgress);
    assert_eq!(turn.items_view, TurnItemsView::NotLoaded);
    assert_eq!(
        turn.items,
        vec![ThreadItem::UserMessage {
            id: turn_id.clone(),
            client_id: None,
            content: vec![V2UserInput::Text {
                text: "commit 1234567: Tidy UI colors".to_string(),
                text_elements: Vec::new(),
            }],
        }]
    );

    // Confirm we see the EnteredReviewMode marker on the main thread.
    let mut saw_entered_review_mode = false;
    for _ in 0..10 {
        let item_started: JSONRPCNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_message("item/started"),
        )
        .await??;
        let started: ItemStartedNotification =
            serde_json::from_value(item_started.params.expect("params must be present"))?;
        match started.item {
            ThreadItem::EnteredReviewMode { id, review } => {
                assert_eq!(id, turn_id);
                assert_eq!(review, "commit 1234567: Tidy UI colors");
                saw_entered_review_mode = true;
                break;
            }
            _ => continue,
        }
    }
    assert!(
        saw_entered_review_mode,
        "did not observe enteredReviewMode item"
    );

    // Confirm we see the ExitedReviewMode marker (with review text)
    // on the same turn. Ignore any other items the stream surfaces.
    let mut review_body: Option<String> = None;
    for _ in 0..10 {
        let review_notif: JSONRPCNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_message("item/completed"),
        )
        .await??;
        let completed: ItemCompletedNotification =
            serde_json::from_value(review_notif.params.expect("params must be present"))?;
        match completed.item {
            ThreadItem::ExitedReviewMode { id, review } => {
                assert_eq!(id, turn_id);
                review_body = Some(review);
                break;
            }
            _ => continue,
        }
    }

    let review = review_body.expect("did not observe a code review item");
    assert!(review.contains("Prefer Stylize helpers"));
    assert!(review.contains("/tmp/file.rs:10-20"));

    Ok(())
}

#[tokio::test]
#[ignore = "TODO(owenlin0): flaky"]
async fn review_start_exec_approval_item_id_matches_command_execution_item() -> Result<()> {
    let responses = vec![
        create_shell_command_sse_response(
            vec![
                "git".to_string(),
                "rev-parse".to_string(),
                "HEAD".to_string(),
            ],
            /*workdir*/ None,
            Some(5000),
            "review-call-1",
        )?,
        create_final_assistant_message_sse_response("done")?,
    ];
    let server = create_mock_responses_server_sequence(responses).await;

    let codex_home = TempDir::new()?;
    create_config_toml_with_approval_policy(codex_home.path(), &server.uri(), "untrusted")?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_id = start_default_thread(&mut mcp).await?;

    let review_req = mcp
        .send_review_start_request(ReviewStartParams {
            thread_id,
            delivery: Some(ReviewDelivery::Inline),
            target: ReviewStartTarget::Commit {
                sha: "1234567deadbeef".to_string(),
                title: Some("Check review approvals".to_string()),
            },
        })
        .await?;
    let review_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(review_req)),
    )
    .await??;
    let ReviewStartResponse { turn, .. } = to_response::<ReviewStartResponse>(review_resp)?;
    let turn_id = turn.id.clone();
    assert_eq!(turn.items_view, TurnItemsView::NotLoaded);
    assert_eq!(
        turn.items,
        vec![ThreadItem::UserMessage {
            id: turn_id.clone(),
            client_id: None,
            content: vec![V2UserInput::Text {
                text: "commit 1234567: Check review approvals".to_string(),
                text_elements: Vec::new(),
            }],
        }]
    );

    let server_req = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::CommandExecutionRequestApproval { request_id, params } = server_req else {
        panic!("expected CommandExecutionRequestApproval request");
    };
    assert_eq!(params.item_id, "review-call-1");
    assert_eq!(params.turn_id, turn_id);

    let mut command_item_id = None;
    for _ in 0..10 {
        let item_started: JSONRPCNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_message("item/started"),
        )
        .await??;
        let started: ItemStartedNotification =
            serde_json::from_value(item_started.params.expect("params must be present"))?;
        if let ThreadItem::CommandExecution { id, .. } = started.item {
            command_item_id = Some(id);
            break;
        }
    }
    let command_item_id = command_item_id.expect("did not observe command execution item");
    assert_eq!(command_item_id, params.item_id);

    mcp.send_response(
        request_id,
        serde_json::json!({ "decision": codex_protocol::protocol::ReviewDecision::Approved }),
    )
    .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test]
async fn review_start_rejects_empty_base_branch() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_review_start_request(ReviewStartParams {
            thread_id,
            delivery: Some(ReviewDelivery::Inline),
            target: ReviewStartTarget::BaseBranch {
                branch: "   ".to_string(),
            },
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("branch must not be empty"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn review_start_rejects_current_turn_diff_target() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_raw_request(
            "review/start",
            Some(json!({
                "threadId": thread_id,
                "delivery": "inline",
                "target": {
                    "type": "currentTurnDiff",
                    "fingerprint": "sha256:turn"
                }
            })),
        )
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("unknown variant"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[cfg_attr(target_os = "windows", ignore = "flaky on windows CI")]
#[tokio::test]
async fn review_start_with_detached_delivery_returns_new_thread_id() -> Result<()> {
    let review_payload = json!({
        "findings": [],
        "overall_correctness": "ok",
        "overall_explanation": "detached review",
        "overall_confidence_score": 0.5
    })
    .to_string();
    let server = create_mock_responses_server_repeating_assistant(&review_payload).await;

    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let thread_id = start_default_thread(&mut mcp).await?;
    materialize_thread_rollout(&mut mcp, &thread_id).await?;

    let review_req = mcp
        .send_review_start_request(ReviewStartParams {
            thread_id: thread_id.clone(),
            delivery: Some(ReviewDelivery::Detached),
            target: ReviewStartTarget::Custom {
                instructions: "detached review".to_string(),
            },
        })
        .await?;
    let review_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(review_req)),
    )
    .await??;
    let ReviewStartResponse {
        turn,
        review_thread_id,
    } = to_response::<ReviewStartResponse>(review_resp)?;

    assert_eq!(turn.status, TurnStatus::InProgress);
    assert_eq!(turn.items_view, TurnItemsView::NotLoaded);
    assert_eq!(
        turn.items,
        vec![ThreadItem::UserMessage {
            id: turn.id.clone(),
            client_id: None,
            content: vec![V2UserInput::Text {
                text: "detached review".to_string(),
                text_elements: Vec::new(),
            }],
        }]
    );
    assert_ne!(
        review_thread_id, thread_id,
        "detached review should run on a different thread"
    );

    let deadline = tokio::time::Instant::now() + DEFAULT_READ_TIMEOUT;
    let notification = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let message = timeout(remaining, mcp.read_next_message()).await??;
        let JSONRPCMessage::Notification(notification) = message else {
            continue;
        };
        if notification.method == "thread/status/changed" {
            let status_changed: ThreadStatusChangedNotification =
                serde_json::from_value(notification.params.expect("params must be present"))?;
            if status_changed.thread_id == review_thread_id {
                anyhow::bail!(
                    "detached review threads should be introduced without a preceding thread/status/changed"
                );
            }
            continue;
        }
        if notification.method == "thread/started" {
            break notification;
        }
    };
    let started: ThreadStartedNotification =
        serde_json::from_value(notification.params.expect("params must be present"))?;
    assert_eq!(started.thread.id, review_thread_id);
    assert_eq!(started.thread.session_id, review_thread_id);

    let _completed = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let runs = load_auto_review_runs(codex_home.path())?;
    assert_eq!(runs.len(), 1, "expected detached review to persist one run");
    let run = runs.into_iter().next().expect("one run");
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    assert_eq!(run.source, AutoReviewRunSource::Manual);
    assert_eq!(run.run_id, turn.id);
    assert_eq!(run.finding_count, 0);
    assert!(run.finding_digests.is_empty());

    Ok(())
}

#[tokio::test]
async fn review_start_rejects_empty_commit_sha() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_review_start_request(ReviewStartParams {
            thread_id,
            delivery: Some(ReviewDelivery::Inline),
            target: ReviewStartTarget::Commit {
                sha: "\t".to_string(),
                title: None,
            },
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("sha must not be empty"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn review_start_rejects_empty_custom_instructions() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_review_start_request(ReviewStartParams {
            thread_id,
            delivery: Some(ReviewDelivery::Inline),
            target: ReviewStartTarget::Custom {
                instructions: "\n\n".to_string(),
            },
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error
            .error
            .message
            .contains("instructions must not be empty"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn background_auto_review_control_rejects_empty_run_id() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_background_auto_review_control_request(BackgroundAutoReviewControlParams {
            thread_id,
            run_id: "  \t  ".to_string(),
            action: BackgroundAutoReviewControlAction::Cancel,
            reason: BackgroundAutoReviewControlReason::UserRequested,
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("runId must not be empty"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn background_auto_review_control_unknown_run_is_acknowledged() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_background_auto_review_control_request(BackgroundAutoReviewControlParams {
            thread_id,
            run_id: "missing-run".to_string(),
            action: BackgroundAutoReviewControlAction::Supersede,
            reason: BackgroundAutoReviewControlReason::SupersededByRun {
                run_id: "replacement-run".to_string(),
            },
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _response: BackgroundAutoReviewControlResponse =
        to_response::<BackgroundAutoReviewControlResponse>(response)?;

    Ok(())
}

#[tokio::test]
async fn auto_review_summary_read_returns_empty_state() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_auto_review_summary_read_request(AutoReviewSummaryReadParams { thread_id })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let summary = to_response::<AutoReviewSummaryReadResponse>(response)?;
    assert_eq!(summary.latest, None);
    assert_eq!(summary.current, None);
    assert_eq!(summary.status_counts, Vec::new());

    Ok(())
}

#[tokio::test]
async fn auto_review_summary_read_returns_current_summary_and_counts() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (run, output) = sample_auto_review_run("run_summary", &thread_cwd, "Stored body");
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_summary_read_request(AutoReviewSummaryReadParams { thread_id })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let summary = to_response::<AutoReviewSummaryReadResponse>(response)?;
    let current = summary.current.expect("current run summary");
    assert_eq!(current.run_id, "run_summary");
    assert_eq!(current.status, BackgroundAutoReviewStatus::Completed);
    assert_eq!(current.source, ApiAutoReviewRunSource::Background);
    assert_eq!(current.freshness, ApiAutoReviewFreshness::Current);
    assert_eq!(current.rendered_findings, 1);
    assert_eq!(current.omitted_findings, 0);
    assert!(current.content.contains("f1"));
    assert_eq!(
        summary.latest.as_ref().map(|run| run.run_id.as_str()),
        Some("run_summary")
    );
    assert_eq!(summary.status_counts.len(), 1);
    assert_eq!(summary.status_counts[0].count, 1);
    assert_eq!(
        summary.diagnostics.as_ref().map(|diagnostics| (
            diagnostics.recent_runs,
            diagnostics.terminal_runs,
            diagnostics.suppressed_stale_runs
        )),
        Some((1, 1, 0))
    );

    Ok(())
}

#[tokio::test]
async fn auto_review_summary_read_returns_duplicate_skip_diagnostics() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (mut run, output) = sample_auto_review_run("run_duplicate_skip", &thread_cwd, "");
    run.status = AutoReviewRunStatus::Skipped;
    run.freshness = codex_auto_review::AutoReviewRunFreshness::Superseded;
    run.superseded_by = Some("existing-run".to_string());
    run.cancel_reason = Some("duplicate_auto_review_scope".to_string());
    run.error_summary = Some("equivalent background auto review already exists".to_string());
    run.finding_count = 0;
    run.omitted_finding_digest_count = 0;
    run.finding_digests.clear();
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_summary_read_request(AutoReviewSummaryReadParams { thread_id })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let summary = to_response::<AutoReviewSummaryReadResponse>(response)?;
    let diagnostics = summary.diagnostics.expect("diagnostics");
    assert_eq!(diagnostics.recent_runs, 1);
    assert_eq!(diagnostics.skipped_runs, 1);
    assert_eq!(diagnostics.duplicate_skipped_runs, 1);
    assert_eq!(
        diagnostics.compact,
        "recent_runs=1 in_flight=0 terminal=1 skipped=1 duplicate_skipped=1"
    );

    Ok(())
}

#[tokio::test]
async fn auto_review_summary_read_treats_current_turn_diff_as_current() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    let repo = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;
    init_git_repo(repo.path())?;
    std::fs::write(repo.path().join("tracked.txt"), "base\nchange\n")?;
    let thread_cwd = std::fs::canonicalize(repo.path())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_thread_with_cwd(&mut mcp, &thread_cwd).await?;
    let active_target = auto_review_target_for_cwd(codex_home.path(), &thread_cwd).await;
    let (mut run, output) = sample_auto_review_run("run_turn_diff", &thread_cwd, "Stored body");
    run.review_target = CoreReviewTarget::CurrentTurnDiff {
        fingerprint: "sha256:synthetic-turn-diff".to_string(),
    };
    run.target = active_target;
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_summary_read_request(AutoReviewSummaryReadParams { thread_id })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let summary = to_response::<AutoReviewSummaryReadResponse>(response)?;
    let current = summary.current.expect("current run summary");
    assert_eq!(current.run_id, "run_turn_diff");
    assert_eq!(current.freshness, ApiAutoReviewFreshness::Current);
    assert_eq!(current.rendered_findings, 1);
    assert!(current.content.contains("f1"));
    assert!(summary.status_counts.iter().any(|count| {
        count.freshness == ApiAutoReviewFreshness::Current && count.target_matches
    }));

    Ok(())
}

#[tokio::test]
async fn auto_review_summary_read_suppresses_stale_findings_by_default() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (mut current_run, current_output) =
        sample_auto_review_run("run_current", &thread_cwd, "Current body");
    current_run.completed_at_unix_secs = Some(2);
    let (mut stale_run, stale_output) =
        sample_auto_review_run("run_stale", &thread_cwd, "Stale body");
    stale_run.target.head_sha = Some("old-head".to_string());
    stale_run.started_at_unix_secs = 10;
    stale_run.completed_at_unix_secs = Some(11);
    save_auto_review_fixture(
        codex_home.path(),
        &thread_cwd,
        &current_run,
        &current_output,
    )?;
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &stale_run, &stale_output)?;

    let request_id = mcp
        .send_auto_review_summary_read_request(AutoReviewSummaryReadParams { thread_id })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let summary = to_response::<AutoReviewSummaryReadResponse>(response)?;
    let current = summary.current.expect("current run summary");
    assert_eq!(current.run_id, "run_current");
    assert!(current.content.contains("f1"));

    let latest = summary.latest.expect("latest run summary");
    assert_eq!(latest.run_id, "run_stale");
    assert_eq!(latest.freshness, ApiAutoReviewFreshness::Stale);
    assert_eq!(latest.rendered_findings, 0);
    assert!(!latest.content.contains("Stale body"));
    assert!(summary.status_counts.iter().any(|count| {
        count.freshness == ApiAutoReviewFreshness::Current && count.target_matches
    }));
    assert!(summary.status_counts.iter().any(|count| {
        count.freshness == ApiAutoReviewFreshness::Stale && !count.target_matches
    }));
    assert_eq!(
        summary
            .status_counts
            .iter()
            .map(|count| count.count)
            .sum::<usize>(),
        2
    );

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_returns_bounded_detail() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (run, output) = sample_auto_review_run(
        "run_detail",
        &thread_cwd,
        &"Use the existing bounded detail store instead of embedding the whole finding. ".repeat(8),
    );
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: Some("f1".to_string()),
            max_bytes: Some(180),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let detail = to_response::<AutoReviewFindingDetailReadResponse>(response)?;
    assert_eq!(detail.run_id, "run_detail");
    assert_eq!(detail.detail_kind, AutoReviewDetailKind::Finding);
    assert_eq!(detail.finding_id.as_deref(), Some("f1"));
    assert_eq!(detail.finding_count, 1);
    assert_eq!(detail.omitted_findings, 0);
    assert_eq!(detail.max_bytes, 180);
    assert!(detail.truncated);
    assert!(detail.bytes <= 180);
    assert!(detail.original_bytes > detail.bytes);
    assert!(detail.content.contains("Prefer bounded details"));
    assert!(detail.content.contains("body:"));
    assert!(!detail.content.contains("code_location"));

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_returns_bounded_run_detail() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (run, output) = sample_auto_review_run_with_findings(
        "run_detail_all",
        &thread_cwd,
        (1..=12)
            .map(|index| (format!("Finding {index}"), format!("Stored body {index}")))
            .collect(),
    );
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail_all".to_string(),
            finding_id: None,
            max_bytes: Some(4096),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let detail = to_response::<AutoReviewFindingDetailReadResponse>(response)?;
    assert_eq!(detail.run_id, "run_detail_all");
    assert_eq!(detail.detail_kind, AutoReviewDetailKind::Run);
    assert_eq!(detail.finding_id, None);
    assert_eq!(detail.finding_count, 12);
    assert_eq!(detail.omitted_findings, 2);
    assert!(detail.truncated);
    assert!(detail.content.contains("overall_correctness"));
    assert!(detail.content.contains("finding_id=f1"));
    assert!(detail.content.contains("finding_id=f10"));
    assert!(!detail.content.contains("finding_id=f11"));
    assert!(detail.content.contains("request a specific findingId"));

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_uses_selected_environment_cwd() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    let environment_cwd = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            cwd: Some(codex_home.path().to_string_lossy().into_owned()),
            environments: Some(vec![TurnEnvironmentParams {
                environment_id: "local".to_string(),
                cwd: environment_cwd.path().to_path_buf().try_into()?,
            }]),
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/started"),
    )
    .await??;

    let (run, output) = sample_auto_review_run(
        "run_environment_detail",
        environment_cwd.path(),
        "Stored environment body",
    );
    save_auto_review_fixture(codex_home.path(), environment_cwd.path(), &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id: thread.id,
            run_id: "run_environment_detail".to_string(),
            finding_id: Some("f1".to_string()),
            max_bytes: Some(1024),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let detail = to_response::<AutoReviewFindingDetailReadResponse>(response)?;
    assert_eq!(detail.run_id, "run_environment_detail");
    assert_eq!(detail.finding_id.as_deref(), Some("f1"));

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_allows_omitted_summary_findings() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (run, output) = sample_auto_review_run_with_findings(
        "run_omitted_detail",
        &thread_cwd,
        (1..=21)
            .map(|index| (format!("Finding {index}"), format!("Stored body {index}")))
            .collect(),
    );
    assert_eq!(run.finding_count, 21);
    assert_eq!(run.finding_digests.len(), 20);
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_omitted_detail".to_string(),
            finding_id: Some("f21".to_string()),
            max_bytes: Some(4096),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;

    let detail = to_response::<AutoReviewFindingDetailReadResponse>(response)?;
    assert_eq!(detail.finding_id.as_deref(), Some("f21"));
    assert!(detail.content.contains("Stored body 21"));

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_rejects_empty_finding_id_when_provided() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: Some(" \t ".to_string()),
            max_bytes: Some(180),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error
            .error
            .message
            .contains("findingId must not be empty when provided"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_rejects_unknown_finding_id() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (run, output) = sample_auto_review_run("run_detail", &thread_cwd, "Stored body");
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: Some("missing".to_string()),
            max_bytes: Some(180),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("auto review detail not found"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_rejects_wrong_review_target() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let thread_id = start_thread_with_cwd(&mut mcp, &thread_cwd).await?;
    let (mut run, output) = sample_auto_review_run("run_wrong_target", &thread_cwd, "Stored body");
    run.target = auto_review_target_for_cwd(codex_home.path(), &thread_cwd).await;
    run.review_target = CoreReviewTarget::Custom {
        instructions: "review a different target".to_string(),
    };
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_wrong_target".to_string(),
            finding_id: Some("f1".to_string()),
            max_bytes: Some(180),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("auto review detail not found"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_rejects_stale_run() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (run, output) = sample_auto_review_run(
        "run_detail",
        &thread_cwd.join("other-worktree"),
        "Stored body",
    );
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: Some("f1".to_string()),
            max_bytes: Some(180),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("auto review detail not found"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_rejects_stale_run_detail() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let thread_id = start_default_thread(&mut mcp).await?;
    let thread_cwd = std::fs::canonicalize(codex_home.path())?;
    let (run, output) = sample_auto_review_run(
        "run_detail",
        &thread_cwd.join("other-worktree"),
        "Stored body",
    );
    save_auto_review_fixture(codex_home.path(), &thread_cwd, &run, &output)?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: None,
            max_bytes: Some(180),
        })
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert!(
        error.error.message.contains("auto review detail not found"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

async fn start_default_thread(mcp: &mut TestAppServer) -> Result<String> {
    start_thread(mcp, /*cwd*/ None).await
}

async fn start_thread_with_cwd(mcp: &mut TestAppServer, cwd: &std::path::Path) -> Result<String> {
    start_thread(mcp, Some(cwd.to_string_lossy().into_owned())).await
}

async fn start_thread(mcp: &mut TestAppServer, cwd: Option<String>) -> Result<String> {
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
            cwd,
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let thread_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(thread_req)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(thread_resp)?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/started"),
    )
    .await??;
    Ok(thread.id)
}

fn sample_auto_review_run(
    run_id: &str,
    worktree_path: &std::path::Path,
    body: &str,
) -> (AutoReviewRun, ReviewOutputEvent) {
    sample_auto_review_run_with_findings(
        run_id,
        worktree_path,
        vec![("Prefer bounded details".to_string(), body.to_string())],
    )
}

fn sample_auto_review_run_with_findings(
    run_id: &str,
    worktree_path: &std::path::Path,
    findings: Vec<(String, String)>,
) -> (AutoReviewRun, ReviewOutputEvent) {
    let output = ReviewOutputEvent {
        findings: findings
            .into_iter()
            .zip(1_u32..)
            .map(|((title, body), line)| ReviewFinding {
                title,
                body,
                confidence_score: 0.92,
                priority: 1,
                code_location: ReviewCodeLocation {
                    absolute_file_path: PathBuf::from("/repo/src/lib.rs"),
                    line_range: ReviewLineRange {
                        start: line,
                        end: line,
                    },
                },
            })
            .collect(),
        overall_correctness: "patch is incorrect".to_string(),
        overall_explanation: "summary".to_string(),
        overall_confidence_score: 0.8,
    };
    let finding_digests = codex_auto_review::finding_digests(&output);
    let run = AutoReviewRun {
        schema_version: SCHEMA_VERSION,
        run_id: run_id.to_string(),
        status: AutoReviewRunStatus::Completed,
        freshness: codex_auto_review::AutoReviewRunFreshness::Current,
        source: AutoReviewRunSource::Background,
        target: AutoReviewRunTarget {
            branch: None,
            head_sha: None,
            base_sha: None,
            worktree_path: Some(worktree_path.to_path_buf()),
            snapshot_epoch: None,
            snapshot_commit: None,
            head_at_launch: None,
            worktree_diff_fingerprint: None,
        },
        review_target: CoreReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1,
        completed_at_unix_secs: Some(2),
        model: Some("review-model".to_string()),
        reasoning_effort: None,
        prompt_token_estimate: None,
        token_count: None,
        saved_token_estimate: None,
        superseded_by: None,
        cancel_reason: None,
        error_summary: None,
        finding_count: output.findings.len(),
        omitted_finding_digest_count: output.findings.len().saturating_sub(finding_digests.len()),
        finding_digests,
    };
    (run, output)
}

fn save_auto_review_fixture(
    codex_home: &std::path::Path,
    store_scope: &std::path::Path,
    run: &AutoReviewRun,
    output: &ReviewOutputEvent,
) -> Result<()> {
    let store = AutoReviewStore::for_scope(codex_home, store_scope);
    store.save_run(run)?;
    store.save_output(&run.run_id, output)?;
    Ok(())
}

async fn auto_review_target_for_cwd(
    codex_home: &std::path::Path,
    cwd: &std::path::Path,
) -> AutoReviewRunTarget {
    let git_info = collect_git_info(cwd).await;
    let repo_root = get_git_repo_root(cwd);
    let worktree_path = repo_root.or_else(|| Some(cwd.to_path_buf()));
    let snapshot_epoch = worktree_path.as_ref().and_then(|scope| {
        ReviewCoordination::for_scope(codex_home, scope)
            .current_snapshot_epoch()
            .ok()
            .filter(|epoch| *epoch > 0)
    });
    AutoReviewRunTarget {
        branch: git_info.as_ref().and_then(|git| git.branch.clone()),
        head_sha: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        base_sha: None,
        worktree_path,
        snapshot_epoch,
        snapshot_commit: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        head_at_launch: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        worktree_diff_fingerprint: get_worktree_diff_fingerprint(cwd).await,
    }
}

fn init_git_repo(repo_path: &std::path::Path) -> Result<()> {
    run_git(repo_path, &["init", "-b", "main"])?;
    run_git(repo_path, &["config", "user.email", "test@example.com"])?;
    run_git(repo_path, &["config", "user.name", "Test User"])?;
    std::fs::write(repo_path.join("tracked.txt"), "base\n")?;
    run_git(repo_path, &["add", "tracked.txt"])?;
    run_git(repo_path, &["commit", "-m", "initial"])?;
    Ok(())
}

fn run_git(repo_path: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {:?} failed: stdout={:?} stderr={:?}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn load_auto_review_runs(codex_home: &std::path::Path) -> Result<Vec<AutoReviewRun>> {
    let review_dir = codex_home.join("state/review");
    if !review_dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(&review_dir)? {
        let store_root = entry?.path().join("auto-review");
        runs.extend(AutoReviewStore::from_store_root(store_root).list_runs()?);
    }
    runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    Ok(runs)
}

async fn materialize_thread_rollout(mcp: &mut TestAppServer, thread_id: &str) -> Result<()> {
    let turn_req = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread_id.to_string(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "materialize rollout".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_req)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    Ok(())
}

fn create_config_toml(codex_home: &std::path::Path, server_uri: &str) -> std::io::Result<()> {
    create_config_toml_with_approval_policy(codex_home, server_uri, "never")
}

fn create_config_toml_with_approval_policy(
    codex_home: &std::path::Path,
    server_uri: &str,
    approval_policy: &str,
) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "{approval_policy}"
sandbox_mode = "read-only"

model_provider = "mock_provider"

[features]
shell_snapshot = false

[model_providers.mock_provider]
name = "Mock provider"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}
