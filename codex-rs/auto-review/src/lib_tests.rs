use std::path::PathBuf;

use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
use pretty_assertions::assert_eq;

use super::AutoReviewDiagnostics;
use super::AutoReviewDuplicateDisposition;
use super::AutoReviewFreshness;
use super::AutoReviewRun;
use super::AutoReviewRunFreshness;
use super::AutoReviewRunSource;
use super::AutoReviewRunStatus;
use super::AutoReviewRunTarget;
use super::AutoReviewStore;
use super::DETAIL_MAX_BYTES;
use super::SCHEMA_VERSION;
use super::finding_digests;

#[test]
fn save_and_load_run_round_trips_compact_index() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Title")]);
    let run = sample_run("run_1", &output);

    let path = store.save_run(&run)?;
    store.save_output("run_1", &output)?;

    let loaded = store.load_run("run_1")?;
    let index_text = std::fs::read_to_string(store.runs_path())?;
    let sidecar_text = std::fs::read_to_string(store.output_path("run_1")?)?;

    assert_eq!(loaded, run);
    assert_eq!(path, store.runs_path());
    assert!(path.ends_with("auto-review/runs.json"));
    assert!(index_text.contains("finding_count"));
    assert!(index_text.contains("finding_digests"));
    assert!(!index_text.contains("Body Title"));
    assert!(sidecar_text.contains("Body Title"));
    Ok(())
}

#[test]
fn scoped_stores_separate_repos_under_one_home() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope_a = tempfile::tempdir()?;
    let scope_b = tempfile::tempdir()?;
    let store_a = AutoReviewStore::for_scope(codex_home.path(), scope_a.path());
    let store_b = AutoReviewStore::for_scope(codex_home.path(), scope_b.path());

    store_a.save_run(&sample_run("run_a", &sample_output(Vec::new())))?;
    store_b.save_run(&sample_run("run_b", &sample_output(Vec::new())))?;

    assert_eq!(run_ids(store_a.list_runs()?), vec!["run_a".to_string()]);
    assert_eq!(run_ids(store_b.list_runs()?), vec!["run_b".to_string()]);
    assert_ne!(store_a.runs_path(), store_b.runs_path());
    Ok(())
}

#[test]
fn list_runs_returns_valid_json_runs_in_id_order() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_b", &sample_output(Vec::new())))?;
    store.save_run(&sample_run("run_a", &sample_output(Vec::new())))?;

    assert_eq!(
        run_ids(store.list_runs()?),
        vec!["run_a".to_string(), "run_b".to_string()]
    );
    Ok(())
}

#[test]
fn canonical_store_does_not_recover_index_from_output_sidecars() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Sidecar only")]);
    store.save_output("run_sidecar", &output)?;

    let runs = store.list_runs()?;

    assert!(runs.is_empty());
    Ok(())
}

#[test]
fn canonical_store_ignores_legacy_unscoped_runs() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let legacy_dir = codex_home.path().join("auto-review").join("runs");
    std::fs::create_dir_all(&legacy_dir)?;
    std::fs::write(legacy_dir.join("legacy_run.json"), "{}\n")?;

    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());

    assert!(store.list_runs()?.is_empty());
    assert!(!AutoReviewStore::has_store_files(codex_home.path()));
    Ok(())
}

#[test]
fn corrupt_sidecar_does_not_block_store_listing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    let bad_path = store.output_path("bad_run")?;
    std::fs::create_dir_all(bad_path.parent().expect("output path parent"))?;
    std::fs::write(&bad_path, "not json\n")?;

    assert_eq!(run_ids(store.list_runs()?), vec!["run_1".to_string()]);
    Ok(())
}

