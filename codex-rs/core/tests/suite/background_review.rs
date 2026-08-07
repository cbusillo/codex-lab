//! End-to-end coverage for the automatic Background Review path.
//!
//! Unlike `suite::review`, which drives reviews through an explicit `Op::Review`,
//! these tests exercise the real trigger: a regular turn that changes the
//! worktree schedules, debounces, runs, and durably records a background review
//! on its own. They also cover the budget stops that turn a background review
//! into a durable terminal run, and the model-facing disposition tool.

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_auto_review::AutoReviewBudget;
use codex_auto_review::AutoReviewDispositionActor;
use codex_auto_review::AutoReviewFindingDisposition;
use codex_auto_review::AutoReviewFindingDispositionRecord;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunFreshness;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunState;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::AutoReviewTerminalReason;
use codex_auto_review::SCHEMA_VERSION;
use codex_auto_review::finding_digests;
use codex_core::CodexThread;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::BackgroundAutoReviewStatus;
use codex_protocol::protocol::BackgroundAutoReviewStatusEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::MockServer;

/// The scheduler debounces for 2s before launching; allow generous slack so a
/// loaded machine cannot turn a slow launch into a failure.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to watch for a background review that must never be scheduled.
/// Comfortably longer than the 2s debounce plus launch work.
const NO_SCHEDULE_WINDOW: Duration = Duration::from_secs(6);

const ADD_FEATURE_PATCH: &str = "*** Begin Patch\n*** Add File: feature.rs\n+pub fn feature() -> u32 {\n+    7\n+}\n*** End Patch\n";
const SEEDED_DEFECT_PATCH: &str = "*** Begin Patch\n*** Add File: average.rs\n+pub fn average(total: u64, count: u64) -> u64 {\n+    total / count\n+}\n*** End Patch\n";

fn run_git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Background Review retains the worktree fingerprint as freshness evidence,
/// so integration fixtures use a real repository with a committed baseline.
fn create_git_repo() -> Result<TempDir> {
    let repo = TempDir::new()?;
    run_git(repo.path(), &["init", "-q", "-b", "main"])?;
    std::fs::write(repo.path().join("README.md"), "initial\n")?;
    run_git(repo.path(), &["add", "README.md"])?;
    run_git(
        repo.path(),
        &[
            "-c",
            "user.name=Codex Test",
            "-c",
            "user.email=codex@example.invalid",
            "commit",
            "-q",
            "-m",
            "initial",
        ],
    )?;
    Ok(repo)
}

fn review_output_json(findings: usize) -> String {
    let findings = (0..findings)
        .map(|index| {
            serde_json::json!({
                "title": format!("Finding {index}"),
                "body": "Background review finding body.",
                "confidence_score": 0.9,
                "priority": 1,
                "code_location": {
                    "absolute_file_path": "/tmp/feature.rs",
                    "line_range": {"start": 1, "end": 3}
                }
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "findings": findings,
        "overall_correctness": "patch is correct",
        "overall_explanation": "Background review completed.",
        "overall_confidence_score": 0.8,
    })
    .to_string()
}

/// The two model responses a single `apply_patch` turn consumes: the tool call,
/// then the follow-up that closes the turn.
fn code_changing_turn_responses(tag: &str) -> Vec<String> {
    code_changing_turn_responses_with_patch(tag, ADD_FEATURE_PATCH)
}

fn code_changing_turn_responses_with_patch(tag: &str, patch: &str) -> Vec<String> {
    vec![
        responses::sse(vec![
            responses::ev_response_created(&format!("resp-{tag}-tool")),
            responses::ev_apply_patch_custom_tool_call(&format!("call-{tag}"), patch),
            responses::ev_completed(&format!("resp-{tag}-tool")),
        ]),
        responses::sse(vec![
            responses::ev_response_created(&format!("resp-{tag}-final")),
            responses::ev_assistant_message(&format!("msg-{tag}"), "added the feature"),
            responses::ev_completed(&format!("resp-{tag}-final")),
        ]),
    ]
}

fn seeded_defect_review_output(path: &Path) -> String {
    serde_json::json!({
        "findings": [{
            "title": "[P1] Guard the zero-count average",
            "body": "Calling average with count == 0 panics; handle the empty case before dividing.",
            "confidence_score": 0.99,
            "priority": 1,
            "code_location": {
                "absolute_file_path": path.join("average.rs"),
                "line_range": {"start": 2, "end": 2}
            }
        }],
        "overall_correctness": "patch is incorrect",
        "overall_explanation": "The new helper panics for an empty input.",
        "overall_confidence_score": 0.99
    })
    .to_string()
}

fn assistant_only_turn_response(tag: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created(&format!("resp-{tag}")),
        responses::ev_assistant_message(&format!("msg-{tag}"), "nothing to change"),
        responses::ev_completed(&format!("resp-{tag}")),
    ])
}

async fn build_codex_in_repo(
    server: &MockServer,
    cwd: AbsolutePathBuf,
    budget: Option<AutoReviewBudget>,
) -> Result<TestCodex> {
    let mut builder = test_codex().with_config(move |config| {
        config.cwd = cwd.clone();
        if let Some(budget) = budget {
            config.background_auto_review_budget = budget;
        }
    });
    Box::pin(builder.build(server)).await
}

async fn submit_turn(codex: &CodexThread, cwd: &AbsolutePathBuf, text: &str) -> Result<()> {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, cwd.as_path());
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::Never),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                ..Default::default()
            },
        })
        .await?;
    Ok(())
}

/// Drains the event stream until the background review reaches `terminal`,
/// returning every background-review status seen along the way. Panics if the
/// review leaks review-mode UI events, which background reviews must never do.
async fn background_review_statuses_until(
    codex: &CodexThread,
    terminal: BackgroundAutoReviewStatus,
) -> Vec<BackgroundAutoReviewStatusEvent> {
    let deadline = tokio::time::Instant::now() + STATUS_TIMEOUT;
    let mut statuses = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, codex.next_event())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for background review status {terminal:?}; saw {statuses:?}"
                )
            })
            .unwrap_or_else(|err| panic!("background review event stream ended: {err}"));
        match event.msg {
            EventMsg::BackgroundAutoReviewStatus(status) => {
                let reached_terminal = status.status == terminal;
                statuses.push(status);
                if reached_terminal {
                    return statuses;
                }
            }
            review_mode @ (EventMsg::EnteredReviewMode(_) | EventMsg::ExitedReviewMode(_)) => {
                panic!("background review emitted review mode event: {review_mode:?}");
            }
            _ => {}
        }
    }
}

