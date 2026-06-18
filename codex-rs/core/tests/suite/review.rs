use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewStore;
use codex_core::CodexThread;
use codex_core::REVIEW_PROMPT;
use codex_core::config::Config;
use codex_core::review_format::render_review_output_text;
use codex_git_utils::get_worktree_diff_fingerprint;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::BackgroundAutoReviewControlAction;
use codex_protocol::protocol::BackgroundAutoReviewControlReason;
use codex_protocol::protocol::BackgroundAutoReviewStatus;
use codex_protocol::protocol::BackgroundAutoReviewStatusEvent;
use codex_protocol::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::user_input::UserInput;
use core_test_support::PathBufExt;
use core_test_support::responses;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_apply_patch_custom_tool_call;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::TestCodexHarness;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::oneshot;
use uuid::Uuid;
use wiremock::MockServer;

/// Verify that submitting `Op::Review` spawns a child task and emits
/// EnteredReviewMode -> ExitedReviewMode(None) -> TurnComplete
/// in that order when the model returns a structured review JSON payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_op_emits_lifecycle_and_review_output() {
    // Skip under Codex sandbox network restrictions.
    skip_if_no_network!();

    // Start mock Responses API server. Return a single assistant message whose
    // text is a JSON-encoded ReviewOutputEvent.
    let review_json = serde_json::json!({
        "findings": [
            {
                "title": "Prefer Stylize helpers",
                "body": "Use .dim()/.bold() chaining instead of manual Style where possible.",
                "confidence_score": 0.9,
                "priority": 1,
                "code_location": {
                    "absolute_file_path": "/tmp/file.rs",
                    "line_range": {"start": 10, "end": 20}
                }
            }
        ],
        "overall_correctness": "good",
        "overall_explanation": "All good with some improvements suggested.",
        "overall_confidence_score": 0.8
    })
    .to_string();
    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse(&review_json),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    // Submit review request.
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Please review my changes".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    // Verify lifecycle: Entered -> Exited(Some(review)) -> TurnComplete.
    let first_lifecycle = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::EnteredReviewMode(_)
                | EventMsg::ExitedReviewMode(_)
                | EventMsg::TurnComplete(_)
        )
    })
    .await;
    assert!(
        matches!(first_lifecycle, EventMsg::EnteredReviewMode(_)),
        "expected EnteredReviewMode before review terminal lifecycle event, got {first_lifecycle:?}"
    );
    let closed = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExitedReviewMode(_))).await;
    let review = match closed {
        EventMsg::ExitedReviewMode(ev) => ev
            .review_output
            .expect("expected ExitedReviewMode with Some(review_output)"),
        other => panic!("expected ExitedReviewMode(..), got {other:?}"),
    };

    // Deep compare full structure using PartialEq (floats are f32 on both sides).
    let expected = ReviewOutputEvent {
        findings: vec![ReviewFinding {
            title: "Prefer Stylize helpers".to_string(),
            body: "Use .dim()/.bold() chaining instead of manual Style where possible.".to_string(),
            confidence_score: 0.9,
            priority: 1,
            code_location: ReviewCodeLocation {
                absolute_file_path: PathBuf::from("/tmp/file.rs"),
                line_range: ReviewLineRange { start: 10, end: 20 },
            },
        }],
        overall_correctness: "good".to_string(),
        overall_explanation: "All good with some improvements suggested.".to_string(),
        overall_confidence_score: 0.8,
    };
    assert_eq!(expected, review);
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let path = codex.rollout_path().expect("rollout path");
    let text = std::fs::read_to_string(&path).expect("read rollout file");
    let parent_thread_id = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .find_map(|line| {
            let rollout_line: RolloutLine = serde_json::from_str(line).expect("rollout line");
            match rollout_line.item {
                RolloutItem::SessionMeta(session_meta) => Some(session_meta.meta.id.to_string()),
                _ => None,
            }
        })
        .expect("parent session meta");

    let request = request_log.single_request();
    assert_eq!(
        request.header("x-openai-subagent").as_deref(),
        Some("review")
    );
    let turn_metadata: serde_json::Value = serde_json::from_str(
        &request
            .header("x-codex-turn-metadata")
            .expect("review request turn metadata"),
    )
    .expect("review request turn metadata json");
    assert!(turn_metadata.get("forked_from_thread_id").is_none());
    assert_eq!(
        turn_metadata["parent_thread_id"].as_str(),
        Some(parent_thread_id.as_str())
    );

    // Also verify that a user message with the header and a formatted finding
    // was recorded back in the parent session's rollout.
    let mut saw_header = false;
    let mut saw_finding_line = false;
    let expected_assistant_text = render_review_output_text(&expected);
    let mut saw_assistant_plain = false;
    let mut saw_assistant_xml = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).expect("jsonl line");
        let rl: RolloutLine = serde_json::from_value(v).expect("rollout line");
        if let RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. }) = rl.item {
            if role == "user" {
                for c in content {
                    if let ContentItem::InputText { text } = c {
                        if text.contains("full review output from reviewer model") {
                            saw_header = true;
                        }
                        if text.contains("- Prefer Stylize helpers — /tmp/file.rs:10-20") {
                            saw_finding_line = true;
                        }
                    }
                }
            } else if role == "assistant" {
                for c in content {
                    if let ContentItem::OutputText { text } = c {
                        if text.contains("<user_action>") {
                            saw_assistant_xml = true;
                        }
                        if text == expected_assistant_text {
                            saw_assistant_plain = true;
                        }
                    }
                }
            }
        }
    }
    assert!(saw_header, "user header missing from rollout");
    assert!(
        saw_finding_line,
        "formatted finding line missing from rollout"
    );
    assert!(
        saw_assistant_plain,
        "assistant review output missing from rollout"
    );
    assert!(
        !saw_assistant_xml,
        "assistant review output contains user_action markup"
    );
    assert!(
        load_auto_review_runs(codex_home.path()).unwrap().is_empty(),
        "default review should not write an auto-review run"
    );

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// When the model returns plain text that is not JSON, ensure the child
/// lifecycle still occurs and the plain text is surfaced via
/// ExitedReviewMode(Some(..)) as the overall_explanation.
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_op_with_plain_text_emits_review_fallback() {
    skip_if_no_network!();

    let (server, _request_log) = start_responses_server_with_sse(
        assistant_message_sse("just plain text"),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Plain text review".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let closed = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExitedReviewMode(_))).await;
    let review = match closed {
        EventMsg::ExitedReviewMode(ev) => ev
            .review_output
            .expect("expected ExitedReviewMode with Some(review_output)"),
        other => panic!("expected ExitedReviewMode(..), got {other:?}"),
    };

    // Expect a structured fallback carrying the plain text.
    let expected = ReviewOutputEvent {
        overall_explanation: "just plain text".to_string(),
        ..Default::default()
    };
    assert_eq!(expected, review);
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let _codex_home_guard = codex_home;
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_op_with_persistence_writes_auto_review_run() {
    skip_if_no_network!();

    let review_json = serde_json::json!({
        "findings": [
            {
                "title": "Persist this finding",
                "body": "The persisted sidecar should keep the structured finding.",
                "confidence_score": 0.95,
                "priority": 2,
                "code_location": {
                    "absolute_file_path": "/tmp/persist.rs",
                    "line_range": {"start": 3, "end": 4}
                }
            }
        ],
        "overall_correctness": "patch is incorrect",
        "overall_explanation": "One persisted finding.",
        "overall_confidence_score": 0.85
    })
    .to_string();
    let (server, _request_log) = start_responses_server_with_sse(
        assistant_message_sse(&review_json),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |config| {
        config.model = Some("gpt-4.1".to_string());
        config.review_model = Some("gpt-5.4".to_string());
    })
    .await;

    let review_target = ReviewTarget::Custom {
        instructions: "persist this review".to_string(),
    };
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: review_target.clone(),
                user_facing_hint: None,
            },
            persistence: Some(ReviewPersistence::ManualAutoReview),
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _exited = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExitedReviewMode(_))).await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let run = load_single_auto_review_run(codex_home.path()).expect("load auto review run");
    let run_id = run.run_id.clone();

    assert_eq!(run.run_id, run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    assert_eq!(run.source, AutoReviewRunSource::Manual);
    assert_eq!(run.review_target, review_target);
    assert_eq!(run.model.as_deref(), Some("gpt-5.4"));
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(run.error_summary, None);
    assert_eq!(run.findings.len(), 1);
    assert_eq!(run.findings[0].finding_id, "f1");
    assert_eq!(run.findings[0].finding.title, "Persist this finding");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_op_with_persistence_writes_failed_run_without_review_output() {
    skip_if_no_network!();

    let (server, _request_log) =
        start_responses_server_with_sse(completed_sse(), /*expected_requests*/ 1).await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    let review_target = ReviewTarget::Custom {
        instructions: "persist this failed review".to_string(),
    };
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: review_target.clone(),
                user_facing_hint: None,
            },
            persistence: Some(ReviewPersistence::ManualAutoReview),
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let exited = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExitedReviewMode(_))).await;
    match exited {
        EventMsg::ExitedReviewMode(ExitedReviewModeEvent { review_output }) => {
            assert_eq!(review_output, None);
        }
        other => panic!("expected ExitedReviewMode(..), got {other:?}"),
    }
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let run = load_single_auto_review_run(codex_home.path()).expect("load auto review run");
    let run_id = run.run_id.clone();

    assert_eq!(run.run_id, run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Failed);
    assert_eq!(run.source, AutoReviewRunSource::Manual);
    assert_eq!(run.review_target, review_target);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(
        run.error_summary.as_deref(),
        Some("review ended without producing review output")
    );
    assert_eq!(run.findings.len(), 0);

    let _codex_home_guard = codex_home;
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_op_with_persistence_writes_cancelled_run_when_interrupted() {
    skip_if_no_network!();

    let (gate_completed_tx, gate_completed_rx) = oneshot::channel();
    let chunks = vec![
        StreamingSseChunk {
            gate: None,
            body: streaming_sse_event(responses::ev_response_created("resp-1")),
        },
        StreamingSseChunk {
            gate: Some(gate_completed_rx),
            body: streaming_sse_event(responses::ev_completed("resp-1")),
        },
    ];
    let (server, _completions) = start_streaming_sse_server(vec![chunks]).await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = test_codex()
        .with_model("gpt-5.4")
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await
        .unwrap()
        .codex;

    let review_target = ReviewTarget::Custom {
        instructions: "persist this cancelled review".to_string(),
    };
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: review_target.clone(),
                user_facing_hint: None,
            },
            persistence: Some(ReviewPersistence::ManualAutoReview),
        })
        .await
        .unwrap();

    server.wait_for_request_count(1).await;
    codex.submit(Op::Interrupt).await.unwrap();

    let _exited = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExitedReviewMode(_))).await;
    let _aborted = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnAborted(_))).await;
    let _ = gate_completed_tx.send(());

    let run = load_single_auto_review_run(codex_home.path()).expect("load auto review run");
    let run_id = run.run_id.clone();

    assert_eq!(run.run_id, run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(run.source, AutoReviewRunSource::Manual);
    assert_eq!(run.review_target, review_target);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(
        run.error_summary.as_deref(),
        Some("review was interrupted before producing structured output")
    );
    assert_eq!(run.findings.len(), 0);

    let _codex_home_guard = codex_home;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_persistence_writes_background_run_without_review_mode_events() {
    skip_if_no_network!();

    let review_json = serde_json::json!({
        "findings": [],
        "overall_correctness": "patch is correct",
        "overall_explanation": "Background review completed.",
        "overall_confidence_score": 0.8
    })
    .to_string();
    let (server, _request_log) = start_responses_server_with_sse(
        assistant_message_sse(&review_json),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    let review_target = ReviewTarget::Custom {
        instructions: "background review".to_string(),
    };
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: review_target.clone(),
                user_facing_hint: None,
            },
            persistence: Some(ReviewPersistence::BackgroundAutoReview),
        })
        .await
        .unwrap();

    let running_status =
        wait_for_background_auto_review_status(&codex, BackgroundAutoReviewStatus::Running, None)
            .await;
    let completed_status = wait_for_background_auto_review_status(
        &codex,
        BackgroundAutoReviewStatus::Completed,
        Some(running_status.run_id.as_str()),
    )
    .await;
    assert_eq!(completed_status.review_target, review_target);
    assert_eq!(completed_status.error_summary, None);

    let run = load_single_auto_review_run(codex_home.path())
        .unwrap_or_else(|err| panic!("load single auto review run: {err}"));
    assert_eq!(run.run_id, completed_status.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert_eq!(run.review_target, review_target);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(run.error_summary, None);

    let _codex_home_guard = codex_home;
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_persistence_writes_cancelled_run_without_review_mode_events() {
    skip_if_no_network!();

    let (gate_completed_tx, gate_completed_rx) = oneshot::channel();
    let chunks = vec![
        StreamingSseChunk {
            gate: None,
            body: streaming_sse_event(responses::ev_response_created("resp-1")),
        },
        StreamingSseChunk {
            gate: Some(gate_completed_rx),
            body: streaming_sse_event(responses::ev_completed("resp-1")),
        },
    ];
    let (server, _completions) = start_streaming_sse_server(vec![chunks]).await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = test_codex()
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await
        .unwrap()
        .codex;

    let review_target = ReviewTarget::Custom {
        instructions: "cancel background review".to_string(),
    };
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: review_target.clone(),
                user_facing_hint: None,
            },
            persistence: Some(ReviewPersistence::BackgroundAutoReview),
        })
        .await
        .unwrap();

    let running_status =
        wait_for_background_auto_review_status(&codex, BackgroundAutoReviewStatus::Running, None)
            .await;

    server.wait_for_request_count(1).await;
    let running = load_single_auto_review_run(codex_home.path())
        .unwrap_or_else(|err| panic!("load running auto review run: {err}"));
    assert_eq!(running.run_id, running_status.run_id);
    assert_eq!(running.status, AutoReviewRunStatus::Running);
    assert_eq!(running.source, AutoReviewRunSource::Background);
    assert_eq!(running.completed_at_unix_secs, None);

    codex.submit(Op::Interrupt).await.unwrap();

    let cancelled_status = wait_for_background_auto_review_status(
        &codex,
        BackgroundAutoReviewStatus::Cancelled,
        Some(running_status.run_id.as_str()),
    )
    .await;
    assert_eq!(cancelled_status.review_target, review_target);
    assert_eq!(
        cancelled_status.error_summary.as_deref(),
        Some("review was interrupted before producing structured output")
    );
    let _ = gate_completed_tx.send(());

    let run = load_single_auto_review_run(codex_home.path())
        .unwrap_or_else(|err| panic!("load cancelled auto review run: {err}"));
    assert_eq!(run.run_id, running.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert_eq!(run.review_target, review_target);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(
        run.error_summary.as_deref(),
        Some("review was interrupted before producing structured output")
    );

    let _codex_home_guard = codex_home;
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_runs_after_file_changing_turn() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let review_json = serde_json::json!({
        "findings": [],
        "overall_correctness": "patch is correct",
        "overall_explanation": "Automatic background review completed.",
        "overall_confidence_score": 0.9
    })
    .to_string();
    let harness = TestCodexHarness::new().await?;
    init_git_repo(harness.cwd());
    let codex_home = harness.test().codex_home_path().to_path_buf();
    let call_id = "auto-bg-apply-patch";
    let patch =
        "*** Begin Patch\n*** Add File: auto_background_review.txt\n+review me\n*** End Patch";
    let request_log = mount_sse_sequence(
        harness.server(),
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_apply_patch_custom_tool_call(call_id, patch),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "patch applied"),
                ev_completed("resp-2"),
            ]),
            responses::sse(assistant_message_sse(&review_json)),
        ],
    )
    .await;

    harness
        .test()
        .submit_turn("create a file to review")
        .await?;

    let pending_status = wait_for_background_auto_review_status(
        harness.test().codex.as_ref(),
        BackgroundAutoReviewStatus::Pending,
        None,
    )
    .await;
    assert_eq!(
        pending_status.review_target,
        ReviewTarget::UncommittedChanges
    );
    assert_eq!(pending_status.error_summary, None);
    let running_status = wait_for_background_auto_review_status(
        harness.test().codex.as_ref(),
        BackgroundAutoReviewStatus::Running,
        Some(pending_status.run_id.as_str()),
    )
    .await;
    let completed_status = wait_for_background_auto_review_status(
        harness.test().codex.as_ref(),
        BackgroundAutoReviewStatus::Completed,
        Some(running_status.run_id.as_str()),
    )
    .await;
    assert_eq!(
        completed_status.review_target,
        ReviewTarget::UncommittedChanges
    );
    assert_eq!(completed_status.error_summary, None);
    let run = load_single_auto_review_run(&codex_home)
        .unwrap_or_else(|err| panic!("load completed auto review run: {err}"));
    assert_eq!(run.run_id, pending_status.run_id);
    assert_eq!(run.run_id, completed_status.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert_eq!(run.review_target, ReviewTarget::UncommittedChanges);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(run.error_summary, None);
    assert!(
        run.target.worktree_diff_fingerprint.is_some(),
        "expected background uncommitted review to record diff fingerprint"
    );

    let file_contents = harness.read_file_text("auto_background_review.txt").await?;
    assert_eq!(file_contents, "review me\n");
    let requests = request_log.requests();
    assert_eq!(
        requests.len(),
        3,
        "expected foreground tool, foreground completion, and background review requests"
    );
    let review_request = &requests[2];
    assert_eq!(
        review_request.header("x-openai-subagent").as_deref(),
        Some("review")
    );
    assert!(
        review_request.body_contains_text("Review the current code changes"),
        "background review request should use the uncommitted-changes review prompt"
    );
    harness.server().verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_review_awareness_injected_for_current_findings() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let harness = TestCodexHarness::new().await?;
    init_git_repo(harness.cwd());
    let reviewed_path = harness.cwd().join("auto_review_awareness.txt");
    std::fs::write(&reviewed_path, "needs review\n")?;
    let codex_home = harness.test().codex_home_path().to_path_buf();
    let fingerprint = expected_worktree_diff_fingerprint(harness.cwd()).await;
    let run = codex_auto_review::AutoReviewRun {
        schema_version: codex_auto_review::SCHEMA_VERSION,
        run_id: "awareness_run".to_string(),
        status: AutoReviewRunStatus::Completed,
        freshness: codex_auto_review::AutoReviewRunFreshness::Current,
        source: AutoReviewRunSource::Background,
        target: codex_auto_review::AutoReviewRunTarget {
            branch: Some("main".to_string()),
            head_sha: current_git_head(harness.cwd()),
            base_sha: None,
            worktree_path: Some(harness.cwd().to_path_buf()),
            worktree_diff_fingerprint: Some(fingerprint),
        },
        review_target: ReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1,
        completed_at_unix_secs: Some(2),
        model: Some("gpt-test".to_string()),
        superseded_by: None,
        cancel_reason: None,
        error_summary: None,
        findings: vec![codex_auto_review::AutoReviewFindingRecord {
            finding_id: "f1".to_string(),
            finding: ReviewFinding {
                title: "Use a clearer fixture name".to_string(),
                body: "full finding body must not be injected into normal turns".to_string(),
                confidence_score: 0.9,
                priority: 1,
                code_location: ReviewCodeLocation {
                    absolute_file_path: reviewed_path,
                    line_range: ReviewLineRange { start: 1, end: 1 },
                },
            },
        }],
    };
    AutoReviewStore::for_scope(&codex_home, harness.cwd()).save_run(&run)?;
    let request_log = mount_sse_sequence(
        harness.server(),
        vec![sse(vec![
            ev_assistant_message("msg-1", "ack"),
            ev_completed("resp-1"),
        ])],
    )
    .await;

    harness.test().submit_turn("continue").await?;
    let requests = request_log.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.body_contains_text("<auto_review_awareness>"));
    assert!(request.body_contains_text("current findings from run awareness_run"));
    assert!(request.body_contains_text("[P1] f1: Use a clearer fixture name"));
    assert!(request.body_contains_text("run_id/finding_id"));
    assert!(
        !request.body_contains_text("full finding body must not be injected into normal turns")
    );

    harness.server().verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_shutdown_persists_cancelled_run() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let call_id = "auto-bg-shutdown-apply-patch";
    let patch = "*** Begin Patch\n*** Add File: auto_background_review_shutdown.txt\n+review shutdown\n*** End Patch";
    let (gate_background_completed_tx, gate_background_completed_rx) = oneshot::channel();
    let foreground_tool = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call(call_id, patch),
            ev_completed("resp-1"),
        ]),
    }];
    let foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-1", "patch applied"),
            ev_completed("resp-2"),
        ]),
    }];
    let background_review = vec![
        StreamingSseChunk {
            gate: None,
            body: streaming_sse_event(responses::ev_response_created("resp-3")),
        },
        StreamingSseChunk {
            gate: Some(gate_background_completed_rx),
            body: streaming_sse_event(responses::ev_completed("resp-3")),
        },
    ];
    let (server, _completions) = start_streaming_sse_server(vec![
        foreground_tool,
        foreground_complete,
        background_review,
    ])
    .await;
    let codex_home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await?;
    init_git_repo(test.cwd_path());

    test.submit_turn("create a file to review").await?;
    server.wait_for_request_count(3).await;
    let pending_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Pending,
        None,
    )
    .await;
    let running_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Running,
        Some(pending_status.run_id.as_str()),
    )
    .await;
    let running = wait_for_auto_review_run_status(
        test.codex.as_ref(),
        codex_home.path(),
        AutoReviewRunStatus::Running,
    )
    .await;
    assert_eq!(running.run_id, running_status.run_id);
    assert_eq!(running.source, AutoReviewRunSource::Background);
    assert_eq!(running.review_target, ReviewTarget::UncommittedChanges);

    test.codex.submit(Op::Shutdown {}).await?;
    let cancelled_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Cancelled,
        Some(running_status.run_id.as_str()),
    )
    .await;
    assert_eq!(
        cancelled_status.review_target,
        ReviewTarget::UncommittedChanges
    );
    assert_eq!(
        cancelled_status.error_summary.as_deref(),
        Some("review was interrupted before producing structured output")
    );
    let _shutdown =
        wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;
    let run = load_single_auto_review_run(codex_home.path())
        .unwrap_or_else(|err| panic!("load cancelled auto review run: {err}"));
    assert_eq!(run.run_id, running.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(
        run.error_summary.as_deref(),
        Some("review was interrupted before producing structured output")
    );

    drop(gate_background_completed_tx);
    let _codex_home_guard = codex_home;
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_shutdown_cancels_pending_debounce() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let call_id = "auto-bg-shutdown-before-debounce";
    let patch = "*** Begin Patch\n*** Add File: auto_background_review_shutdown_pending.txt\n+pending shutdown\n*** End Patch";
    let foreground_tool = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call(call_id, patch),
            ev_completed("resp-1"),
        ]),
    }];
    let foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-1", "patch applied"),
            ev_completed("resp-2"),
        ]),
    }];
    let background_review = vec![StreamingSseChunk {
        gate: None,
        body: streaming_sse_event(responses::ev_completed("resp-3")),
    }];
    let (server, _completions) = start_streaming_sse_server(vec![
        foreground_tool,
        foreground_complete,
        background_review,
    ])
    .await;
    let codex_home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await?;
    init_git_repo(test.cwd_path());

    test.submit_turn("create a file to review").await?;
    server.wait_for_request_count(2).await;

    let pending_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Pending,
        None,
    )
    .await;

    test.codex.submit(Op::Shutdown {}).await?;
    let cancelled_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Cancelled,
        Some(pending_status.run_id.as_str()),
    )
    .await;
    assert_eq!(
        cancelled_status.review_target,
        ReviewTarget::UncommittedChanges
    );
    assert_eq!(
        cancelled_status.error_summary.as_deref(),
        Some("review was interrupted before producing structured output")
    );
    let _shutdown =
        wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;
    server
        .assert_request_count_stays(2, Duration::from_millis(2500))
        .await;
    let run = load_single_auto_review_run(codex_home.path())?;
    assert_eq!(run.run_id, pending_status.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(
        run.error_summary.as_deref(),
        Some("review was interrupted before producing structured output")
    );

    let _codex_home_guard = codex_home;
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_control_cancels_pending_run() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let call_id = "auto-bg-control-pending";
    let patch = "*** Begin Patch\n*** Add File: auto_background_review_control_pending.txt\n+pending control\n*** End Patch";
    let foreground_tool = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call(call_id, patch),
            ev_completed("resp-1"),
        ]),
    }];
    let foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-1", "patch applied"),
            ev_completed("resp-2"),
        ]),
    }];
    let background_review = vec![StreamingSseChunk {
        gate: None,
        body: streaming_sse_event(responses::ev_completed("resp-3")),
    }];
    let (server, _completions) = start_streaming_sse_server(vec![
        foreground_tool,
        foreground_complete,
        background_review,
    ])
    .await;
    let codex_home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await?;
    init_git_repo(test.cwd_path());

    test.submit_turn("create a file to review").await?;
    server.wait_for_request_count(2).await;

    let pending_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Pending,
        None,
    )
    .await;

    test.codex
        .submit(Op::BackgroundAutoReviewControl {
            run_id: pending_status.run_id.clone(),
            action: BackgroundAutoReviewControlAction::Cancel,
            reason: BackgroundAutoReviewControlReason::UserRequested,
        })
        .await?;
    let cancelled_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Cancelled,
        Some(pending_status.run_id.as_str()),
    )
    .await;
    assert_eq!(
        cancelled_status.review_target,
        ReviewTarget::UncommittedChanges
    );
    assert_eq!(
        cancelled_status.error_summary.as_deref(),
        Some("background auto review was cancelled by request")
    );
    server
        .assert_request_count_stays(2, Duration::from_millis(2500))
        .await;
    let run = load_single_auto_review_run(codex_home.path())?;
    assert_eq!(run.run_id, pending_status.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert!(run.completed_at_unix_secs.is_some());
    assert_eq!(
        run.error_summary.as_deref(),
        Some("background auto review was cancelled by request")
    );

    test.codex.submit(Op::Shutdown {}).await?;
    let _shutdown =
        wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;
    let _codex_home_guard = codex_home;
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_control_cancels_running_run() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let call_id = "auto-bg-control-running";
    let patch = "*** Begin Patch\n*** Add File: auto_background_review_control_running.txt\n+running control\n*** End Patch";
    let (gate_background_completed_tx, gate_background_completed_rx) = oneshot::channel();
    let foreground_tool = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call(call_id, patch),
            ev_completed("resp-1"),
        ]),
    }];
    let foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-1", "patch applied"),
            ev_completed("resp-2"),
        ]),
    }];
    let background_review = vec![
        StreamingSseChunk {
            gate: None,
            body: streaming_sse_event(responses::ev_response_created("resp-3")),
        },
        StreamingSseChunk {
            gate: Some(gate_background_completed_rx),
            body: streaming_sse_event(responses::ev_completed("resp-3")),
        },
    ];
    let (server, _completions) = start_streaming_sse_server(vec![
        foreground_tool,
        foreground_complete,
        background_review,
    ])
    .await;
    let codex_home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await?;
    init_git_repo(test.cwd_path());

    test.submit_turn("create a file to review").await?;
    server.wait_for_request_count(3).await;
    let pending_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Pending,
        None,
    )
    .await;
    let running_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Running,
        Some(pending_status.run_id.as_str()),
    )
    .await;

    test.codex
        .submit(Op::BackgroundAutoReviewControl {
            run_id: running_status.run_id.clone(),
            action: BackgroundAutoReviewControlAction::Cancel,
            reason: BackgroundAutoReviewControlReason::UserRequested,
        })
        .await?;
    let cancelled_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Cancelled,
        Some(running_status.run_id.as_str()),
    )
    .await;
    assert_eq!(
        cancelled_status.error_summary.as_deref(),
        Some("background auto review was cancelled by request")
    );

    drop(gate_background_completed_tx);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let run = load_single_auto_review_run(codex_home.path())?;
    assert_eq!(run.run_id, running_status.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert_eq!(
        run.error_summary.as_deref(),
        Some("background auto review was cancelled by request")
    );

    test.codex.submit(Op::Shutdown {}).await?;
    let _shutdown =
        wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;
    let _codex_home_guard = codex_home;
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_skips_oversized_diff() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let patch = "*** Begin Patch\n*** Add File: auto_background_review_large.txt\n+too large\n*** End Patch";
    let foreground_tool = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call("auto-bg-large", patch),
            ev_completed("resp-1"),
        ]),
    }];
    let foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-1", "patch applied"),
            ev_completed("resp-2"),
        ]),
    }];
    let (server, _completions) =
        start_streaming_sse_server(vec![foreground_tool, foreground_complete]).await;
    let codex_home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(codex_home.clone())
        .with_config(|config| {
            config.background_auto_review_max_diff_bytes = Some(1);
        })
        .build_with_streaming_server(&server)
        .await?;
    init_git_repo(test.cwd_path());

    test.submit_turn("create a file too large for background review")
        .await?;

    let pending_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Pending,
        None,
    )
    .await;
    let skipped_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Skipped,
        Some(pending_status.run_id.as_str()),
    )
    .await;
    assert_eq!(
        skipped_status.review_target,
        ReviewTarget::UncommittedChanges
    );
    assert!(
        skipped_status
            .error_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("diff exceeds background review size limit"))
    );
    let runs = load_auto_review_runs(codex_home.path())?;
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.run_id, skipped_status.run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Skipped);
    assert_eq!(
        run.error_summary.as_deref(),
        skipped_status.error_summary.as_deref()
    );
    server
        .assert_request_count_stays(2, Duration::from_millis(2500))
        .await;

    let _codex_home_guard = codex_home;
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_suppresses_unchanged_dirty_diff() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let review_json = serde_json::json!({
        "findings": [],
        "overall_correctness": "patch is correct",
        "overall_explanation": "Initial background review completed.",
        "overall_confidence_score": 0.9
    })
    .to_string();
    let patch = "*** Begin Patch\n*** Add File: auto_background_review_once.txt\n+review once\n*** End Patch";
    let (gate_second_completed_tx, gate_second_completed_rx) = oneshot::channel();
    let foreground_tool = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call("auto-bg-once", patch),
            ev_completed("resp-1"),
        ]),
    }];
    let foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-1", "patch applied"),
            ev_completed("resp-2"),
        ]),
    }];
    let background_review = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(assistant_message_sse(&review_json)),
    }];
    let unchanged_foreground = vec![StreamingSseChunk {
        gate: Some(gate_second_completed_rx),
        body: responses::sse(vec![
            ev_assistant_message("msg-2", "no further changes"),
            ev_completed("resp-4"),
        ]),
    }];
    let (server, _completions) = start_streaming_sse_server(vec![
        foreground_tool,
        foreground_complete,
        background_review,
        unchanged_foreground,
    ])
    .await;
    let codex_home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await?;
    init_git_repo(test.cwd_path());

    test.submit_turn("create a file to review").await?;
    let first_run = wait_for_background_auto_review_run_without_review_mode_events(
        test.codex.as_ref(),
        codex_home.path(),
    )
    .await;
    assert_eq!(first_run.status, AutoReviewRunStatus::Completed);
    assert_eq!(
        first_run.target.worktree_diff_fingerprint,
        Some(expected_worktree_diff_fingerprint(test.cwd_path()).await)
    );

    let second_turn = test.submit_turn("respond without changing files");
    tokio::pin!(second_turn);
    tokio::select! {
        result = &mut second_turn => panic!("second turn completed before gate: {result:?}"),
        _ = server.wait_for_request_count(4) => {}
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    let _ = gate_second_completed_tx.send(());
    second_turn.await?;
    server
        .assert_request_count_stays(4, Duration::from_millis(2500))
        .await;
    assert_eq!(load_auto_review_runs(codex_home.path())?.len(), 1);

    let _codex_home_guard = codex_home;
    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_background_review_debounce_ignores_superseded_diff() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let review_json = serde_json::json!({
        "findings": [],
        "overall_correctness": "patch is correct",
        "overall_explanation": "Superseded background review completed once.",
        "overall_confidence_score": 0.9
    })
    .to_string();
    let first_patch =
        "*** Begin Patch\n*** Add File: auto_background_review_first.txt\n+first\n*** End Patch";
    let second_patch =
        "*** Begin Patch\n*** Add File: auto_background_review_second.txt\n+second\n*** End Patch";
    let (gate_second_patch_tx, gate_second_patch_rx) = oneshot::channel();
    let first_foreground_tool = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_response_created("resp-1"),
            ev_apply_patch_custom_tool_call("auto-bg-first", first_patch),
            ev_completed("resp-1"),
        ]),
    }];
    let first_foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-1", "first patch applied"),
            ev_completed("resp-2"),
        ]),
    }];
    let second_foreground_tool = vec![
        StreamingSseChunk {
            gate: None,
            body: streaming_sse_event(ev_response_created("resp-3")),
        },
        StreamingSseChunk {
            gate: Some(gate_second_patch_rx),
            body: responses::sse(vec![
                ev_apply_patch_custom_tool_call("auto-bg-second", second_patch),
                ev_completed("resp-3"),
            ]),
        },
    ];
    let second_foreground_complete = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(vec![
            ev_assistant_message("msg-2", "second patch applied"),
            ev_completed("resp-4"),
        ]),
    }];
    let background_review = vec![StreamingSseChunk {
        gate: None,
        body: responses::sse(assistant_message_sse(&review_json)),
    }];
    let (server, _completions) = start_streaming_sse_server(vec![
        first_foreground_tool,
        first_foreground_complete,
        second_foreground_tool,
        second_foreground_complete,
        background_review,
    ])
    .await;
    let codex_home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(codex_home.clone())
        .build_with_streaming_server(&server)
        .await?;
    init_git_repo(test.cwd_path());

    test.submit_turn("create the first file").await?;
    let first_pending_status = wait_for_background_auto_review_status(
        test.codex.as_ref(),
        BackgroundAutoReviewStatus::Pending,
        None,
    )
    .await;
    let second_turn = test.submit_turn("create the second file");
    tokio::pin!(second_turn);
    tokio::select! {
        result = &mut second_turn => panic!("second turn completed before gate: {result:?}"),
        _ = server.wait_for_request_count(3) => {}
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    let _ = gate_second_patch_tx.send(());
    second_turn.await?;

    let run = wait_for_background_auto_review_run_without_review_mode_events(
        test.codex.as_ref(),
        codex_home.path(),
    )
    .await;
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert_eq!(run.review_target, ReviewTarget::UncommittedChanges);
    assert_eq!(
        run.target.worktree_diff_fingerprint,
        Some(expected_worktree_diff_fingerprint(test.cwd_path()).await)
    );
    let runs = load_auto_review_runs(codex_home.path())?;
    assert_eq!(runs.len(), 2, "expected cancelled and completed runs");
    let cancelled = runs
        .iter()
        .find(|run| run.run_id == first_pending_status.run_id)
        .expect("expected first pending run to be persisted");
    assert_eq!(cancelled.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(cancelled.source, AutoReviewRunSource::Background);
    assert_eq!(cancelled.review_target, ReviewTarget::UncommittedChanges);
    assert_eq!(
        cancelled.error_summary.as_deref(),
        Some("foreground work started before background auto review could continue")
    );
    assert!(cancelled.completed_at_unix_secs.is_some());
    assert!(runs.iter().any(|candidate| candidate.run_id == run.run_id));
    server.wait_for_request_count(5).await;
    server
        .assert_request_count_stays(5, Duration::from_millis(500))
        .await;

    let _codex_home_guard = codex_home;
    server.shutdown().await;
    Ok(())
}

/// Ensure review flow suppresses assistant-specific streaming/completion events:
/// - AgentMessageContentDelta
/// - ItemCompleted for TurnItem::AgentMessage
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_filters_agent_message_related_events() {
    skip_if_no_network!();

    let (server, _request_log) = start_responses_server_with_sse(
        vec![
            responses::ev_message_item_added("msg-1", ""),
            responses::ev_output_text_delta("Hi"),
            responses::ev_output_text_delta(" there"),
            responses::ev_assistant_message("msg-1", "Hi there"),
            responses::ev_completed("resp-1"),
        ],
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Filter streaming events".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    let mut saw_entered = false;
    let mut saw_exited = false;

    // Drain until TurnComplete; assert streaming-related events never surface.
    wait_for_event(&codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::EnteredReviewMode(_) => {
            saw_entered = true;
            false
        }
        EventMsg::ExitedReviewMode(_) => {
            saw_exited = true;
            false
        }
        // The following must be filtered by review flow
        EventMsg::AgentMessageContentDelta(_) => {
            panic!("unexpected AgentMessageContentDelta surfaced during review")
        }
        _ => false,
    })
    .await;
    assert!(saw_entered && saw_exited, "missing review lifecycle events");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// When the model returns structured JSON in a review, ensure only a single
/// non-streaming AgentMessage is emitted; the UI consumes the structured
/// result via ExitedReviewMode plus a final assistant message.
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_does_not_emit_agent_message_on_structured_output() {
    skip_if_no_network!();

    let review_json = serde_json::json!({
        "findings": [
            {
                "title": "Example",
                "body": "Structured review output.",
                "confidence_score": 0.5,
                "priority": 1,
                "code_location": {
                    "absolute_file_path": "/tmp/file.rs",
                    "line_range": {"start": 1, "end": 2}
                }
            }
        ],
        "overall_correctness": "ok",
        "overall_explanation": "ok",
        "overall_confidence_score": 0.5
    })
    .to_string();
    let (server, _request_log) = start_responses_server_with_sse(
        assistant_message_sse(&review_json),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "check structured".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    // Drain events until TurnComplete; ensure we only see a final
    // AgentMessage (no streaming assistant messages).
    let mut saw_entered = false;
    let mut saw_exited = false;
    let mut agent_messages = 0;
    wait_for_event(&codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::AgentMessage(_) => {
            agent_messages += 1;
            false
        }
        EventMsg::EnteredReviewMode(_) => {
            saw_entered = true;
            false
        }
        EventMsg::ExitedReviewMode(_) => {
            saw_exited = true;
            false
        }
        _ => false,
    })
    .await;
    assert_eq!(1, agent_messages, "expected exactly one AgentMessage event");
    assert!(saw_entered && saw_exited, "missing review lifecycle events");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// Ensure that when a custom `review_model` is set in the config, the review
/// request uses that model (and not the main chat model).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_uses_custom_review_model_from_config() {
    skip_if_no_network!();

    let (server, request_log) =
        start_responses_server_with_sse(completed_sse(), /*expected_requests*/ 1).await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    // Choose a review model different from the main model; ensure it is used.
    let codex = new_conversation_for_server(&server, codex_home.clone(), |cfg| {
        cfg.model = Some("gpt-4.1".to_string());
        cfg.review_model = Some("gpt-5.4".to_string());
    })
    .await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "use custom model".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    // Wait for completion
    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: None
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Assert the request body model equals the configured review model
    let request = request_log.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let body = request.body_json();
    assert_eq!(body["model"].as_str().unwrap(), "gpt-5.4");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// Ensure that when `review_model` is not set in the config, the review request
/// uses the session model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_uses_session_model_when_review_model_unset() {
    skip_if_no_network!();

    let (server, request_log) =
        start_responses_server_with_sse(completed_sse(), /*expected_requests*/ 1).await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |cfg| {
        cfg.model = Some("gpt-4.1".to_string());
        cfg.review_model = None;
    })
    .await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "use session model".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: None
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = request_log.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let body = request.body_json();
    assert_eq!(body["model"].as_str().unwrap(), "gpt-4.1");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// When a review session begins, it must not prepend prior chat history from
/// the parent session. The request `input` should contain only the review
/// prompt from the user.
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_input_isolated_from_parent_history() {
    skip_if_no_network!();

    let (server, request_log) =
        start_responses_server_with_sse(completed_sse(), /*expected_requests*/ 1).await;

    // Seed a parent session history via resume file with both user + assistant items.
    let codex_home = Arc::new(TempDir::new().unwrap());

    let session_file = codex_home.path().join("resume.jsonl");
    {
        let mut f = tokio::fs::File::create(&session_file).await.unwrap();
        let convo_id = Uuid::new_v4();
        // Proper session_meta line (enveloped) with a conversation id
        let meta_line = serde_json::json!({
            "timestamp": "2024-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": convo_id,
                "timestamp": "2024-01-01T00:00:00Z",
                "cwd": ".",
                "originator": "test_originator",
                "cli_version": "test_version",
                "model_provider": "test-provider"
            }
        });
        f.write_all(format!("{meta_line}\n").as_bytes())
            .await
            .unwrap();

        // Prior user message (enveloped response_item)
        let user = codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::InputText {
                text: "parent: earlier user message".to_string(),
            }],
            phase: None,
        };
        let user_json = serde_json::to_value(&user).unwrap();
        let user_line = serde_json::json!({
            "timestamp": "2024-01-01T00:00:01.000Z",
            "type": "response_item",
            "payload": user_json
        });
        f.write_all(format!("{user_line}\n").as_bytes())
            .await
            .unwrap();

        // Prior assistant message (enveloped response_item)
        let assistant = codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText {
                text: "parent: assistant reply".to_string(),
            }],
            phase: None,
        };
        let assistant_json = serde_json::to_value(&assistant).unwrap();
        let assistant_line = serde_json::json!({
            "timestamp": "2024-01-01T00:00:02.000Z",
            "type": "response_item",
            "payload": assistant_json
        });
        f.write_all(format!("{assistant_line}\n").as_bytes())
            .await
            .unwrap();
    }
    let codex =
        resume_conversation_for_server(&server, codex_home.clone(), session_file.clone(), |_| {})
            .await;

    // Submit review request; it must start fresh (no parent history in `input`).
    let review_prompt = "Please review only this".to_string();
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: review_prompt.clone(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: None
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Assert the request `input` contains the environment context followed by the user review prompt.
    let request = request_log.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let body = request.body_json();
    let input = body["input"].as_array().expect("input array");
    assert!(
        input.len() >= 2,
        "expected at least environment context and review prompt"
    );

    let env_text = input
        .iter()
        .filter_map(|msg| msg.get("content").and_then(|content| content.as_array()))
        .flat_map(|content| content.iter())
        .filter_map(|entry| entry.get("text").and_then(|text| text.as_str()))
        .find(|text| text.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG))
        .expect("env text");
    assert!(
        env_text.contains("<cwd>"),
        "environment context should include cwd"
    );

    let review_text = input
        .iter()
        .filter_map(|msg| msg.get("content").and_then(|content| content.as_array()))
        .flat_map(|content| content.iter())
        .filter_map(|entry| entry.get("text").and_then(|text| text.as_str()))
        .find(|text| *text == review_prompt)
        .expect("review prompt text");
    assert_eq!(
        review_text, review_prompt,
        "user message should only contain the raw review prompt"
    );

    // Ensure the REVIEW_PROMPT rubric is sent via instructions.
    let instructions = body["instructions"].as_str().expect("instructions string");
    assert_eq!(instructions, REVIEW_PROMPT);

    // Also verify that a user interruption note was recorded in the rollout.
    let path = codex.rollout_path().expect("rollout path");
    let text = std::fs::read_to_string(&path).expect("read rollout file");
    let mut saw_interruption_message = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).expect("jsonl line");
        let rl: RolloutLine = serde_json::from_value(v).expect("rollout line");
        if let RolloutItem::ResponseItem(ResponseItem::Message { role, content, .. }) = rl.item
            && role == "user"
        {
            for c in content {
                if let ContentItem::InputText { text } = c
                    && text.contains("User initiated a review task, but was interrupted.")
                {
                    saw_interruption_message = true;
                    break;
                }
            }
        }
        if saw_interruption_message {
            break;
        }
    }
    assert!(
        saw_interruption_message,
        "expected user interruption message in rollout"
    );

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// After a review thread finishes, its conversation should be visible in the
/// parent session so later turns can reference the results.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_history_surfaces_in_parent_session() {
    skip_if_no_network!();

    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse("review assistant output"),
        /*expected_requests*/ 2,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    // 1) Run a review turn that produces an assistant message (isolated in child).
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Start a review".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();
    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: Some(_)
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // 2) Continue in the parent session; request input must not include any review items.
    let followup = "back to parent".to_string();
    codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: followup.clone(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .unwrap();
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Inspect the second request (parent turn) input contents.
    // Parent turns include session initial messages (user_instructions, environment_context).
    // Critically, no messages from the review thread should appear.
    let requests = request_log.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.path(), "/v1/responses");
    }
    let body = requests[1].body_json();
    let input = body["input"].as_array().expect("input array");

    // Must include the followup as the last item for this turn
    let last = input.last().expect("at least one item in input");
    assert_eq!(last["role"].as_str().unwrap(), "user");
    let last_text = last["content"][0]["text"].as_str().unwrap();
    assert_eq!(last_text, followup);

    // Ensure review-thread content is present for downstream turns.
    let contains_review_rollout_user = input.iter().any(|msg| {
        msg["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("User initiated a review task.")
    });
    let contains_review_assistant = input.iter().any(|msg| {
        msg["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("review assistant output")
    });
    assert!(
        contains_review_rollout_user,
        "review rollout user message missing from parent turn input"
    );
    assert!(
        contains_review_assistant,
        "review assistant output missing from parent turn input"
    );

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// `/review` should use the session's current cwd (including runtime overrides)
/// when resolving base-branch review prompts (merge-base computation).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_uses_overridden_cwd_for_base_branch_merge_base() {
    skip_if_no_network!();

    let (server, request_log) =
        start_responses_server_with_sse(completed_sse(), /*expected_requests*/ 1).await;

    let initial_cwd = TempDir::new().unwrap();

    let repo_dir = TempDir::new().unwrap();
    let repo_path = repo_dir.path();

    run_git(repo_path, &["init", "-b", "main"]);
    run_git(repo_path, &["config", "user.email", "test@example.com"]);
    run_git(repo_path, &["config", "user.name", "Test User"]);
    std::fs::write(repo_path.join("file.txt"), "hello\n").unwrap();
    run_git(repo_path, &["add", "."]);
    run_git(repo_path, &["commit", "-m", "initial"]);

    let head_sha = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse HEAD");
    assert!(head_sha.status.success());
    let head_sha = String::from_utf8(head_sha.stdout)
        .expect("utf8 sha")
        .trim()
        .to_string();

    let codex_home = Arc::new(TempDir::new().unwrap());
    let initial_cwd_path = initial_cwd.path().to_path_buf();
    let codex = new_conversation_for_server(&server, codex_home.clone(), move |config| {
        config.cwd = initial_cwd_path.abs();
    })
    .await;

    core_test_support::submit_thread_settings(
        &codex,
        codex_protocol::protocol::ThreadSettingsOverrides {
            cwd: Some(repo_path.to_path_buf().abs()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::BaseBranch {
                    branch: "main".to_string(),
                },
                user_facing_hint: None,
            },
            persistence: None,
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 1);
    for request in &requests {
        assert_eq!(request.path(), "/v1/responses");
    }
    let body = requests[0].body_json();
    let input = body["input"].as_array().expect("input array");

    let saw_merge_base_sha = input
        .iter()
        .filter_map(|msg| msg["content"][0]["text"].as_str())
        .any(|text| text.contains(&head_sha));
    assert!(
        saw_merge_base_sha,
        "expected review prompt to include merge-base sha {head_sha}"
    );

    let _codex_home_guard = codex_home;
    server.verify().await;
}

fn assistant_message_sse(text: &str) -> Vec<serde_json::Value> {
    vec![
        responses::ev_assistant_message("msg-1", text),
        responses::ev_completed("resp-1"),
    ]
}

fn completed_sse() -> Vec<serde_json::Value> {
    vec![responses::ev_completed("resp-1")]
}

fn streaming_sse_event(event: serde_json::Value) -> String {
    format!("data: {event}\n\n")
}

fn load_single_auto_review_run(
    codex_home: &std::path::Path,
) -> anyhow::Result<codex_auto_review::AutoReviewRun> {
    let mut runs = load_auto_review_runs(codex_home)?;
    if runs.len() != 1 {
        anyhow::bail!("expected exactly one auto review run, found {}", runs.len());
    }
    Ok(runs.remove(0))
}

fn load_auto_review_runs(
    codex_home: &std::path::Path,
) -> anyhow::Result<Vec<codex_auto_review::AutoReviewRun>> {
    let review_dir = codex_home.join("state/review");
    if !review_dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in std::fs::read_dir(&review_dir)
        .map_err(|err| anyhow::anyhow!("auto review state dir: {err}"))?
    {
        let entry = entry?;
        let store_root = entry.path().join("auto-review");
        let store = AutoReviewStore::from_store_root(store_root);
        runs.extend(store.list_runs()?);
    }
    runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    Ok(runs)
}

async fn expected_worktree_diff_fingerprint(cwd: &Path) -> String {
    get_worktree_diff_fingerprint(cwd)
        .await
        .expect("expected dirty worktree fingerprint")
}

async fn wait_for_background_auto_review_run_without_review_mode_events(
    codex: &CodexThread,
    codex_home: &std::path::Path,
) -> codex_auto_review::AutoReviewRun {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(Some(run)) = load_auto_review_runs(codex_home).map(|runs| {
            runs.into_iter()
                .find(|run| run.status == AutoReviewRunStatus::Completed)
        }) {
            drain_pending_events_without_review_mode(codex).await;
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timeout waiting for automatic background auto-review run"
        );
        match tokio::time::timeout(Duration::from_millis(50), codex.next_event()).await {
            Ok(Ok(event)) => match event.msg {
                EventMsg::EnteredReviewMode(_) | EventMsg::ExitedReviewMode(_) => {
                    panic!(
                        "automatic background review emitted review mode event: {:?}",
                        event.msg
                    )
                }
                _ => {}
            },
            Ok(Err(err)) => {
                panic!("stream ended while waiting for automatic background auto-review: {err}")
            }
            Err(_) => {}
        }
    }
}

async fn wait_for_auto_review_run_status(
    codex: &CodexThread,
    codex_home: &std::path::Path,
    expected_status: AutoReviewRunStatus,
) -> codex_auto_review::AutoReviewRun {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(run) = load_single_auto_review_run(codex_home)
            && run.status == expected_status
        {
            return run;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timeout waiting for auto-review run status {expected_status:?}"
        );
        match tokio::time::timeout(Duration::from_millis(50), codex.next_event()).await {
            Ok(Ok(event)) => match event.msg {
                EventMsg::EnteredReviewMode(_) | EventMsg::ExitedReviewMode(_) => {
                    panic!(
                        "background review emitted review mode event while waiting for {expected_status:?}: {:?}",
                        event.msg
                    )
                }
                _ => {}
            },
            Ok(Err(err)) => {
                panic!("stream ended while waiting for auto-review run status: {err}")
            }
            Err(_) => {}
        }
    }
}