#[test]
fn corrupt_index_is_a_read_error() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    corrupt_runs_index(&store)?;

    let error = store
        .list_runs()
        .expect_err("corrupt canonical index should be explicit");

    assert!(
        error
            .to_string()
            .contains("failed to parse auto review runs index"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn non_canonical_index_shape_is_a_read_error() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let mut value = serde_json::to_value(sample_run(
        "run_1",
        &sample_output(vec![sample_finding("Body only")]),
    ))?;
    value["findings"] = serde_json::json!([{ "title": "old shape" }]);
    let index = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "runs": [value],
    });
    std::fs::create_dir_all(store.runs_path().parent().expect("runs parent"))?;
    std::fs::write(store.runs_path(), serde_json::to_vec_pretty(&index)?)?;

    let error = store
        .list_runs()
        .expect_err("non-canonical compact index should be explicit");

    assert!(
        error
            .to_string()
            .contains("failed to parse auto review runs index"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn finding_detail_reads_completed_output_sidecar() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding(&"x".repeat(200))]);
    let run = sample_run("run_1", &output);
    store.save_run(&run)?;
    store.save_output("run_1", &output)?;

    let detail = store.finding_detail("run_1", "f1", 120)?;

    assert_eq!(detail.finding_id, "f1");
    assert!(detail.truncated);
    assert!(detail.bytes <= 120);
    Ok(())
}

#[test]
fn finding_detail_requires_completed_run_and_sidecar() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = AutoReviewRun {
        status: AutoReviewRunStatus::Cancelled,
        ..sample_run("run_1", &sample_output(vec![sample_finding("Title")]))
    };
    store.save_run(&run)?;

    let not_completed = store
        .finding_detail("run_1", "f1", DETAIL_MAX_BYTES)
        .expect_err("non-completed runs should not expose finding detail");

    assert!(
        not_completed
            .to_string()
            .contains("auto review run is not completed: run_1")
    );
    Ok(())
}

#[test]
fn diagnostics_counts_terminal_skipped_duplicates_and_stale_suppression() {
    let active_target = sample_target("main", "head-2", "/repo");
    let stale_finding = AutoReviewRun {
        run_id: "stale_finding".to_string(),
        target: sample_target("main", "head-1", "/repo"),
        ..sample_run("unused", &sample_output(vec![sample_finding("Stale")]))
    };
    let duplicate_skipped = AutoReviewRun {
        run_id: "duplicate_skipped".to_string(),
        status: AutoReviewRunStatus::Skipped,
        freshness: AutoReviewRunFreshness::Superseded,
        superseded_by: Some("existing".to_string()),
        cancel_reason: Some("duplicate_auto_review_scope".to_string()),
        completed_at_unix_secs: Some(3),
        finding_count: 0,
        finding_digests: Vec::new(),
        ..sample_run("unused", &sample_output(Vec::new()))
    };
    let failed = AutoReviewRun {
        run_id: "failed".to_string(),
        status: AutoReviewRunStatus::Failed,
        completed_at_unix_secs: Some(4),
        finding_count: 0,
        finding_digests: Vec::new(),
        ..sample_run("unused", &sample_output(Vec::new()))
    };
    let running = AutoReviewRun {
        run_id: "running".to_string(),
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        finding_count: 0,
        finding_digests: Vec::new(),
        ..sample_run("unused", &sample_output(Vec::new()))
    };

    let diagnostics = AutoReviewDiagnostics::from_runs(
        [&stale_finding, &duplicate_skipped, &failed, &running],
        Some(&active_target),
        Some(&ReviewTarget::UncommittedChanges),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.recent_runs, 4);
    assert_eq!(diagnostics.in_flight_runs, 1);
    assert_eq!(diagnostics.terminal_runs, 3);
    assert_eq!(diagnostics.skipped_runs, 1);
    assert_eq!(diagnostics.duplicate_skipped_runs, 1);
    assert_eq!(diagnostics.failed_runs, 1);
    assert_eq!(diagnostics.suppressed_stale_runs, 1);
    assert_eq!(
        diagnostics.compact_line(),
        "recent_runs=4 in_flight=1 terminal=3 suppressed_stale=1 skipped=1 duplicate_skipped=1 failed=1"
    );
}