/// Waits for the turn to finish and then keeps draining long enough to prove
/// that no background review was scheduled for it.
async fn assert_turn_schedules_no_background_review(codex: &CodexThread) {
    let turn_deadline = tokio::time::Instant::now() + STATUS_TIMEOUT;
    loop {
        let remaining = turn_deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, codex.next_event())
            .await
            .expect("timed out waiting for turn completion")
            .expect("event stream ended");
        match event.msg {
            EventMsg::BackgroundAutoReviewStatus(status) => {
                panic!("no-op turn scheduled a background review: {status:?}");
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    let quiet_deadline = tokio::time::Instant::now() + NO_SCHEDULE_WINDOW;
    loop {
        let remaining = quiet_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, codex.next_event()).await {
            Err(_) => return,
            Ok(Ok(event)) => {
                if let EventMsg::BackgroundAutoReviewStatus(status) = event.msg {
                    panic!("no-op turn scheduled a background review: {status:?}");
                }
            }
            Ok(Err(err)) => panic!("event stream ended: {err}"),
        }
    }
}

fn single_run(store: &AutoReviewStore) -> AutoReviewRun {
    let mut runs = store.list_runs().expect("list auto review runs");
    assert_eq!(runs.len(), 1, "expected exactly one auto review run");
    runs.remove(0)
}

fn in_flight_background_run(repo: &Path, run_id: &str) -> AutoReviewRun {
    AutoReviewRun {
        schema_version: SCHEMA_VERSION,
        run_id: run_id.to_string(),
        status: AutoReviewRunStatus::Running,
        freshness: AutoReviewRunFreshness::Current,
        source: AutoReviewRunSource::Background,
        target: AutoReviewRunTarget {
            branch: None,
            head_sha: None,
            base_sha: None,
            worktree_path: Some(repo.to_path_buf()),
            snapshot_epoch: None,
            snapshot_commit: None,
            head_at_launch: None,
            worktree_diff_fingerprint: None,
            current_turn: None,
            build_provenance: None,
        },
        review_target: ReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1_700_000_000,
        completed_at_unix_secs: None,
        model: Some("mock-model".to_string()),
        reasoning_effort: None,
        prompt_token_estimate: None,
        token_count: None,
        saved_token_estimate: None,
        superseded_by: None,
        cancel_reason: None,
        error_summary: None,
        finding_count: 0,
        finding_digests: Vec::new(),
        omitted_finding_digest_count: 0,
    }
}

/// A process restart must turn an in-flight durable Background Review into one
/// concrete cancellation event instead of leaving the TUI in a running state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_reconciles_orphaned_background_review_into_cancelled_event() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let codex_home = Arc::new(TempDir::new()?);
    let store = AutoReviewStore::for_scope(codex_home.path(), repo.path());
    store.save_run(&in_flight_background_run(
        repo.path(),
        "orphaned-background-review",
    ))?;

    let server = responses::start_mock_server().await;
    let config_cwd = cwd.clone();
    let test = Box::pin(
        test_codex()
            .with_home(Arc::clone(&codex_home))
            .with_config(move |config| config.cwd = config_cwd)
            .build(&server),
    )
    .await?;

    let statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Cancelled,
    ))
    .await;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].run_id, "orphaned-background-review");
    assert_eq!(statuses[0].status, BackgroundAutoReviewStatus::Cancelled);
    assert_eq!(
        statuses[0].error_summary.as_deref(),
        Some("background review did not survive process restart")
    );

    let reconciled = store.load_run("orphaned-background-review")?;
    assert_eq!(reconciled.status, AutoReviewRunStatus::Lost);
    assert_eq!(reconciled.freshness, AutoReviewRunFreshness::Lost);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_reconciles_another_threads_orphan_without_emitting_event() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let codex_home = Arc::new(TempDir::new()?);
    let server = responses::start_mock_server().await;
    let owner_cwd = cwd.clone();
    let owner = Box::pin(
        test_codex()
            .with_home(Arc::clone(&codex_home))
            .with_config(move |config| config.cwd = owner_cwd)
            .build(&server),
    )
    .await?;
    let owner_thread_id = owner.session_configured.thread_id.to_string();

    let store = AutoReviewStore::for_scope(codex_home.path(), repo.path());
    let run = in_flight_background_run(repo.path(), "other-thread-orphan");
    store.save_run(&run)?;
    store.update_run_state(&run.run_id, |state| {
        state.owner_thread_id = Some(owner_thread_id);
        Ok(())
    })?;

    let other_cwd = cwd.clone();
    let other = Box::pin(
        test_codex()
            .with_home(Arc::clone(&codex_home))
            .with_config(move |config| config.cwd = other_cwd)
            .build(&server),
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, other.codex.next_event()).await {
            Err(_) => break,
            Ok(Ok(event)) => {
                if let EventMsg::BackgroundAutoReviewStatus(status) = event.msg
                    && status.run_id == run.run_id
                {
                    panic!("foreign ownership routed recovery event: {status:?}");
                }
            }
            Ok(Err(err)) => panic!("background review event stream ended: {err}"),
        }
    }
    assert_eq!(
        store.load_run(&run.run_id)?.status,
        AutoReviewRunStatus::Lost
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_does_not_emit_recovery_when_owner_state_is_corrupt() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let codex_home = Arc::new(TempDir::new()?);
    let store = AutoReviewStore::for_scope(codex_home.path(), repo.path());
    let run = in_flight_background_run(repo.path(), "corrupt-owner-state");
    store.save_run(&run)?;
    store.update_run_state(&run.run_id, |state| {
        state.owner_thread_id = Some("unknown-owner".to_string());
        Ok(())
    })?;
    let state_path = store
        .runs_path()
        .parent()
        .context("auto review store root")?
        .join("run-states")
        .join(format!("{}.json", run.run_id));
    std::fs::write(state_path, "{not valid json\n")?;

    let server = responses::start_mock_server().await;
    let config_cwd = cwd.clone();
    let test = Box::pin(
        test_codex()
            .with_home(Arc::clone(&codex_home))
            .with_config(move |config| config.cwd = config_cwd)
            .build(&server),
    )
    .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, test.codex.next_event()).await {
            Err(_) => break,
            Ok(Ok(event)) => {
                if let EventMsg::BackgroundAutoReviewStatus(status) = event.msg
                    && status.run_id == run.run_id
                {
                    panic!("corrupt ownership state routed recovery event: {status:?}");
                }
            }
            Ok(Err(err)) => panic!("background review event stream ended: {err}"),
        }
    }
    assert_eq!(
        store.load_run(&run.run_id)?.status,
        AutoReviewRunStatus::Lost
    );

    Ok(())
}