async fn wait_for_background_auto_review_status(
    codex: &CodexThread,
    expected_status: BackgroundAutoReviewStatus,
    expected_run_id: Option<&str>,
) -> BackgroundAutoReviewStatusEvent {
    loop {
        let event = match tokio::time::timeout(Duration::from_secs(10), codex.next_event()).await {
            Ok(Ok(event)) => event,
            Ok(Err(err)) => {
                panic!(
                    "stream ended unexpectedly while waiting for background auto-review status: {err}"
                )
            }
            Err(_) => {
                panic!("timeout waiting for background auto-review status {expected_status:?}")
            }
        };
        match event.msg {
            EventMsg::BackgroundAutoReviewStatus(status_event)
                if status_event.status == expected_status
                    && expected_run_id.is_none_or(|run_id| run_id == status_event.run_id) =>
            {
                return status_event;
            }
            EventMsg::BackgroundAutoReviewStatus(status_event) => {
                panic!(
                    "unexpected background auto-review status while waiting for {expected_status:?}: {status_event:?}"
                );
            }
            EventMsg::EnteredReviewMode(_) | EventMsg::ExitedReviewMode(_) => {
                panic!(
                    "background review emitted review mode event while waiting for status {expected_status:?}: {:?}",
                    event.msg
                )
            }
            _ => {}
        }
    }
}

