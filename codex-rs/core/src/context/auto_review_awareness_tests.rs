use std::path::PathBuf;

use codex_auto_review::AutoReviewFindingDigest;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
use tempfile::TempDir;

use crate::context::ContextualUserFragment;
use crate::state::BackgroundAutoReviewActiveSnapshot;

use super::AutoReviewAwareness;
use super::build_auto_review_awareness;
use super::render_awareness;

#[test]
fn awareness_renders_marked_bounded_context() {
    let awareness = AutoReviewAwareness::new("Auto Review awareness:\n- status").unwrap();

    assert_eq!(awareness.role(), "user");
    assert!(
        awareness
            .render()
            .starts_with("<auto_review_awareness>\nAuto Review awareness:")
    );
    assert!(awareness.render().ends_with("\n</auto_review_awareness>"));
}

#[test]
fn awareness_includes_current_summary_without_finding_body() {
    let run = sample_run(
        "run_current",
        AutoReviewRunStatus::Completed,
        vec![sample_finding(
            "f1",
            "Use checked add",
            "long body must stay hidden",
        )],
    );

    let awareness = render_awareness(
        &[run],
        &sample_target("main", "head-2", "/repo"),
        &ReviewTarget::UncommittedChanges,
        &BackgroundAutoReviewActiveSnapshot::default(),
    )
    .expect("current findings should render awareness");
    let body = awareness.body();

    assert!(body.contains("current findings from run run_current"));
    assert!(body.contains("[P1] f1: Use checked add"));
    assert!(body.contains("run_id/finding_id"));
    assert!(!body.contains("long body must stay hidden"));
}

#[test]
fn awareness_treats_current_turn_diff_as_current_uncommitted_target() {
    let run = AutoReviewRun {
        target: AutoReviewRunTarget {
            worktree_diff_fingerprint: Some("sha256:turn".to_string()),
            ..sample_target("main", "head-2", "/repo")
        },
        review_target: ReviewTarget::CurrentTurnDiff {
            fingerprint: "sha256:turn".to_string(),
        },
        ..sample_run(
            "run_current",
            AutoReviewRunStatus::Completed,
            vec![sample_finding("f1", "Use checked add", "hidden body")],
        )
    };
    let active_target = AutoReviewRunTarget {
        worktree_diff_fingerprint: Some("sha256:turn".to_string()),
        ..sample_target("main", "head-2", "/repo")
    };

    let awareness = render_awareness(
        &[run],
        &active_target,
        &ReviewTarget::UncommittedChanges,
        &BackgroundAutoReviewActiveSnapshot::default(),
    )
    .expect("current turn diff findings should render awareness");
    let body = awareness.body();

    assert!(body.contains("current findings from run run_current"));
    assert!(body.contains("[P1] f1: Use checked add"));
    assert!(!body.contains("hidden body"));
}

#[test]
fn awareness_reports_stale_status_without_stale_findings() {
    let stale_run = AutoReviewRun {
        target: sample_target("main", "head-1", "/repo"),
        ..sample_run(
            "run_stale",
            AutoReviewRunStatus::Completed,
            vec![sample_finding("f1", "Stale title", "stale body")],
        )
    };

    let awareness = render_awareness(
        &[stale_run],
        &sample_target("main", "head-2", "/repo"),
        &ReviewTarget::UncommittedChanges,
        &BackgroundAutoReviewActiveSnapshot::default(),
    )
    .expect("stale status count should render awareness");
    let body = awareness.body();

    assert!(body.contains("background/completed/stale/off_target: 1"));
    assert!(body.contains(
        "recent run diagnostics: recent_runs=1 in_flight=0 terminal=1 suppressed_stale=1"
    ));
    assert!(!body.contains("Stale title"));
    assert!(!body.contains("stale body"));
}

#[test]
fn awareness_reports_duplicate_skipped_diagnostics() {
    let duplicate_skipped = AutoReviewRun {
        status: AutoReviewRunStatus::Skipped,
        freshness: codex_auto_review::AutoReviewRunFreshness::Superseded,
        superseded_by: Some("existing-run".to_string()),
        cancel_reason: Some("duplicate_auto_review_scope".to_string()),
        error_summary: Some("equivalent background auto review already exists".to_string()),
        finding_count: 0,
        finding_digests: Vec::new(),
        ..sample_run("duplicate-skip", AutoReviewRunStatus::Skipped, Vec::new())
    };

    let awareness = render_awareness(
        &[duplicate_skipped],
        &sample_target("main", "head-2", "/repo"),
        &ReviewTarget::UncommittedChanges,
        &BackgroundAutoReviewActiveSnapshot::default(),
    )
    .expect("duplicate skipped status should render awareness");
    let body = awareness.body();

    assert!(body.contains("background/skipped/stale/off_target: 1"));
    assert!(body.contains(
        "recent run diagnostics: recent_runs=1 in_flight=0 terminal=1 skipped=1 duplicate_skipped=1"
    ));
}