/// Opening another live session for the same worktree must not mistake the
/// first session's debouncing Pending review for a process-restart orphan.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_preserves_a_pending_review_owned_by_another_live_session() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let codex_home = Arc::new(TempDir::new()?);
    let server = responses::start_mock_server().await;
    let _responses_mock =
        responses::mount_sse_sequence(&server, code_changing_turn_responses("live-pending")).await;

    let first_cwd = cwd.clone();
    let first = Box::pin(
        test_codex()
            .with_home(Arc::clone(&codex_home))
            .with_config(move |config| config.cwd = first_cwd)
            .build(&server),
    )
    .await?;
    Box::pin(submit_turn(&first.codex, &cwd, "add the feature")).await?;
    let pending = Box::pin(background_review_statuses_until(
        &first.codex,
        BackgroundAutoReviewStatus::Pending,
    ))
    .await;
    let run_id = pending[0].run_id.clone();

    let second_cwd = cwd.clone();
    let _second = Box::pin(
        test_codex()
            .with_home(Arc::clone(&codex_home))
            .with_config(move |config| config.cwd = second_cwd)
            .build(&server),
    )
    .await?;

    let store = AutoReviewStore::for_scope(codex_home.path(), repo.path());
    let run = store.load_run(&run_id)?;
    assert_ne!(run.status, AutoReviewRunStatus::Lost);
    assert_ne!(run.freshness, AutoReviewRunFreshness::Lost);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_turn_cancels_pending_review_and_releases_repo_lock() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("first");
    bodies.extend(code_changing_turn_responses_with_patch(
        "second",
        SEEDED_DEFECT_PATCH,
    ));
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-second-review"),
        responses::ev_assistant_message("msg-second-review", &review_output_json(/*findings*/ 0)),
        responses::ev_completed("resp-second-review"),
    ]));
    let _responses_mock = responses::mount_sse_sequence(&server, bodies).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;

    Box::pin(submit_turn(&test.codex, &cwd, "add the first feature")).await?;
    let first_pending = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Pending,
    ))
    .await;
    let first_run_id = first_pending[0].run_id.clone();

    Box::pin(submit_turn(&test.codex, &cwd, "add the second feature")).await?;
    let statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Completed,
    ))
    .await;
    let cancelled = statuses
        .iter()
        .filter(|status| {
            status.run_id == first_run_id && status.status == BackgroundAutoReviewStatus::Cancelled
        })
        .count();
    assert_eq!(cancelled, 1, "pending run must terminate exactly once");
    let completed = statuses
        .iter()
        .find(|status| status.status == BackgroundAutoReviewStatus::Completed)
        .context("second background review must complete")?;
    assert_ne!(completed.run_id, first_run_id);

    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let first = store.load_run(&first_run_id)?;
    assert_eq!(first.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(
        first.cancel_reason.as_deref(),
        Some("foreground_work_started")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_turn_preserves_cancel_reason_for_running_review() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let _responses_mock =
        responses::mount_sse_sequence(&server, code_changing_turn_responses("first-running")).await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;

    Box::pin(submit_turn(&test.codex, &cwd, "add the first feature")).await?;
    let first_statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Running,
    ))
    .await;
    let first_run_id = first_statuses[0].run_id.clone();

    Box::pin(submit_turn(&test.codex, &cwd, "add the second feature")).await?;
    let cancelled_statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Cancelled,
    ))
    .await;
    assert_eq!(cancelled_statuses[0].run_id, first_run_id);

    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let first = store.load_run(&first_run_id)?;
    assert_eq!(first.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(
        first.cancel_reason.as_deref(),
        Some("foreground_work_started")
    );

    Ok(())
}