async fn drain_pending_events_without_review_mode(codex: &CodexThread) {
    loop {
        match tokio::time::timeout(Duration::from_millis(50), codex.next_event()).await {
            Ok(Ok(event)) => match event.msg {
                EventMsg::EnteredReviewMode(_) | EventMsg::ExitedReviewMode(_) => {
                    panic!(
                        "automatic background review emitted review mode event: {:?}",
                        event.msg
                    )
                }
                _ => {}
            },
            Ok(Err(err)) => {
                panic!("stream ended while draining automatic background auto-review events: {err}")
            }
            Err(_) => return,
        }
    }
}

fn init_git_repo(repo_path: &std::path::Path) {
    run_git(repo_path, &["init", "-b", "main"]);
    run_git(repo_path, &["config", "user.email", "test@example.com"]);
    run_git(repo_path, &["config", "user.name", "Test User"]);
    std::fs::write(repo_path.join("README.md"), "initial\n").expect("write initial file");
    run_git(repo_path, &["add", "."]);
    run_git(repo_path, &["commit", "-m", "initial"]);
}

fn run_git(repo_path: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={:?} stderr={:?}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn current_git_head(repo_path: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("spawn git rev-parse HEAD");
    assert!(
        output.status.success(),
        "git rev-parse HEAD failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Start a mock Responses API server and mount the given SSE events.
async fn start_responses_server_with_sse(
    events: Vec<serde_json::Value>,
    expected_requests: usize,
) -> (MockServer, ResponseMock) {
    let server = start_mock_server().await;
    let sse = responses::sse(events);
    let responses = vec![sse; expected_requests];
    let request_log = mount_sse_sequence(&server, responses).await;
    (server, request_log)
}

/// Create a conversation configured to talk to the provided mock server.
#[expect(clippy::expect_used)]
async fn new_conversation_for_server<F>(
    server: &MockServer,
    codex_home: Arc<TempDir>,
    mutator: F,
) -> Arc<CodexThread>
where
    F: FnOnce(&mut Config) + Send + 'static,
{
    let base_url = format!("{}/v1", server.uri());
    let mut builder = test_codex()
        .with_home(codex_home)
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url.clone());
            mutator(config);
        });
    builder
        .build(server)
        .await
        .expect("create conversation")
        .codex
}

/// Create a conversation resuming from a rollout file, configured to talk to the provided mock server.
#[expect(clippy::expect_used)]
async fn resume_conversation_for_server<F>(
    server: &MockServer,
    codex_home: Arc<TempDir>,
    resume_path: std::path::PathBuf,
    mutator: F,
) -> Arc<CodexThread>
where
    F: FnOnce(&mut Config) + Send + 'static,
{
    let base_url = format!("{}/v1", server.uri());
    let mut builder = test_codex()
        .with_home(codex_home.clone())
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url.clone());
            mutator(config);
        });
    builder
        .resume(server, codex_home, resume_path)
        .await
        .expect("resume conversation")
        .codex
}