#[test]
fn diagnostics_are_absent_for_empty_runs() {
    assert_eq!(AutoReviewDiagnostics::from_runs([], None, None), None);
}

#[test]
fn detail_lookup_clamps_to_crate_owned_hard_cap() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding(&"x".repeat(DETAIL_MAX_BYTES * 2))]);
    let run = sample_run("run_1", &output);
    store.save_run(&run)?;
    store.save_output("run_1", &output)?;

    let detail = store.finding_detail("run_1", "f1", DETAIL_MAX_BYTES * 10)?;

    assert_eq!(detail.max_bytes, DETAIL_MAX_BYTES);
    assert!(detail.bytes <= DETAIL_MAX_BYTES);
    assert!(detail.truncated);
    Ok(())
}

#[test]
fn detail_lookup_rejects_unknown_ids_and_empty_budget() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Title")]);
    let run = sample_run("run_1", &output);
    store.save_run(&run)?;
    store.save_output("run_1", &output)?;

    let missing = store
        .finding_detail("run_1", "missing", 120)
        .expect_err("missing finding should fail");
    let empty_budget = store
        .finding_detail("run_1", "f1", /*max_bytes*/ 0)
        .expect_err("empty budget should fail");

    assert!(
        missing
            .to_string()
            .contains("invalid auto review finding id: missing")
    );
    assert!(
        empty_budget
            .to_string()
            .contains("auto review detail max_bytes must be positive")
    );
    Ok(())
}

#[test]
fn summary_only_surfaces_current_finding_digests() {
    let active = sample_target("main", "head-2", "/repo");
    let mut run = sample_run("run_1", &sample_output(vec![sample_finding("Title")]));
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
fn summary_treats_current_turn_diff_as_current_uncommitted_target() {
    let active = target_with_fingerprint("sha256:turn");
    let run = AutoReviewRun {
        target: target_with_fingerprint("sha256:turn"),
        review_target: ReviewTarget::CurrentTurnDiff {
            fingerprint: "sha256:turn".to_string(),
        },
        ..sample_run("run_1", &sample_output(vec![sample_finding("Title")]))
    };

    assert_eq!(
        run.summary(&active, &ReviewTarget::UncommittedChanges)
            .content,
        "[P1] f1: Title (/tmp/example.rs:7-9)"
    );
}

#[test]
fn summary_hides_findings_for_mismatched_review_target() {
    let active = sample_target("main", "head-2", "/repo");
    let run = sample_run("run_1", &sample_output(vec![sample_finding("Title")]));

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
fn summary_hides_findings_when_worktree_diff_fingerprint_changes() {
    let active = AutoReviewRunTarget {
        worktree_diff_fingerprint: Some("sha256:new".to_string()),
        ..sample_target("main", "head-2", "/repo")
    };
    let run = AutoReviewRun {
        target: AutoReviewRunTarget {
            worktree_diff_fingerprint: Some("sha256:old".to_string()),
            ..sample_target("main", "head-2", "/repo")
        },
        ..sample_run("run_1", &sample_output(vec![sample_finding("Title")]))
    };

    assert_eq!(run.freshness(&active), AutoReviewFreshness::Stale);
    assert!(
        run.visible_finding_digests(&active, &ReviewTarget::UncommittedChanges)
            .is_empty()
    );
}

#[test]
fn visible_finding_digests_require_completed_run_status() {
    let active = sample_target("main", "head-2", "/repo");
    let active_review_target = ReviewTarget::UncommittedChanges;

    for status in [
        AutoReviewRunStatus::Pending,
        AutoReviewRunStatus::Snapshotting,
        AutoReviewRunStatus::Running,
        AutoReviewRunStatus::Reviewing,
        AutoReviewRunStatus::Resolving,
        AutoReviewRunStatus::Failed,
        AutoReviewRunStatus::Cancelled,
        AutoReviewRunStatus::Superseded,
        AutoReviewRunStatus::Skipped,
        AutoReviewRunStatus::Lost,
    ] {
        let run = AutoReviewRun {
            status,
            ..sample_run("run_1", &sample_output(vec![sample_finding("Title")]))
        };

        assert!(
            run.visible_finding_digests(&active, &active_review_target)
                .is_empty()
        );
        assert_eq!(run.summary(&active, &active_review_target).content, "");
    }
}

#[test]
fn summary_marks_omitted_findings_when_count_cap_is_hit() {
    let active = sample_target("main", "head-2", "/repo");
    let output = sample_output(
        (0..25)
            .map(|index| sample_finding(&format!("Title {index}")))
            .collect(),
    );
    let run = sample_run("run_1", &output);

    let summary = run.summary(&active, &ReviewTarget::UncommittedChanges);

    assert_eq!(summary.rendered_findings, 20);
    assert_eq!(summary.omitted_findings, 5);
    assert!(summary.content.contains("... 5 more finding(s) omitted"));
}

#[test]
fn duplicate_lookup_prefers_adoptable_in_flight_match() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let completed = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("completed", &sample_output(Vec::new()))
    };
    let reviewing = AutoReviewRun {
        status: AutoReviewRunStatus::Reviewing,
        completed_at_unix_secs: None,
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("reviewing", &sample_output(Vec::new()))
    };
    store.save_run(&completed)?;
    store.save_run(&reviewing)?;

    let duplicate = store
        .find_duplicate_by_fingerprint_with_target_proof_and_filter(
            "sha256:abc",
            Some(&target_with_fingerprint("sha256:abc")),
            Some(&ReviewTarget::UncommittedChanges),
            |_| true,
        )?
        .expect("duplicate should be found");

    assert_eq!(duplicate.run_id, "reviewing");
    assert_eq!(duplicate.disposition, AutoReviewDuplicateDisposition::Adopt);
    Ok(())
}