/// Reproduces the August 1 mismatch with the repository review orchestrator
/// present while the detached child has no collaboration tools. The bounded
/// direct-review contract must still preserve #571's exact diff and publish the
/// seeded actionable defect instead of following the unavailable workflow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orchestration_skill_is_suppressed_while_seeded_defect_is_detected() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let skill_dir = repo.path().join(".codex/skills/code-review");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: code-review\ndescription: Run a final code review\n---\n\nUse subagents to review code.\n",
    )?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses_with_patch("seeded", SEEDED_DEFECT_PATCH);
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-seeded-review"),
        responses::ev_assistant_message(
            "msg-seeded-review",
            &seeded_defect_review_output(repo.path()),
        ),
        responses::ev_completed("resp-seeded-review"),
    ]));
    let responses_mock = responses::mount_sse_sequence(&server, bodies).await;

    let config_cwd = cwd.clone();
    let test = Box::pin(
        test_codex()
            .with_config(move |config| {
                config.cwd = config_cwd;
                config
                    .features
                    .enable(Feature::CodeMode)
                    .expect("enable code mode for mismatch reproduction");
                config
                    .features
                    .enable(Feature::CodeModeOnly)
                    .expect("enable code-mode registry for mismatch reproduction");
            })
            .build(&server),
    )
    .await?;
    Box::pin(submit_turn(&test.codex, &cwd, "add the average helper")).await?;

    let statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Completed,
    ))
    .await;
    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let run = single_run(&store);
    assert_eq!(run.run_id, statuses[0].run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    assert_eq!(run.finding_count, 1);
    assert_eq!(
        run.finding_digests[0].title,
        "[P1] Guard the zero-count average"
    );
    assert!(matches!(
        run.review_target,
        ReviewTarget::CurrentTurnDiff { .. }
    ));
    assert_eq!(
        run.target
            .current_turn
            .as_ref()
            .and_then(|target| target.diff_fingerprint.as_deref()),
        match &run.review_target {
            ReviewTarget::CurrentTurnDiff { fingerprint } => Some(fingerprint.as_str()),
            _ => None,
        }
    );

    let requests = responses_mock.requests();
    assert_eq!(
        requests.len(),
        3,
        "expected foreground tool/final plus review"
    );
    let review_body = requests[2].body_json();
    let review_body_text = review_body.to_string();
    assert!(review_body_text.contains("You are the sole reviewer"));
    assert!(review_body_text.contains("average.rs"));
    assert!(!review_body_text.contains("Use subagents to review code"));
    assert_eq!(
        review_body["max_output_tokens"].as_u64(),
        Some(65_536),
        "review request must carry a provider output cap bounded by the review output budget: {review_body:?}"
    );
    let tools = review_body["tools"]
        .as_array()
        .context("review request must contain a bounded tool registry")?;
    assert!(
        tools.iter().all(|tool| {
            !matches!(
                tool.get("name").and_then(serde_json::Value::as_str),
                Some("spawn_agent" | "exec" | "wait")
            )
        }),
        "review exposed unavailable orchestration or code-mode tools: {tools:?}"
    );

    let state = store
        .load_run_state(&run.run_id)?
        .context("completed review must persist bounded diagnostics")?;
    assert_eq!(
        state.owner_thread_id,
        Some(test.session_configured.thread_id.to_string())
    );
    assert_eq!(state.usage.orchestration_skills_suppressed, Some(true));
    assert_eq!(state.usage.request_count, Some(1));
    assert!(
        state
            .usage
            .tool_registry_tokens
            .is_some_and(|tokens| tokens < generous_budget().max_total_tokens / 10)
    );
    assert!(
        state
            .usage
            .tool_output_limit_tokens
            .is_some_and(|tokens| tokens <= 4_096)
    );
    assert!(state.usage.effective_total_token_limit.is_some());
    assert!(state.usage.accounting_tolerance_tokens.is_some());

    Ok(())
}

/// A regular turn that edits the worktree must schedule, launch, and durably
/// complete a background review with no explicit `Op::Review`, and a following
/// turn that changes nothing must not schedule anything at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_changing_turn_auto_triggers_background_review_and_no_op_turn_does_not() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    run_git(repo.path(), &["branch", "stale"])?;
    std::fs::write(repo.path().join("main-only.txt"), "newer main history\n")?;
    run_git(repo.path(), &["add", "main-only.txt"])?;
    run_git(
        repo.path(),
        &[
            "-c",
            "user.name=Codex Test",
            "-c",
            "user.email=codex@example.invalid",
            "commit",
            "-q",
            "-m",
            "advance main",
        ],
    )?;
    run_git(repo.path(), &["checkout", "-q", "stale"])?;
    std::fs::write(
        repo.path().join("README.md"),
        "initial\npreexisting tracked dirt marker\n",
    )?;
    std::fs::write(
        repo.path().join("preexisting-untracked.txt"),
        "preexisting untracked dirt marker\n",
    )?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("turn1");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review"),
        responses::ev_assistant_message("msg-review", &review_output_json(/*findings*/ 1)),
        responses::ev_completed("resp-review"),
    ]));
    bodies.push(assistant_only_turn_response("turn2"));
    let responses_mock = responses::mount_sse_sequence(&server, bodies).await;

    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;
    Box::pin(submit_turn(&test.codex, &cwd, "add the feature")).await?;

    let statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Completed,
    ))
    .await;
    assert_eq!(
        statuses
            .iter()
            .map(|status| status.status)
            .collect::<Vec<_>>(),
        vec![
            BackgroundAutoReviewStatus::Pending,
            BackgroundAutoReviewStatus::Running,
            BackgroundAutoReviewStatus::Completed,
        ]
    );
    let run_id = statuses[0].run_id.clone();
    assert!(
        statuses
            .iter()
            .all(|status| status.run_id == run_id && status.error_summary.is_none()),
        "unexpected background review statuses: {statuses:?}"
    );
    assert!(
        matches!(
            statuses[0].review_target,
            ReviewTarget::CurrentTurnDiff { .. }
        ),
        "auto-triggered review should target the current turn diff: {:?}",
        statuses[0].review_target
    );

    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let run = single_run(&store);
    assert_eq!(run.run_id, run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    assert_eq!(run.source, AutoReviewRunSource::Background);
    assert_eq!(run.review_target, statuses[0].review_target);
    assert_eq!(run.finding_count, 1);
    assert_eq!(run.error_summary, None);
    assert_eq!(run.target.branch.as_deref(), Some("stale"));
    assert!(run.target.worktree_diff_fingerprint.is_some());
    assert!(run.target.build_provenance.is_some());
    let current_turn = run
        .target
        .current_turn
        .as_ref()
        .context("current-turn identity must be durable")?;
    let ReviewTarget::CurrentTurnDiff { fingerprint } = &run.review_target else {
        unreachable!("background review target asserted above")
    };
    assert_eq!(
        current_turn.diff_fingerprint.as_deref(),
        Some(fingerprint.as_str())
    );
    assert_eq!(current_turn.changed_path_count, Some(1));
    assert!(current_turn.diff_bytes.is_some_and(|bytes| bytes > 0));
    assert_eq!(current_turn.construction_error, None);

    let review_request = responses_mock
        .last_request()
        .context("background review request must be captured")?;
    let review_input = serde_json::to_string(&review_request.input())?;
    assert!(review_input.contains("feature.rs"));
    assert!(!review_input.contains("preexisting tracked dirt marker"));
    assert!(!review_input.contains("preexisting untracked dirt marker"));

    // A turn that leaves the worktree untouched must not schedule anything.
    Box::pin(submit_turn(
        &test.codex,
        &cwd,
        "just tell me what you think",
    ))
    .await?;
    Box::pin(assert_turn_schedules_no_background_review(&test.codex)).await;
    assert_eq!(single_run(&store).run_id, run_id);

    Ok(())
}

