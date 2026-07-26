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
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunFreshness;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::AutoReviewTerminalReason;
use codex_auto_review::SCHEMA_VERSION;
use codex_auto_review::finding_digests;
use codex_core::CodexThread;
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

/// Background Review only schedules when the worktree fingerprint changes, so
/// the turn has to run inside a real repository with a committed baseline.
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
    vec![
        responses::sse(vec![
            responses::ev_response_created(&format!("resp-{tag}-tool")),
            responses::ev_apply_patch_custom_tool_call(&format!("call-{tag}"), ADD_FEATURE_PATCH),
            responses::ev_completed(&format!("resp-{tag}-tool")),
        ]),
        responses::sse(vec![
            responses::ev_response_created(&format!("resp-{tag}-final")),
            responses::ev_assistant_message(&format!("msg-{tag}"), "added the feature"),
            responses::ev_completed(&format!("resp-{tag}-final")),
        ]),
    ]
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

/// A regular turn that edits the worktree must schedule, launch, and durably
/// complete a background review with no explicit `Op::Review`, and a following
/// turn that changes nothing must not schedule anything at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_changing_turn_auto_triggers_background_review_and_no_op_turn_does_not() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let repo = create_git_repo()?;
    let cwd = AbsolutePathBuf::try_from(repo.path().to_path_buf())?;
    let server = responses::start_mock_server().await;
    let mut bodies = code_changing_turn_responses("turn1");
    bodies.push(responses::sse(vec![
        responses::ev_response_created("resp-review"),
        responses::ev_assistant_message("msg-review", &review_output_json(/*findings*/ 1)),
        responses::ev_completed("resp-review"),
    ]));
    bodies.push(assistant_only_turn_response("turn2"));
    responses::mount_sse_sequence(&server, bodies).await;

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
        Some("background review exceeded finding budget: 2 findings > 1 findings")
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
                "background review exceeded output budget: {} bytes > 16 bytes",
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
                    &serde_json::json!({
                        "run_id": SEEDED_RUN_ID,
                        "action": "defer",
                        "reason": "tracked in the follow-up issue",
                    })
                    .to_string(),
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
    seed_completed_background_review(&store, cwd.as_path())?;

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

    let output = responses_mock
        .function_call_output_text(call_id)
        .context("model must observe the disposition tool output")?;
    assert!(
        output.contains("\"status\":\"ok\""),
        "unexpected tool output: {output}"
    );

    let state = store
        .load_run_state(SEEDED_RUN_ID)?
        .context("disposition must persist run state")?;
    let record = state
        .finding_disposition
        .context("disposition must be recorded")?;
    assert_eq!(record.disposition, AutoReviewFindingDisposition::Deferred);
    assert_eq!(record.actor, AutoReviewDispositionActor::Agent);
    assert_eq!(
        record.reason.as_deref(),
        Some("tracked in the follow-up issue")
    );

    Ok(())
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

/// Writes a completed background review whose target matches what the session
/// will compute for a plain (non-git) workspace, so the run reads as current.
fn seed_completed_background_review(
    store: &AutoReviewStore,
    cwd: &Path,
) -> Result<ReviewOutputEvent> {
    let output = seeded_review_output();
    let digests = finding_digests(&output);
    let run = AutoReviewRun {
        schema_version: SCHEMA_VERSION,
        run_id: SEEDED_RUN_ID.to_string(),
        status: AutoReviewRunStatus::Completed,
        freshness: AutoReviewRunFreshness::Current,
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
