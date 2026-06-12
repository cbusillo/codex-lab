use std::path::PathBuf;

use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewTarget;
use pretty_assertions::assert_eq;

use super::AutoReviewDetail;
use super::AutoReviewFindingRecord;
use super::AutoReviewFreshness;
use super::AutoReviewRun;
use super::AutoReviewRunSource;
use super::AutoReviewRunStatus;
use super::AutoReviewRunTarget;
use super::AutoReviewStore;
use super::DETAIL_MAX_BYTES;
use super::SCHEMA_VERSION;

#[test]
fn save_and_load_run_round_trips_under_codex_home() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let store = AutoReviewStore::new(codex_home.path());
    let run = sample_run("run_1", vec![sample_finding("f1", "Title")]);

    let path = store.save_run(&run)?;
    let loaded = store.load_run("run_1")?;

    assert_eq!(loaded, run);
    assert_eq!(
        path,
        codex_home
            .path()
            .join("auto-review")
            .join("runs")
            .join("run_1.json")
    );
    Ok(())
}

#[test]
fn unsafe_run_ids_are_rejected() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let store = AutoReviewStore::new(codex_home.path());
    let run = sample_run("../run", Vec::new());

    let error = store.save_run(&run).expect_err("unsafe id should fail");

    assert!(error.to_string().contains("auto review run_id"));
}

#[test]
fn unsafe_and_duplicate_finding_ids_are_rejected() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let store = AutoReviewStore::new(codex_home.path());
    let unsafe_id = sample_run("run_1", vec![sample_finding("../f1", "Title")]);
    let duplicate_id = sample_run(
        "run_1",
        vec![sample_finding("f1", "Title"), sample_finding("f1", "Other")],
    );

    let unsafe_error = store
        .save_run(&unsafe_id)
        .expect_err("unsafe finding id should fail");
    let duplicate_error = store
        .save_run(&duplicate_id)
        .expect_err("duplicate finding id should fail");

    assert!(unsafe_error.to_string().contains("auto review finding_id"));
    assert!(
        duplicate_error
            .to_string()
            .contains("duplicate auto review finding id: f1")
    );
}

#[test]
fn unsupported_schema_versions_are_rejected() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let store = AutoReviewStore::new(codex_home.path());
    let run = AutoReviewRun {
        schema_version: SCHEMA_VERSION + 1,
        ..sample_run("run_1", Vec::new())
    };

    let error = store
        .save_run(&run)
        .expect_err("unsupported schema should fail");

    assert!(
        error
            .to_string()
            .contains("unsupported auto review schema version")
    );
}

#[test]
fn freshness_classifies_current_stale_and_detached() {
    let active = sample_target("main", "head-2", "/repo");
    let current = sample_target("main", "head-2", "/repo");
    let stale = sample_target("main", "head-1", "/repo");
    let stale_base = AutoReviewRunTarget {
        base_sha: Some("base-0".to_string()),
        ..sample_target("main", "head-2", "/repo")
    };
    let detached_branch = sample_target("feature", "head-2", "/repo");
    let detached_worktree = sample_target("main", "head-2", "/other");

    assert_eq!(current.freshness(&active), AutoReviewFreshness::Current);
    assert_eq!(stale.freshness(&active), AutoReviewFreshness::Stale);
    assert_eq!(stale_base.freshness(&active), AutoReviewFreshness::Stale);
    assert_eq!(
        detached_branch.freshness(&active),
        AutoReviewFreshness::Detached
    );
    assert_eq!(
        detached_worktree.freshness(&active),
        AutoReviewFreshness::Detached
    );
}

#[test]
fn summary_only_surfaces_current_findings() {
    let active = sample_target("main", "head-2", "/repo");
    let mut run = sample_run("run_1", vec![sample_finding("f1", "Title")]);
    run.target = sample_target("main", "head-1", "/repo");

    assert_eq!(
        run.summary(&active, &ReviewTarget::UncommittedChanges)
            .content,
        ""
    );

    run.target = active.clone();
    assert_eq!(
        run.summary(&active, &ReviewTarget::UncommittedChanges)
            .content,
        "[P1] f1: Title (/tmp/example.rs:7-9)"
    );
}