/// A filesystem mutation that cannot produce an exact turn diff must end as a
/// durable skip; it must never substitute the now-dirty whole worktree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_exact_turn_diff_persists_skip_without_launching_review() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let shell_arguments = serde_json::json!({
        "command": "printf 'shell mutation\\n' > shell-created.txt",
        "login": false,
        "timeout_ms": 1_000,
    });
    let responses_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-shell-tool"),
                responses::ev_function_call(
                    "call-shell",
                    "shell_command",
                    &serde_json::to_string(&shell_arguments)?,
                ),
                responses::ev_completed("resp-shell-tool"),
            ]),
            assistant_only_turn_response("shell-final"),
        ],
    )
    .await;
    let test = build_codex_in_repo(&server, cwd.clone(), /*budget*/ None).await?;

    Box::pin(submit_turn(
        &test.codex,
        &cwd,
        "create a file with the shell",
    ))
    .await?;
    let statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Skipped,
    ))
    .await;

    assert_eq!(responses_mock.requests().len(), 2, "review must not launch");
    let run = single_run(&AutoReviewStore::for_scope(
        test.codex_home_path(),
        repo.path(),
    ));
    assert_eq!(run.status, AutoReviewRunStatus::Skipped);
    assert_eq!(run.run_id, statuses[0].run_id);
    assert_eq!(
        run.error_summary.as_deref(),
        Some("exact current-turn diff was unavailable; review scope was not widened")
    );
    assert!(
        run.target
            .current_turn
            .is_some_and(|target| target.construction_error.is_some())
    );

    Ok(())
}

/// A review whose findings overflow the configured budget must be discarded and
/// persisted as a terminal, budget-attributed run rather than published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_over_finding_budget_persists_terminal_run() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let run = Box::pin(run_background_review_with_budget(
        AutoReviewBudget {
            max_findings: 1,
            ..generous_budget()
        },
        review_output_json(/*findings*/ 2),
    ))
    .await?;

    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(
        run.cancel_reason.as_deref(),
        Some(AutoReviewTerminalReason::BudgetFindingCount.cancel_reason())
    );
    assert_eq!(
        run.error_summary.as_deref(),
        Some(
            "background review exceeded finding budget: 2 findings > 1 findings; retry with a narrower diff or increase `auto_review.background_max_findings`"
        )
    );
    assert_eq!(
        run.finding_count, 0,
        "over-budget findings must not persist"
    );
    assert_eq!(
        run.terminal_reason,
        Some(AutoReviewTerminalReason::BudgetFindingCount)
    );
    assert_eq!(run.usage_finding_count, Some(2));

    Ok(())
}

/// The same contract for the output-size budget, which trips before findings are
/// even parsed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_over_output_budget_persists_terminal_run() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let output = review_output_json(/*findings*/ 1);
    let run = Box::pin(run_background_review_with_budget(
        AutoReviewBudget {
            max_output_bytes: 16,
            ..generous_budget()
        },
        output.clone(),
    ))
    .await?;

    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(
        run.cancel_reason.as_deref(),
        Some(AutoReviewTerminalReason::BudgetOutput.cancel_reason())
    );
    assert_eq!(
        run.error_summary.as_deref(),
        Some(
            format!(
                "background review exceeded output budget: {} bytes > 16 bytes; retry with a narrower diff or increase `auto_review.background_max_output_bytes`",
                output.len()
            )
            .as_str()
        )
    );
    assert_eq!(
        run.terminal_reason,
        Some(AutoReviewTerminalReason::BudgetOutput)
    );
    assert_eq!(run.usage_finding_count, None);

    Ok(())
}

/// Provider usage is cumulative but arrives only after a response. A large
/// tool result must be truncated before it is considered for the next prompt,
/// and that follow-up must be rejected before sampling when the projected
/// cumulative total would cross the configured hard ceiling.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_caps_tool_output_and_rejects_projected_followup() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let command = if cfg!(windows) {
        "for ($i=1; $i -le 100000; $i++) { Write-Output $i }"
    } else {
        "seq 1 100000"
    };
    let shell_arguments = serde_json::json!({
        "command": command,
        "timeout_ms": 5_000,
    });
    let mut bodies = code_changing_turn_responses("token-cap");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review-tool"),
        responses::ev_function_call(
            "call-review-shell",
            "shell_command",
            &serde_json::to_string(&shell_arguments)?,
        ),
        responses::ev_completed_with_tokens("resp-review-tool", 19_000),
    ]));
    let responses_mock = responses::mount_sse_sequence(&server, bodies).await;
    let budget = AutoReviewBudget {
        max_total_tokens: 150_000,
        ..generous_budget()
    };
    let test = build_codex_in_repo(&server, cwd.clone(), Some(budget.clone())).await?;

    Box::pin(submit_turn(&test.codex, &cwd, "add the feature")).await?;
    let statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Cancelled,
    ))
    .await;
    assert_eq!(
        responses_mock.requests().len(),
        3,
        "projected over-budget follow-up must not reach the provider"
    );
    let review_request = responses_mock.requests()[2].body_json();
    assert!(
        review_request["max_output_tokens"]
            .as_u64()
            .is_some_and(|limit| limit < budget.max_total_tokens),
        "review response must be capped below the total budget: {review_request:?}"
    );

    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let run = single_run(&store);
    assert_eq!(run.run_id, statuses[0].run_id);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(
        run.cancel_reason.as_deref(),
        Some(AutoReviewTerminalReason::BudgetTotalTokens.cancel_reason())
    );
    assert!(run.error_summary.as_deref().is_some_and(|summary| {
        summary.contains("background_max_total_tokens")
            && summary.contains("fixed intrinsic response reservation")
    }));
    let state = store
        .load_run_state(&run.run_id)?
        .context("token-stopped run must persist projected diagnostics")?;
    assert_eq!(
        state.terminal_reason,
        Some(AutoReviewTerminalReason::BudgetTotalTokens)
    );
    assert_eq!(state.usage.total_tokens, Some(19_000));
    assert_eq!(state.usage.request_count, Some(1));
    assert_eq!(state.usage.tool_output_limit_tokens, Some(4_096));
    assert_eq!(
        state.usage.response_output_reservation_tokens,
        Some(128_000)
    );
    let tool_output_limit = state
        .usage
        .tool_output_limit_tokens
        .and_then(|tokens| u64::try_from(tokens).ok())
        .context("tool output limit must be recorded")?;
    assert!(
        state
            .usage
            .tool_output_tokens
            .is_some_and(|tokens| tokens > 0 && tokens <= tool_output_limit + 1_024),
        "tool output must be truncated before follow-up projection: {:?}",
        state.usage.tool_output_tokens
    );
    assert!(
        state
            .usage
            .projected_total_tokens
            .is_some_and(|tokens| tokens > 150_000)
    );

    Ok(())
}

