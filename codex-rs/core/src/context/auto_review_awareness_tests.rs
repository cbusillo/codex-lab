use std::path::PathBuf;

use codex_auto_review::AutoReviewFindingRecord;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
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

    assert!(body.contains("background/completed/stale/target_match: 1"));
    assert!(!body.contains("Stale title"));
    assert!(!body.contains("stale body"));
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
async fn awareness_is_absent_when_store_is_unreadable() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let repo = TempDir::new()?;
    let runs_dir = codex_home.path().join("auto-review").join("runs");
    std::fs::create_dir_all(&runs_dir)?;
    std::fs::write(runs_dir.join("broken.json"), "not json")?;

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
    findings: Vec<AutoReviewFindingRecord>,
) -> AutoReviewRun {
    AutoReviewRun {
        schema_version: codex_auto_review::SCHEMA_VERSION,
        run_id: run_id.to_string(),
        status,
        source: AutoReviewRunSource::Background,
        target: sample_target("main", "head-2", "/repo"),
        review_target: ReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1,
        completed_at_unix_secs: Some(2),
        model: Some("gpt-test".to_string()),
        error_summary: None,
        findings,
    }
}

fn sample_target(branch: &str, head_sha: &str, worktree_path: &str) -> AutoReviewRunTarget {
    AutoReviewRunTarget {
        branch: Some(branch.to_string()),
        head_sha: Some(head_sha.to_string()),
        base_sha: Some("base-1".to_string()),
        worktree_path: Some(PathBuf::from(worktree_path)),
        worktree_diff_fingerprint: None,
    }
}

fn sample_finding(finding_id: &str, title: &str, body: &str) -> AutoReviewFindingRecord {
    AutoReviewFindingRecord {
        finding_id: finding_id.to_string(),
        finding: ReviewFinding {
            title: title.to_string(),
            body: body.to_string(),
            confidence_score: 0.9,
            priority: 1,
            code_location: ReviewCodeLocation {
                absolute_file_path: PathBuf::from("/tmp/example.rs"),
                line_range: ReviewLineRange { start: 7, end: 9 },
            },
        },
    }
}