#[test]
fn summary_hides_findings_for_mismatched_review_target() {
    let active = sample_target("main", "head-2", "/repo");
    let run = sample_run("run_1", vec![sample_finding("f1", "Title")]);

    assert_eq!(
        run.summary(
            &active,
            &ReviewTarget::BaseBranch {
                branch: "main".to_string()
            }
        )
        .content,
        ""
    );
}

#[test]
fn summary_marks_omitted_findings_when_count_cap_is_hit() {
    let active = sample_target("main", "head-2", "/repo");
    let findings = (0..25)
        .map(|index| sample_finding(&format!("f{index}"), &format!("Title {index}")))
        .collect();
    let run = sample_run("run_1", findings);

    let summary = run.summary(&active, &ReviewTarget::UncommittedChanges);

    assert_eq!(summary.rendered_findings, 20);
    assert_eq!(summary.omitted_findings, 5);
    assert!(summary.content.contains("... 5 more finding(s) omitted"));
}

#[test]
fn summary_marks_omitted_findings_when_byte_cap_is_hit() {
    let active = sample_target("main", "head-2", "/repo");
    let findings = (0..20)
        .map(|index| sample_finding(&format!("f{index}"), &"x".repeat(1000)))
        .collect();
    let run = sample_run("run_1", findings);

    let summary = run.summary(&active, &ReviewTarget::UncommittedChanges);

    assert!(summary.truncated);
    assert!(summary.omitted_findings > 0);
    assert!(summary.content.contains("more finding(s) omitted"));
}

#[test]
fn detail_lookup_returns_bounded_json() -> anyhow::Result<()> {
    let run = sample_run("run_1", vec![sample_finding("f1", &"x".repeat(200))]);

    let detail = run.finding_detail("f1", 120)?;

    assert_eq!(
        detail,
        AutoReviewDetail {
            finding_id: "f1".to_string(),
            bytes: detail.content.len(),
            original_bytes: detail.original_bytes,
            max_bytes: 120,
            truncated: true,
            content: detail.content.clone(),
        }
    );
    assert!(detail.original_bytes > detail.bytes);
    assert!(detail.content.len() <= 120);
    Ok(())
}

#[test]
fn detail_lookup_clamps_to_crate_owned_hard_cap() -> anyhow::Result<()> {
    let run = sample_run(
        "run_1",
        vec![sample_finding("f1", &"x".repeat(DETAIL_MAX_BYTES * 2))],
    );

    let detail = run.finding_detail("f1", DETAIL_MAX_BYTES * 10)?;

    assert_eq!(detail.max_bytes, DETAIL_MAX_BYTES);
    assert!(detail.bytes <= DETAIL_MAX_BYTES);
    assert!(detail.truncated);
    Ok(())
}

#[test]
fn detail_lookup_rejects_unknown_ids_and_empty_budget() {
    let run = sample_run("run_1", vec![sample_finding("f1", "Title")]);

    let missing = run
        .finding_detail("missing", 120)
        .expect_err("missing finding should fail");
    let empty_budget = run
        .finding_detail("f1", /*max_bytes*/ 0)
        .expect_err("empty budget should fail");

    assert!(
        missing
            .to_string()
            .contains("unknown auto review finding id: missing")
    );
    assert!(
        empty_budget
            .to_string()
            .contains("auto review detail max_bytes must be positive")
    );
}

fn sample_run(run_id: &str, findings: Vec<AutoReviewFindingRecord>) -> AutoReviewRun {
    AutoReviewRun {
        schema_version: SCHEMA_VERSION,
        run_id: run_id.to_string(),
        status: AutoReviewRunStatus::Completed,
        source: AutoReviewRunSource::Manual,
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
    }
}

fn sample_finding(finding_id: &str, title: &str) -> AutoReviewFindingRecord {
    AutoReviewFindingRecord {
        finding_id: finding_id.to_string(),
        finding: ReviewFinding {
            title: title.to_string(),
            body: "Body".to_string(),
            confidence_score: 0.9,
            priority: 1,
            code_location: ReviewCodeLocation {
                absolute_file_path: PathBuf::from("/tmp/example.rs"),
                line_range: ReviewLineRange { start: 7, end: 9 },
            },
        },
    }
}