/// Hitting the provider-side response cap is a budget stop, not a retryable
/// transport failure. Persist the structured token remediation without
/// resending an output-limited prompt or degrading to an empty-output error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_output_limited_response_persists_token_budget_stop() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("response-output-limit");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review-output-limited"),
        serde_json::json!({
            "type": "response.incomplete",
            "response": {
                "id": "resp-review-output-limited",
                "object": "response",
                "status": "incomplete",
                "error": null,
                "incomplete_details": {
                    "reason": "max_output_tokens"
                }
            }
        }),
    ]));
    let responses_mock = responses::mount_sse_sequence(&server, bodies).await;
    let budget = AutoReviewBudget {
        max_total_tokens: 250_000,
        ..generous_budget()
    };
    let test = build_codex_in_repo(&server, cwd.clone(), Some(budget)).await?;

    Box::pin(submit_turn(&test.codex, &cwd, "add the feature")).await?;
    Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Cancelled,
    ))
    .await;

    assert_eq!(
        responses_mock.requests().len(),
        3,
        "output-limited review request must not be retried"
    );
    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let run = single_run(&store);
    assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    assert_eq!(
        run.cancel_reason.as_deref(),
        Some(AutoReviewTerminalReason::BudgetTotalTokens.cancel_reason())
    );
    assert!(run.error_summary.as_deref().is_some_and(|summary| {
        summary.contains("background_max_total_tokens")
            && summary.contains("fixed intrinsic response reservation")
    }));
    let state = store
        .load_run_state(&run.run_id)?
        .context("output-limited review must persist token diagnostics")?;
    assert_eq!(
        state.terminal_reason,
        Some(AutoReviewTerminalReason::BudgetTotalTokens)
    );
    assert_eq!(state.usage.request_count, Some(1));
    assert_eq!(state.usage.retry_count, Some(0));
    assert!(state.usage.response_output_limit_tokens.is_some());
    assert_eq!(
        state.usage.response_output_reservation_tokens,
        Some(128_000)
    );

    Ok(())
}

/// Cumulative provider usage must replace each request's conservative
/// projection so a useful multi-tool review can complete without stacking the
/// same repeated prompt three times.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_review_accounts_multiple_requests_without_projection_stacking() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let shell_arguments = serde_json::json!({
        "command": if cfg!(windows) { "Write-Output inspected" } else { "echo inspected" },
        "timeout_ms": 5_000,
    });
    let mut bodies = code_changing_turn_responses("multi-request-budget");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review-tool-1"),
        responses::ev_function_call(
            "call-review-shell-1",
            "shell_command",
            &serde_json::to_string(&shell_arguments)?,
        ),
        responses::ev_completed_with_tokens("resp-review-tool-1", 2_000),
    ]));
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review-tool-2"),
        responses::ev_function_call(
            "call-review-shell-2",
            "shell_command",
            &serde_json::to_string(&shell_arguments)?,
        ),
        responses::ev_completed_with_tokens("resp-review-tool-2", 3_000),
    ]));
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review-final"),
        responses::ev_assistant_message(
            "msg-review-final",
            &seeded_defect_review_output(repo.path()),
        ),
        responses::ev_completed_with_tokens("resp-review-final", 4_000),
    ]));
    let responses_mock = responses::mount_sse_sequence(&server, bodies).await;
    let budget = AutoReviewBudget {
        max_total_tokens: 250_000,
        ..generous_budget()
    };
    let test = build_codex_in_repo(&server, cwd.clone(), Some(budget.clone())).await?;

    Box::pin(submit_turn(&test.codex, &cwd, "add the feature")).await?;
    Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Completed,
    ))
    .await;

    let requests = responses_mock.requests();
    assert_eq!(
        requests.len(),
        5,
        "expected two foreground and three review requests"
    );
    for request in &requests[2..] {
        assert!(
            request.body_json()["max_output_tokens"].as_u64().is_some(),
            "every review request must carry a provider output cap"
        );
    }
    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let run = single_run(&store);
    assert_eq!(run.status, AutoReviewRunStatus::Completed);
    let state = store
        .load_run_state(&run.run_id)?
        .context("multi-request review must persist accounting diagnostics")?;
    assert_eq!(state.usage.request_count, Some(3));
    assert_eq!(state.usage.retry_count, Some(0));
    assert_eq!(state.usage.total_tokens, Some(9_000));
    assert_eq!(
        state.usage.response_output_reservation_tokens,
        Some(128_000)
    );
    assert!(
        state
            .usage
            .total_tokens
            .is_some_and(|tokens| tokens <= budget.max_total_tokens)
    );
    assert!(
        state
            .usage
            .projected_total_tokens
            .is_some_and(|tokens| tokens <= budget.max_total_tokens)
    );

    Ok(())
}

