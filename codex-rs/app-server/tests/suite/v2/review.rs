use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_shell_command_sse_response;
use app_test_support::to_response;
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
use codex_app_server_protocol::ReviewTarget;
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
use codex_auto_review::AutoReviewFindingRecord;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::SCHEMA_VERSION;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
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
            target: ReviewTarget::Commit {
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
            target: ReviewTarget::Commit {
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
            target: ReviewTarget::BaseBranch {
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
            target: ReviewTarget::Custom {
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
    assert_eq!(run.findings.len(), 0);

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
            target: ReviewTarget::Commit {
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
            target: ReviewTarget::Custom {
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
    AutoReviewStore::for_scope(codex_home.path(), &thread_cwd).save_run(
        &sample_auto_review_run("run_summary", "finding_summary", &thread_cwd, "Stored body"),
    )?;

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
    assert!(current.content.contains("finding_summary"));
    assert_eq!(
        summary.latest.as_ref().map(|run| run.run_id.as_str()),
        Some("run_summary")
    );
    assert_eq!(summary.status_counts.len(), 1);
    assert_eq!(summary.status_counts[0].count, 1);

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
    let mut current_run = sample_auto_review_run(
        "run_current",
        "finding_current",
        &thread_cwd,
        "Current body",
    );
    current_run.completed_at_unix_secs = Some(2);
    let mut stale_run =
        sample_auto_review_run("run_stale", "finding_stale", &thread_cwd, "Stale body");
    stale_run.target.head_sha = Some("old-head".to_string());
    stale_run.started_at_unix_secs = 10;
    stale_run.completed_at_unix_secs = Some(11);
    let store = AutoReviewStore::for_scope(codex_home.path(), &thread_cwd);
    store.save_run(&current_run)?;
    store.save_run(&stale_run)?;

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
    assert!(current.content.contains("finding_current"));

    let latest = summary.latest.expect("latest run summary");
    assert_eq!(latest.run_id, "run_stale");
    assert_eq!(latest.freshness, ApiAutoReviewFreshness::Stale);
    assert_eq!(latest.rendered_findings, 0);
    assert!(!latest.content.contains("finding_stale"));
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
    AutoReviewStore::for_scope(codex_home.path(), &thread_cwd).save_run(
        &sample_auto_review_run(
            "run_detail",
            "finding_detail",
            &thread_cwd,
            &"Use the existing bounded detail store instead of embedding the whole finding. "
                .repeat(8),
        ),
    )?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: "finding_detail".to_string(),
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
    assert_eq!(detail.finding_id, "finding_detail");
    assert_eq!(detail.max_bytes, 180);
    assert!(detail.truncated);
    assert!(detail.bytes <= 180);
    assert!(detail.original_bytes > detail.bytes);
    assert!(detail.content.contains("finding_detail"));

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

    AutoReviewStore::for_scope(codex_home.path(), environment_cwd.path()).save_run(
        &sample_auto_review_run(
            "run_environment_detail",
            "environment_finding",
            environment_cwd.path(),
            "Stored environment body",
        ),
    )?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id: thread.id,
            run_id: "run_environment_detail".to_string(),
            finding_id: "environment_finding".to_string(),
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
    assert_eq!(detail.finding_id, "environment_finding");

    Ok(())
}

#[tokio::test]
async fn auto_review_finding_detail_read_rejects_empty_finding_id() -> Result<()> {
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
            finding_id: " \t ".to_string(),
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
        error.error.message.contains("findingId must not be empty"),
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
    AutoReviewStore::for_scope(codex_home.path(), &thread_cwd).save_run(
        &sample_auto_review_run("run_detail", "finding_detail", &thread_cwd, "Stored body"),
    )?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: "missing".to_string(),
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
            .contains("auto review finding not found"),
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
    AutoReviewStore::for_scope(codex_home.path(), &thread_cwd).save_run(
        &sample_auto_review_run(
            "run_detail",
            "finding_detail",
            &thread_cwd.join("other-worktree"),
            "Stored body",
        ),
    )?;

    let request_id = mcp
        .send_auto_review_finding_detail_read_request(AutoReviewFindingDetailReadParams {
            thread_id,
            run_id: "run_detail".to_string(),
            finding_id: "finding_detail".to_string(),
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
            .contains("auto review finding not found"),
        "unexpected message: {}",
        error.error.message
    );

    Ok(())
}

async fn start_default_thread(mcp: &mut TestAppServer) -> Result<String> {
    let thread_req = mcp
        .send_thread_start_request(ThreadStartParams {
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
    finding_id: &str,
    worktree_path: &std::path::Path,
    body: &str,
) -> AutoReviewRun {
    AutoReviewRun {
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
            worktree_diff_fingerprint: None,
        },
        review_target: CoreReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1,
        completed_at_unix_secs: Some(2),
        model: Some("review-model".to_string()),
        superseded_by: None,
        cancel_reason: None,
        error_summary: None,
        findings: vec![AutoReviewFindingRecord {
            finding_id: finding_id.to_string(),
            finding: ReviewFinding {
                title: "Prefer bounded details".to_string(),
                body: body.to_string(),
                confidence_score: 0.92,
                priority: 1,
                code_location: ReviewCodeLocation {
                    absolute_file_path: PathBuf::from("/repo/src/lib.rs"),
                    line_range: ReviewLineRange { start: 12, end: 14 },
                },
            },
        }],
    }
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