#[test]
fn duplicate_lookup_uses_finding_count_for_completed_priority() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let clean = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("clean", &sample_output(Vec::new()))
    };
    let finding = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("finding", &sample_output(vec![sample_finding("Title")]))
    };
    store.save_run(&clean)?;
    store.save_run(&finding)?;

    let duplicate = store
        .find_duplicate_by_fingerprint_with_target_proof_and_filter(
            "sha256:abc",
            Some(&target_with_fingerprint("sha256:abc")),
            Some(&ReviewTarget::UncommittedChanges),
            |_| true,
        )?
        .expect("duplicate should be found");

    assert_eq!(duplicate.run_id, "finding");
    assert_eq!(duplicate.finding_count, 1);
    assert_eq!(
        duplicate.disposition,
        AutoReviewDuplicateDisposition::ReuseTerminal
    );
    Ok(())
}

#[test]
fn mark_superseded_preserves_runs_with_evidence() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("finding", &sample_output(vec![sample_finding("Title")]))
    };
    store.save_run(&run)?;

    let changed = store.mark_superseded("finding", "new_run")?;

    assert!(!changed);
    assert_eq!(
        store.load_run("finding")?.status,
        AutoReviewRunStatus::Completed
    );
    Ok(())
}

#[test]
fn mark_superseded_by_fingerprint_only_supersedes_clean_matching_scope() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let clean = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("clean", &sample_output(Vec::new()))
    };
    let finding = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("finding", &sample_output(vec![sample_finding("Title")]))
    };
    let other = AutoReviewRun {
        target: target_with_fingerprint("sha256:other"),
        ..sample_run("other", &sample_output(Vec::new()))
    };
    store.save_run(&clean)?;
    store.save_run(&finding)?;
    store.save_run(&other)?;

    let changed = store.mark_superseded_by_fingerprint_with_target(
        "sha256:abc",
        "new_run",
        Some("main"),
        Some("head-2"),
        /*active_review_target*/ None,
    )?;

    assert_eq!(changed, 1);
    assert_eq!(
        store.load_run("clean")?.status,
        AutoReviewRunStatus::Superseded
    );
    assert_eq!(
        store.load_run("finding")?.status,
        AutoReviewRunStatus::Completed
    );
    assert_eq!(
        store.load_run("other")?.status,
        AutoReviewRunStatus::Completed
    );
    Ok(())
}