fn generous_budget() -> AutoReviewBudget {
    AutoReviewBudget {
        max_scope_bytes: 1_000_000,
        max_elapsed_ms: 300_000,
        max_total_tokens: 1_000_000,
        max_output_bytes: 1_000_000,
        max_findings: 100,
    }
}

/// Flattened view of the durable state a budget stop must leave behind: the run
/// metadata plus the run-state fields that record why it stopped.
struct TerminalBackgroundReviewRun {
    status: AutoReviewRunStatus,
    cancel_reason: Option<String>,
    error_summary: Option<String>,
    finding_count: usize,
    terminal_reason: Option<AutoReviewTerminalReason>,
    usage_finding_count: Option<usize>,
}

async fn run_background_review_with_budget(
    budget: AutoReviewBudget,
    review_output: String,
) -> Result<TerminalBackgroundReviewRun> {
    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("budget");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review"),
        responses::ev_assistant_message("msg-review", &review_output),
        responses::ev_completed("resp-review"),
    ]));
    responses::mount_sse_sequence(&server, bodies).await;

    let test = build_codex_in_repo(&server, cwd.clone(), Some(budget)).await?;
    Box::pin(submit_turn(&test.codex, &cwd, "add the feature")).await?;

    let statuses = Box::pin(background_review_statuses_until(
        &test.codex,
        BackgroundAutoReviewStatus::Cancelled,
    ))
    .await;
    let run_id = statuses[0].run_id.clone();
    let store = AutoReviewStore::for_scope(test.codex_home_path(), repo.path());
    let run = single_run(&store);
    assert_eq!(run.run_id, run_id);
    let state = store
        .load_run_state(&run_id)?
        .context("budget-stopped run must persist run state")?;

    Ok(TerminalBackgroundReviewRun {
        status: run.status,
        cancel_reason: run.cancel_reason,
        error_summary: run.error_summary,
        finding_count: run.finding_count,
        terminal_reason: state.terminal_reason,
        usage_finding_count: state.usage.finding_count,
    })
}

/// The model-facing `auto_review_disposition` tool is the agent's only way to
/// acknowledge Background Review findings; its write must land in the durable
/// run state with the agent recorded as the actor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_review_disposition_tool_persists_agent_disposition() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let turn = Box::pin(run_disposition_tool_turn(
        serde_json::json!({
            "run_id": SEEDED_RUN_ID,
            "action": "defer",
            "reason": "tracked in the follow-up issue",
        }),
        SeededRun::completed_and_current(),
    ))
    .await?;

    assert!(
        turn.output.contains("\"status\":\"ok\""),
        "unexpected tool output: {}",
        turn.output
    );
    assert_eq!(
        turn.finding_disposition()?,
        AutoReviewFindingDispositionRecord {
            disposition: AutoReviewFindingDisposition::Deferred,
            actor: AutoReviewDispositionActor::Agent,
            reason: Some("tracked in the follow-up issue".to_string()),
            updated_at_unix_secs: turn.finding_disposition()?.updated_at_unix_secs,
        }
    );

    Ok(())
}

/// `repair` is the only disposition that opens a bounded repair turn, so it must
/// hand the model the finding detail alongside the persisted `repairing` record
/// and supply the default reason when the model omits one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_review_disposition_tool_repair_returns_detail_and_persists_repairing() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let turn = Box::pin(run_disposition_tool_turn(
        serde_json::json!({
            "run_id": SEEDED_RUN_ID,
            "action": "repair",
        }),
        SeededRun::completed_and_current(),
    ))
    .await?;

    let response: serde_json::Value = serde_json::from_str(&turn.output)?;
    assert_eq!(response["status"], serde_json::json!("ok"));
    assert_eq!(response["disposition"], serde_json::json!("repairing"));
    assert_eq!(
        response["reason"],
        serde_json::json!("bounded repair turn opened by agent")
    );
    let detail = response
        .get("repair_detail")
        .context("repair must return the finding detail the model repairs against")?;
    assert_eq!(detail["finding_count"], serde_json::json!(1));
    assert_eq!(detail["omitted_findings"], serde_json::json!(0));
    assert_eq!(detail["truncated"], serde_json::json!(false));
    let content = detail["content"]
        .as_str()
        .context("repair detail must carry content")?;
    assert!(
        content.contains("Guard the new branch"),
        "repair detail must describe the seeded finding: {content}"
    );

    let record = turn.finding_disposition()?;
    assert_eq!(
        record,
        AutoReviewFindingDispositionRecord {
            disposition: AutoReviewFindingDisposition::Repairing,
            actor: AutoReviewDispositionActor::Agent,
            reason: Some("bounded repair turn opened by agent".to_string()),
            updated_at_unix_secs: record.updated_at_unix_secs,
        }
    );

    Ok(())
}

/// `obsolete` is the escape hatch for runs the agent can no longer act on, so it
/// must persist even when the run is stale and `repair`/`defer` would be
/// rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_review_disposition_tool_obsolete_persists_for_stale_run() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let turn = Box::pin(run_disposition_tool_turn(
        serde_json::json!({
            "run_id": SEEDED_RUN_ID,
            "action": "obsolete",
            "reason": "  the findings no longer apply  ",
        }),
        SeededRun {
            freshness: AutoReviewRunFreshness::Obsolete,
            ..SeededRun::completed_and_current()
        },
    ))
    .await?;

    assert!(
        turn.output.contains("\"status\":\"ok\""),
        "unexpected tool output: {}",
        turn.output
    );
    let record = turn.finding_disposition()?;
    assert_eq!(
        record,
        AutoReviewFindingDispositionRecord {
            disposition: AutoReviewFindingDisposition::Obsolete,
            actor: AutoReviewDispositionActor::Agent,
            // The handler trims the model-supplied reason before persisting it.
            reason: Some("the findings no longer apply".to_string()),
            updated_at_unix_secs: record.updated_at_unix_secs,
        }
    );

    Ok(())
}