#[test]
fn awareness_keeps_current_findings_even_with_many_unrelated_newer_runs() {
    let mut runs = (0..30)
        .map(|index| AutoReviewRun {
            run_id: format!("unrelated_{index}"),
            started_at_unix_secs: 100 + index,
            target: sample_target("other", "head-other", "/other"),
            ..sample_run("unused", AutoReviewRunStatus::Completed, Vec::new())
        })
        .collect::<Vec<_>>();
    runs.push(sample_run(
        "older_current",
        AutoReviewRunStatus::Completed,
        vec![sample_finding("f1", "Older current title", "hidden body")],
    ));

    let awareness = render_awareness(
        &runs,
        &sample_target("main", "head-2", "/repo"),
        &ReviewTarget::UncommittedChanges,
        &BackgroundAutoReviewActiveSnapshot::default(),
    )
    .expect("older current findings should render despite unrelated runs");
    let body = awareness.body();

    assert!(body.contains("current findings from run older_current"));
    assert!(body.contains("Older current title"));
    assert!(!body.contains("hidden body"));
}

#[test]
fn awareness_uses_newest_current_run_findings() {
    let older_run = AutoReviewRun {
        started_at_unix_secs: 10,
        completed_at_unix_secs: Some(20),
        ..sample_run(
            "z_older_current",
            AutoReviewRunStatus::Completed,
            vec![sample_finding("f1", "Older current title", "hidden older")],
        )
    };
    let newer_run = AutoReviewRun {
        started_at_unix_secs: 30,
        completed_at_unix_secs: Some(40),
        ..sample_run(
            "a_newer_current",
            AutoReviewRunStatus::Completed,
            vec![sample_finding("f1", "Newer current title", "hidden newer")],
        )
    };

    let awareness = render_awareness(
        &[older_run, newer_run],
        &sample_target("main", "head-2", "/repo"),
        &ReviewTarget::UncommittedChanges,
        &BackgroundAutoReviewActiveSnapshot::default(),
    )
    .expect("newer current findings should render awareness");
    let body = awareness.body();

    assert!(body.contains("current findings from run a_newer_current"));
    assert!(body.contains("Newer current title"));
    assert!(!body.contains("Older current title"));
    assert!(!body.contains("hidden newer"));
}

#[test]
fn awareness_reports_live_pending_and_running_runs() {
    let awareness = render_awareness(
        &[],
        &sample_target("main", "head-2", "/repo"),
        &ReviewTarget::UncommittedChanges,
        &BackgroundAutoReviewActiveSnapshot {
            pending_run_id: Some("pending_1".to_string()),
            running_run_id: Some("running_1".to_string()),
        },
    )
    .expect("live status should render awareness");
    let body = awareness.body();

    assert!(body.contains("pending background run: pending_1"));
    assert!(body.contains("running background run: running_1"));
}

#[tokio::test]
async fn awareness_is_absent_when_store_and_snapshot_are_empty() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let repo = TempDir::new()?;

    let awareness = build_auto_review_awareness(
        codex_home.path(),
        repo.path(),
        BackgroundAutoReviewActiveSnapshot::default(),
    )
    .await;

    assert_eq!(awareness, None);
    Ok(())
}

#[tokio::test]
async fn awareness_is_absent_when_store_is_unreadable() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let repo = TempDir::new()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), repo.path());
    let runs_path = store.runs_path();
    std::fs::create_dir_all(runs_path.parent().expect("runs path parent"))?;
    std::fs::write(runs_path, "not json")?;

    let awareness = build_auto_review_awareness(
        codex_home.path(),
        repo.path(),
        BackgroundAutoReviewActiveSnapshot::default(),
    )
    .await;

    assert_eq!(awareness, None);
    Ok(())
}

fn sample_run(
    run_id: &str,
    status: AutoReviewRunStatus,
    findings: Vec<AutoReviewFindingDigest>,
) -> AutoReviewRun {
    AutoReviewRun {
        schema_version: codex_auto_review::SCHEMA_VERSION,
        run_id: run_id.to_string(),
        status,
        freshness: codex_auto_review::AutoReviewRunFreshness::Current,
        source: AutoReviewRunSource::Background,
        target: sample_target("main", "head-2", "/repo"),
        review_target: ReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1,
        completed_at_unix_secs: Some(2),
        model: Some("gpt-test".to_string()),
        reasoning_effort: None,
        prompt_token_estimate: None,
        token_count: None,
        saved_token_estimate: None,
        superseded_by: None,
        cancel_reason: None,
        error_summary: None,
        finding_count: findings.len(),
        omitted_finding_digest_count: 0,
        finding_digests: findings,
    }
}

fn sample_target(branch: &str, head_sha: &str, worktree_path: &str) -> AutoReviewRunTarget {
    AutoReviewRunTarget {
        branch: Some(branch.to_string()),
        head_sha: Some(head_sha.to_string()),
        base_sha: Some("base-1".to_string()),
        worktree_path: Some(PathBuf::from(worktree_path)),
        snapshot_epoch: None,
        snapshot_commit: None,
        head_at_launch: None,
        worktree_diff_fingerprint: None,
    }
}

fn sample_finding(finding_id: &str, title: &str, body: &str) -> AutoReviewFindingDigest {
    let finding = ReviewFinding {
        title: title.to_string(),
        body: body.to_string(),
        confidence_score: 0.9,
        priority: 1,
        code_location: ReviewCodeLocation {
            absolute_file_path: PathBuf::from("/tmp/example.rs"),
            line_range: ReviewLineRange { start: 7, end: 9 },
        },
    };
    codex_auto_review::finding_digests(&ReviewOutputEvent {
        findings: vec![finding],
        overall_correctness: String::new(),
        overall_explanation: String::new(),
        overall_confidence_score: 0.0,
    })
    .into_iter()
    .next()
    .map(|digest| AutoReviewFindingDigest {
        finding_id: finding_id.to_string(),
        ..digest
    })
    .expect("digest")
}