#[test]
fn mark_superseded_by_fingerprint_requires_matching_review_target() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let different_turn_diff = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        review_target: ReviewTarget::CurrentTurnDiff {
            fingerprint: "sha256:first-turn".to_string(),
        },
        ..sample_run("different_turn_diff", &sample_output(Vec::new()))
    };
    let matching_turn_diff = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        review_target: ReviewTarget::CurrentTurnDiff {
            fingerprint: "sha256:second-turn".to_string(),
        },
        ..sample_run("matching_turn_diff", &sample_output(Vec::new()))
    };
    store.save_run(&different_turn_diff)?;
    store.save_run(&matching_turn_diff)?;

    let changed = store.mark_superseded_by_fingerprint_with_target(
        "sha256:abc",
        "new_run",
        Some("main"),
        Some("head-2"),
        Some(&ReviewTarget::CurrentTurnDiff {
            fingerprint: "sha256:second-turn".to_string(),
        }),
    )?;

    assert_eq!(changed, 1);
    assert_eq!(
        store.load_run("different_turn_diff")?.status,
        AutoReviewRunStatus::Completed
    );
    assert_eq!(
        store.load_run("matching_turn_diff")?.status,
        AutoReviewRunStatus::Superseded
    );
    Ok(())
}

#[test]
fn reconcile_orphaned_in_flight_marks_lost() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let running = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("running", &sample_output(Vec::new()))
    };
    let completed = sample_run("completed", &sample_output(Vec::new()));
    store.save_run(&running)?;
    store.save_run(&completed)?;

    let changed = store.reconcile_orphaned_in_flight(std::iter::empty::<&str>(), 99)?;

    let running = store.load_run("running")?;
    let completed = store.load_run("completed")?;
    assert_eq!(changed, 1);
    assert_eq!(running.status, AutoReviewRunStatus::Lost);
    assert_eq!(running.freshness, AutoReviewRunFreshness::Lost);
    assert_eq!(running.completed_at_unix_secs, Some(99));
    assert_eq!(completed.status, AutoReviewRunStatus::Completed);
    Ok(())
}

#[test]
fn reconcile_orphaned_in_flight_marks_manual_and_background_lost() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let manual = AutoReviewRun {
        status: AutoReviewRunStatus::Pending,
        completed_at_unix_secs: None,
        ..sample_run("manual", &sample_output(Vec::new()))
    };
    let background = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        source: AutoReviewRunSource::Background,
        completed_at_unix_secs: None,
        ..sample_run("background", &sample_output(Vec::new()))
    };
    let live_manual = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("live_manual", &sample_output(Vec::new()))
    };
    store.save_run(&manual)?;
    store.save_run(&background)?;
    store.save_run(&live_manual)?;

    let changed = store.reconcile_orphaned_in_flight(["live_manual"], 99)?;

    let manual = store.load_run("manual")?;
    let background = store.load_run("background")?;
    let live_manual = store.load_run("live_manual")?;
    assert_eq!(changed, 2);
    assert_eq!(manual.status, AutoReviewRunStatus::Lost);
    assert_eq!(manual.freshness, AutoReviewRunFreshness::Lost);
    assert_eq!(manual.completed_at_unix_secs, Some(99));
    assert_eq!(background.status, AutoReviewRunStatus::Lost);
    assert_eq!(background.freshness, AutoReviewRunFreshness::Lost);
    assert_eq!(background.completed_at_unix_secs, Some(99));
    assert_eq!(live_manual.status, AutoReviewRunStatus::Running);
    assert_eq!(live_manual.freshness, AutoReviewRunFreshness::Current);
    assert_eq!(live_manual.completed_at_unix_secs, None);
    Ok(())
}