/// Repairing against a run whose findings no longer describe the worktree would
/// send the agent chasing phantom findings, so a stale run must be refused and
/// leave no disposition behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_review_disposition_tool_rejects_repair_for_stale_run() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let turn = Box::pin(run_disposition_tool_turn(
        serde_json::json!({
            "run_id": SEEDED_RUN_ID,
            "action": "repair",
        }),
        SeededRun {
            freshness: AutoReviewRunFreshness::Superseded,
            ..SeededRun::completed_and_current()
        },
    ))
    .await?;

    assert!(
        turn.output
            .contains("repair/defer requires a completed Background Review with current findings"),
        "stale run must be refused: {}",
        turn.output
    );
    assert_eq!(turn.state.and_then(|state| state.finding_disposition), None);

    Ok(())
}

/// The same refusal must apply to a run that never completed, which has no
/// durable findings to repair at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_review_disposition_tool_rejects_defer_for_non_completed_run() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let turn = Box::pin(run_disposition_tool_turn(
        serde_json::json!({
            "run_id": SEEDED_RUN_ID,
            "action": "defer",
            "reason": "will look at it later",
        }),
        SeededRun {
            status: AutoReviewRunStatus::Failed,
            ..SeededRun::completed_and_current()
        },
    ))
    .await?;

    assert!(
        turn.output
            .contains("repair/defer requires a completed Background Review with current findings"),
        "non-completed run must be refused: {}",
        turn.output
    );
    assert_eq!(turn.state.and_then(|state| state.finding_disposition), None);

    Ok(())
}

/// What the model saw and what the store kept after one `auto_review_disposition`
/// call.
struct DispositionToolTurn {
    output: String,
    state: Option<AutoReviewRunState>,
}

impl DispositionToolTurn {
    fn finding_disposition(&self) -> Result<AutoReviewFindingDispositionRecord> {
        self.state
            .as_ref()
            .context("disposition must persist run state")?
            .finding_disposition
            .clone()
            .context("disposition must be recorded")
    }
}

/// Runs one real turn whose only tool call is `auto_review_disposition` against a
/// seeded run, so the assertions cover the production handler path rather than a
/// direct call into the store.
async fn run_disposition_tool_turn(
    arguments: serde_json::Value,
    seed: SeededRun,
) -> Result<DispositionToolTurn> {
    let server = responses::start_mock_server().await;
    let call_id = "disposition-call";
    let responses_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-tool"),
                responses::ev_function_call(
                    call_id,
                    "auto_review_disposition",
                    &arguments.to_string(),
                ),
                responses::ev_completed("resp-tool"),
            ]),
            assistant_only_turn_response("disposition-final"),
        ],
    )
    .await;

    let mut builder = test_codex();
    let test = Box::pin(builder.build(&server)).await?;
    let cwd = test.config.cwd.clone();
    let store = AutoReviewStore::for_scope(test.codex_home_path(), cwd.as_path());
    seed.write(&store, cwd.as_path())?;

    Box::pin(submit_turn(
        &test.codex,
        &cwd,
        "deal with the review findings",
    ))
    .await?;
    Box::pin(core_test_support::wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    }))
    .await;

    Ok(DispositionToolTurn {
        output: responses_mock
            .function_call_output_text(call_id)
            .context("model must observe the disposition tool output")?,
        state: store.load_run_state(SEEDED_RUN_ID)?,
    })
}

const SEEDED_RUN_ID: &str = "seeded-background-review";

fn seeded_review_output() -> ReviewOutputEvent {
    ReviewOutputEvent {
        findings: vec![ReviewFinding {
            title: "Guard the new branch".to_string(),
            body: "The new branch is unreachable without a guard.".to_string(),
            confidence_score: 0.9,
            priority: 1,
            code_location: ReviewCodeLocation {
                absolute_file_path: PathBuf::from("/tmp/feature.rs"),
                line_range: ReviewLineRange { start: 1, end: 3 },
            },
        }],
        overall_correctness: "needs attention".to_string(),
        overall_explanation: "One finding needs attention.".to_string(),
        overall_confidence_score: 0.8,
    }
}

/// The lifecycle fields that decide whether the disposition tool will accept a
/// run; everything else about the seeded run is fixed.
struct SeededRun {
    status: AutoReviewRunStatus,
    freshness: AutoReviewRunFreshness,
}

impl SeededRun {
    fn completed_and_current() -> Self {
        Self {
            status: AutoReviewRunStatus::Completed,
            freshness: AutoReviewRunFreshness::Current,
        }
    }

    fn write(self, store: &AutoReviewStore, cwd: &Path) -> Result<ReviewOutputEvent> {
        seed_background_review(store, cwd, self.status, self.freshness)
    }
}

/// Writes a background review whose target matches what the session will compute
/// for a plain (non-git) workspace, so a `Current` run reads as current.
fn seed_background_review(
    store: &AutoReviewStore,
    cwd: &Path,
    status: AutoReviewRunStatus,
    freshness: AutoReviewRunFreshness,
) -> Result<ReviewOutputEvent> {
    let output = seeded_review_output();
    let digests = finding_digests(&output);
    let run = AutoReviewRun {
        schema_version: SCHEMA_VERSION,
        run_id: SEEDED_RUN_ID.to_string(),
        status,
        freshness,
        source: AutoReviewRunSource::Background,
        target: AutoReviewRunTarget {
            branch: None,
            head_sha: None,
            base_sha: None,
            worktree_path: Some(cwd.to_path_buf()),
            snapshot_epoch: None,
            snapshot_commit: None,
            head_at_launch: None,
            worktree_diff_fingerprint: None,
            current_turn: None,
            build_provenance: None,
        },
        review_target: ReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1_700_000_000,
        completed_at_unix_secs: Some(1_700_000_060),
        model: Some("mock-model".to_string()),
        reasoning_effort: None,
        prompt_token_estimate: None,
        token_count: None,
        saved_token_estimate: None,
        superseded_by: None,
        cancel_reason: None,
        error_summary: None,
        finding_count: output.findings.len(),
        finding_digests: digests,
        omitted_finding_digest_count: 0,
    };
    store.save_run(&run)?;
    store.save_output(SEEDED_RUN_ID, &output)?;
    Ok(output)
}