#[test]
fn commit_review_targets_match_by_sha_even_when_titles_differ() {
    let active = sample_target("main", "abc123", "/repo");
    let run = AutoReviewRun {
        review_target: ReviewTarget::Commit {
            sha: "abc123".to_string(),
            title: Some("Original title".to_string()),
        },
        ..sample_run("run_1", &sample_output(vec![sample_finding("Title")]))
    };
    let active_review_target = ReviewTarget::Commit {
        sha: "abc123".to_string(),
        title: None,
    };

    assert_eq!(
        run.visible_finding_digests(&active, &active_review_target)
            .len(),
        1
    );
    assert_eq!(
        run.visible_finding_digests(
            &active,
            &ReviewTarget::Commit {
                sha: "def456".to_string(),
                title: Some("Original title".to_string())
            }
        )
        .len(),
        0
    );
}

#[test]
fn lifecycle_freshness_overrides_matching_target_freshness() {
    let active = sample_target("main", "head-2", "/repo");

    for (freshness, status) in [
        (AutoReviewRunFreshness::Lost, AutoReviewRunStatus::Lost),
        (
            AutoReviewRunFreshness::Superseded,
            AutoReviewRunStatus::Superseded,
        ),
    ] {
        let run = AutoReviewRun {
            freshness,
            status,
            ..sample_run("run_1", &sample_output(vec![sample_finding("Title")]))
        };

        assert_eq!(run.freshness(&active), AutoReviewFreshness::Stale);
        assert!(
            run.visible_finding_digests(&active, &ReviewTarget::UncommittedChanges)
                .is_empty()
        );
    }
}

fn run_ids(runs: Vec<AutoReviewRun>) -> Vec<String> {
    runs.into_iter().map(|run| run.run_id).collect()
}

fn sample_run(run_id: &str, output: &ReviewOutputEvent) -> AutoReviewRun {
    let finding_digests = finding_digests(output);
    AutoReviewRun {
        schema_version: SCHEMA_VERSION,
        run_id: run_id.to_string(),
        status: AutoReviewRunStatus::Completed,
        freshness: AutoReviewRunFreshness::Current,
        source: AutoReviewRunSource::Manual,
        target: sample_target("main", "head-2", "/repo"),
        review_target: ReviewTarget::UncommittedChanges,
        started_at_unix_secs: 1,
        completed_at_unix_secs: Some(2),
        model: Some("gpt-test".to_string()),
        superseded_by: None,
        cancel_reason: None,
        error_summary: None,
        finding_count: output.findings.len(),
        omitted_finding_digest_count: output.findings.len().saturating_sub(finding_digests.len()),
        finding_digests,
    }
}

fn corrupt_runs_index(store: &AutoReviewStore) -> anyhow::Result<()> {
    let runs_path = store.runs_path();
    std::fs::create_dir_all(runs_path.parent().expect("runs path parent"))?;
    std::fs::write(runs_path, "not json\n")?;
    Ok(())
}

fn target_with_fingerprint(fingerprint: &str) -> AutoReviewRunTarget {
    AutoReviewRunTarget {
        worktree_diff_fingerprint: Some(fingerprint.to_string()),
        ..sample_target("main", "head-2", "/repo")
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

fn sample_output(findings: Vec<ReviewFinding>) -> ReviewOutputEvent {
    ReviewOutputEvent {
        findings,
        overall_correctness: "patch is incorrect".to_string(),
        overall_explanation: "summary digest".to_string(),
        overall_confidence_score: 0.8,
    }
}

fn sample_finding(title: &str) -> ReviewFinding {
    ReviewFinding {
        title: title.to_string(),
        body: format!("Body {title}"),
        confidence_score: 0.9,
        priority: 1,
        code_location: ReviewCodeLocation {
            absolute_file_path: PathBuf::from("/tmp/example.rs"),
            line_range: ReviewLineRange { start: 7, end: 9 },
        },
    }
}
