use std::path::PathBuf;

use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewTarget;
use pretty_assertions::assert_eq;

use super::AutoReviewDetail;
use super::AutoReviewDuplicateDisposition;
use super::AutoReviewFindingRecord;
use super::AutoReviewFreshness;
use super::AutoReviewRun;
use super::AutoReviewRunFreshness;
use super::AutoReviewRunSource;
use super::AutoReviewRunStatus;
use super::AutoReviewRunTarget;
use super::AutoReviewStore;
use super::DETAIL_MAX_BYTES;
use super::SCHEMA_VERSION;

#[test]
fn save_and_load_run_round_trips_under_codex_home() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = sample_run("run_1", vec![sample_finding("f1", "Title")]);

    let path = store.save_run(&run)?;
    let loaded = store.load_run("run_1")?;

    assert_eq!(loaded, run);
    assert_eq!(path, store.runs_path());
    assert!(path.ends_with("auto-review/runs.json"));
    assert!(store.output_path("run_1")?.exists());
    Ok(())
}

#[test]
fn scoped_stores_separate_repos_under_one_home() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope_a = tempfile::tempdir()?;
    let scope_b = tempfile::tempdir()?;
    let store_a = AutoReviewStore::for_scope(codex_home.path(), scope_a.path());
    let store_b = AutoReviewStore::for_scope(codex_home.path(), scope_b.path());

    store_a.save_run(&sample_run("run_a", Vec::new()))?;
    store_b.save_run(&sample_run("run_b", Vec::new()))?;

    assert_eq!(
        store_a
            .list_runs()?
            .into_iter()
            .map(|run| run.run_id)
            .collect::<Vec<_>>(),
        vec!["run_a".to_string()]
    );
    assert_eq!(
        store_b
            .list_runs()?
            .into_iter()
            .map(|run| run.run_id)
            .collect::<Vec<_>>(),
        vec!["run_b".to_string()]
    );
    assert_ne!(store_a.runs_path(), store_b.runs_path());
    Ok(())
}

#[test]
fn list_runs_returns_valid_json_runs_in_id_order() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run_b = sample_run("run_b", Vec::new());
    let run_a = sample_run("run_a", Vec::new());
    store.save_run(&run_b)?;
    store.save_run(&run_a)?;

    let runs = store.list_runs()?;

    assert_eq!(
        runs.into_iter().map(|run| run.run_id).collect::<Vec<_>>(),
        vec!["run_a".to_string(), "run_b".to_string()]
    );
    Ok(())
}

#[test]
fn list_runs_recovers_runs_missing_from_index_but_present_in_outputs() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run_a = sample_run("run_a", Vec::new());
    let run_b = sample_run("run_b", Vec::new());
    store.save_run(&run_a)?;
    store.save_run(&run_b)?;

    let index = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "runs": [run_b],
    });
    std::fs::write(store.runs_path(), format!("{index}\n"))?;

    let runs = store.list_runs()?;

    assert_eq!(
        runs.into_iter().map(|run| run.run_id).collect::<Vec<_>>(),
        vec!["run_a".to_string(), "run_b".to_string()]
    );
    Ok(())
}

#[test]
fn sidecar_does_not_override_index_run() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let indexed = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        findings: Vec::new(),
        ..sample_run("run_1", Vec::new())
    };
    let sidecar = AutoReviewRun {
        status: AutoReviewRunStatus::Completed,
        findings: vec![sample_finding("f1", "Sidecar")],
        ..sample_run("run_1", Vec::new())
    };
    let index = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "runs": [indexed],
    });
    let runs_path = store.runs_path();
    std::fs::create_dir_all(runs_path.parent().expect("runs path parent"))?;
    std::fs::write(&runs_path, format!("{index}\n"))?;
    let output_path = store.output_path("run_1")?;
    std::fs::create_dir_all(output_path.parent().expect("output path parent"))?;
    std::fs::write(&output_path, serde_json::to_string_pretty(&sidecar)?)?;

    let loaded = store.load_run("run_1")?;

    assert_eq!(loaded.status, AutoReviewRunStatus::Running);
    assert!(loaded.findings.is_empty());
    Ok(())
}

#[test]
fn corrupt_sidecar_does_not_block_store_listing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", Vec::new()))?;
    let bad_path = store.output_path("bad_run")?;
    std::fs::create_dir_all(bad_path.parent().expect("output path parent"))?;
    std::fs::write(&bad_path, "not json\n")?;

    let runs = store.list_runs()?;

    assert_eq!(
        runs.into_iter().map(|run| run.run_id).collect::<Vec<_>>(),
        vec!["run_1".to_string()]
    );
    Ok(())
}

#[test]
fn list_runs_returns_empty_when_store_is_missing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let runs = AutoReviewStore::for_scope(codex_home.path(), scope.path()).list_runs()?;

    assert!(runs.is_empty());
    Ok(())
}

#[test]
fn scoped_store_reads_legacy_unscoped_runs() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let legacy_run = sample_run("legacy_run", vec![sample_finding("f1", "Legacy")]);
    let legacy_dir = codex_home.path().join("auto-review").join("runs");
    std::fs::create_dir_all(&legacy_dir)?;
    std::fs::write(
        legacy_dir.join("legacy_run.json"),
        format!("{}\n", serde_json::to_string_pretty(&legacy_run)?),
    )?;

    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let runs = store.list_runs()?;
    let loaded = store.load_run("legacy_run")?;
    let detail = store.finding_detail("legacy_run", "f1", 1024)?;

    assert_eq!(runs, vec![legacy_run.clone()]);
    assert_eq!(loaded, legacy_run);
    assert!(detail.content.contains("Legacy"));
    Ok(())
}

#[test]
fn scoped_store_prefers_new_runs_over_legacy_runs() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let legacy_run = sample_run("same_run", vec![sample_finding("f1", "Legacy")]);
    let new_run = sample_run("same_run", vec![sample_finding("f1", "New")]);
    let legacy_dir = codex_home.path().join("auto-review").join("runs");
    std::fs::create_dir_all(&legacy_dir)?;
    std::fs::write(
        legacy_dir.join("same_run.json"),
        format!("{}\n", serde_json::to_string_pretty(&legacy_run)?),
    )?;

    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&new_run)?;

    assert_eq!(store.load_run("same_run")?, new_run);
    Ok(())
}

#[test]
fn detects_new_or_legacy_store_files() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;

    assert!(!AutoReviewStore::has_store_files(codex_home.path()));

    std::fs::create_dir_all(
        codex_home
            .path()
            .join("state")
            .join("review")
            .join("repo-empty")
            .join("auto-review")
            .join("outputs"),
    )?;
    assert!(!AutoReviewStore::has_store_files(codex_home.path()));

    std::fs::create_dir_all(codex_home.path().join("auto-review").join("runs"))?;
    assert!(!AutoReviewStore::has_store_files(codex_home.path()));
    std::fs::write(
        codex_home
            .path()
            .join("auto-review")
            .join("runs")
            .join("bad_run.json"),
        "not json\n",
    )?;
    std::fs::write(
        codex_home
            .path()
            .join("auto-review")
            .join("runs")
            .join("bad run.json"),
        "not json\n",
    )?;
    assert!(!AutoReviewStore::has_store_files(codex_home.path()));
    std::fs::write(
        codex_home
            .path()
            .join("auto-review")
            .join("runs")
            .join("legacy_run.json"),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&sample_run("legacy_run", Vec::new()))?
        ),
    )?;
    assert!(AutoReviewStore::has_store_files(codex_home.path()));

    let codex_home = tempfile::tempdir()?;
    AutoReviewStore::for_scope(codex_home.path(), scope.path())
        .save_run(&sample_run("run_1", Vec::new()))?;

    assert!(AutoReviewStore::has_store_files(codex_home.path()));
    Ok(())
}

#[test]
fn detail_lookup_uses_new_index_before_legacy_run() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let legacy_run = sample_run("same_run", vec![sample_finding("f1", "Legacy")]);
    let new_run = sample_run("same_run", vec![sample_finding("f1", "New")]);
    let legacy_dir = codex_home.path().join("auto-review").join("runs");
    std::fs::create_dir_all(&legacy_dir)?;
    std::fs::write(
        legacy_dir.join("same_run.json"),
        format!("{}\n", serde_json::to_string_pretty(&legacy_run)?),
    )?;

    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let index = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "runs": [new_run],
    });
    let runs_path = store.runs_path();
    std::fs::create_dir_all(runs_path.parent().expect("runs path parent"))?;
    std::fs::write(&runs_path, format!("{index}\n"))?;

    let detail = store.finding_detail("same_run", "f1", 1024)?;

    assert!(detail.content.contains("New"));
    assert!(!detail.content.contains("Legacy"));
    Ok(())
}

#[test]
fn corrupt_legacy_run_does_not_hide_new_store() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let new_run = sample_run("new_run", vec![sample_finding("f1", "New")]);
    let legacy_dir = codex_home.path().join("auto-review").join("runs");
    std::fs::create_dir_all(&legacy_dir)?;
    std::fs::write(legacy_dir.join("bad_run.json"), "not json\n")?;
    std::fs::write(legacy_dir.join("bad run.json"), "not json\n")?;

    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&new_run)?;

    assert_eq!(
        store
            .list_runs()?
            .into_iter()
            .map(|run| run.run_id)
            .collect::<Vec<_>>(),
        vec!["new_run".to_string()]
    );
    assert_eq!(store.load_run("new_run")?.run_id, "new_run");
    assert!(
        store
            .finding_detail("new_run", "f1", 1024)?
            .content
            .contains("New")
    );
    Ok(())
}

#[test]
fn load_run_defaults_missing_worktree_diff_fingerprint() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let path = store.runs_path();
    let parent = path.parent().expect("runs path should have parent");
    std::fs::create_dir_all(parent)?;
    std::fs::write(
        &path,
        r#"{
  "schema_version": 1,
  "runs": [
    {
      "schema_version": 1,
      "run_id": "run_1",
      "status": "completed",
      "source": "manual",
      "target": {
        "branch": "main",
        "head_sha": "head-2",
        "base_sha": "base-1",
        "worktree_path": "/repo"
      },
      "review_target": {
        "type": "uncommittedChanges"
      },
      "started_at_unix_secs": 1,
      "completed_at_unix_secs": 2,
      "model": "gpt-test",
      "error_summary": null,
      "findings": []
    }
  ]
}"#,
    )?;

    let loaded = store.load_run("run_1")?;

    assert_eq!(loaded.target.worktree_diff_fingerprint, None);
    Ok(())
}

#[test]
fn load_run_defaults_missing_lifecycle_fields() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let path = store.runs_path();
    std::fs::create_dir_all(path.parent().expect("runs path parent"))?;
    std::fs::write(
        &path,
        r#"{
  "schema_version": 1,
  "runs": [
    {
      "schema_version": 1,
      "run_id": "run_1",
      "status": "completed",
      "source": "manual",
      "target": {
        "branch": "main",
        "head_sha": "head-2",
        "base_sha": "base-1",
        "worktree_path": "/repo"
      },
      "review_target": {
        "type": "uncommittedChanges"
      },
      "started_at_unix_secs": 1,
      "completed_at_unix_secs": 2,
      "model": "gpt-test",
      "error_summary": null,
      "findings": []
    }
  ]
}"#,
    )?;

    let loaded = store.load_run("run_1")?;

    assert_eq!(loaded.freshness, AutoReviewRunFreshness::Unknown);
    assert_eq!(loaded.superseded_by, None);
    assert_eq!(loaded.cancel_reason, None);
    Ok(())
}

#[test]
fn unsafe_run_ids_are_rejected() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let scope = tempfile::tempdir().expect("tempdir");
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = sample_run("../run", Vec::new());

    let error = store.save_run(&run).expect_err("unsafe id should fail");

    assert!(error.to_string().contains("auto review run_id"));
}

#[test]
fn unsafe_and_duplicate_finding_ids_are_rejected() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let scope = tempfile::tempdir().expect("tempdir");
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
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
    let scope = tempfile::tempdir().expect("tempdir");
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
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
        ..sample_run("run_1", vec![sample_finding("f1", "Title")])
    };

    assert_eq!(run.freshness(&active), AutoReviewFreshness::Stale);
    assert!(
        run.visible_findings(&active, &ReviewTarget::UncommittedChanges)
            .is_empty()
    );
}

#[test]
fn visible_findings_require_completed_run_status() {
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
            ..sample_run("run_1", vec![sample_finding("f1", "Title")])
        };

        assert!(
            run.visible_findings(&active, &active_review_target)
                .is_empty()
        );
        assert_eq!(run.summary(&active, &active_review_target).content, "");
    }
}

#[test]
fn duplicate_lookup_prefers_adoptable_in_flight_match() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let completed = AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("completed", Vec::new())
    };
    let reviewing = AutoReviewRun {
        status: AutoReviewRunStatus::Reviewing,
        completed_at_unix_secs: None,
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("reviewing", Vec::new())
    };
    store.save_run(&completed)?;
    store.save_run(&reviewing)?;

    let duplicate = store
        .find_duplicate_by_fingerprint("sha256:abc")?
        .expect("duplicate match");

    assert_eq!(duplicate.run_id, "reviewing");
    assert_eq!(duplicate.disposition, AutoReviewDuplicateDisposition::Adopt);
    Ok(())
}

#[test]
fn duplicate_lookup_can_filter_stale_target_matches() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&AutoReviewRun {
        target: AutoReviewRunTarget {
            branch: Some("main".to_string()),
            head_sha: Some("old-head".to_string()),
            worktree_diff_fingerprint: Some("sha256:abc".to_string()),
            ..sample_target("main", "old-head", "/repo")
        },
        ..sample_run("old", Vec::new())
    })?;

    assert!(
        store
            .find_duplicate_by_fingerprint_with_target(
                "sha256:abc",
                Some("main"),
                Some("new-head")
            )?
            .is_none()
    );
    assert!(
        store
            .find_duplicate_by_fingerprint_with_target(
                "sha256:abc",
                Some("main"),
                Some("old-head")
            )?
            .is_some()
    );
    Ok(())
}

#[test]
fn duplicate_lookup_reports_pending_but_ignores_lost_skipped_and_superseded() -> anyhow::Result<()>
{
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&AutoReviewRun {
        status: AutoReviewRunStatus::Pending,
        target: target_with_fingerprint("sha256:pending"),
        ..sample_run("pending", Vec::new())
    })?;
    for (run_id, status) in [
        ("lost", AutoReviewRunStatus::Lost),
        ("skipped", AutoReviewRunStatus::Skipped),
        ("superseded", AutoReviewRunStatus::Superseded),
    ] {
        store.save_run(&AutoReviewRun {
            status,
            target: target_with_fingerprint("sha256:abc"),
            ..sample_run(run_id, Vec::new())
        })?;
    }

    assert_eq!(store.find_duplicate_by_fingerprint("sha256:abc")?, None);
    let duplicate = store
        .find_duplicate_by_fingerprint("sha256:pending")?
        .expect("pending duplicate match");
    assert_eq!(duplicate.run_id, "pending");
    assert_eq!(
        duplicate.disposition,
        AutoReviewDuplicateDisposition::SupersedeTerminal
    );
    Ok(())
}

#[test]
fn mark_superseded_preserves_runs_with_evidence() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("clean", Vec::new()))?;
    store.save_run(&sample_run(
        "with_finding",
        vec![sample_finding("f1", "Keep")],
    ))?;

    assert!(store.mark_superseded("clean", "new_run")?);
    assert!(!store.mark_superseded("with_finding", "new_run")?);

    let clean = store.load_run("clean")?;
    let with_finding = store.load_run("with_finding")?;
    assert_eq!(clean.status, AutoReviewRunStatus::Superseded);
    assert_eq!(clean.freshness, AutoReviewRunFreshness::Superseded);
    assert_eq!(clean.superseded_by.as_deref(), Some("new_run"));
    assert_eq!(with_finding.status, AutoReviewRunStatus::Completed);
    Ok(())
}

#[test]
fn mark_superseded_by_fingerprint_only_supersedes_matching_scope() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&AutoReviewRun {
        target: target_with_fingerprint("sha256:abc"),
        ..sample_run("same_scope", Vec::new())
    })?;
    store.save_run(&AutoReviewRun {
        target: target_with_fingerprint("sha256:other"),
        ..sample_run("other_scope", Vec::new())
    })?;

    let changed = store.mark_superseded_by_fingerprint("sha256:abc", "new_run")?;

    assert_eq!(changed, 1);
    assert_eq!(
        store.load_run("same_scope")?.status,
        AutoReviewRunStatus::Superseded
    );
    assert_eq!(
        store.load_run("other_scope")?.status,
        AutoReviewRunStatus::Completed
    );
    Ok(())
}

#[test]
fn reconcile_orphaned_in_flight_marks_lost() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&AutoReviewRun {
        status: AutoReviewRunStatus::Reviewing,
        completed_at_unix_secs: None,
        ..sample_run("reviewing", Vec::new())
    })?;
    store.save_run(&sample_run("completed", Vec::new()))?;

    let changed = store.reconcile_orphaned_in_flight(std::iter::empty::<&str>(), 42)?;

    assert_eq!(changed, 1);
    let lost = store.load_run("reviewing")?;
    assert_eq!(lost.status, AutoReviewRunStatus::Lost);
    assert_eq!(lost.freshness, AutoReviewRunFreshness::Lost);
    assert_eq!(lost.completed_at_unix_secs, Some(42));
    assert_eq!(
        lost.cancel_reason.as_deref(),
        Some("agent_missing_after_restart")
    );
    assert_eq!(
        store.load_run("completed")?.status,
        AutoReviewRunStatus::Completed
    );
    Ok(())
}

#[test]
fn reconcile_orphaned_in_flight_preserves_live_runs() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&AutoReviewRun {
        status: AutoReviewRunStatus::Reviewing,
        completed_at_unix_secs: None,
        ..sample_run("live", Vec::new())
    })?;
    store.save_run(&AutoReviewRun {
        status: AutoReviewRunStatus::Reviewing,
        completed_at_unix_secs: None,
        ..sample_run("orphan", Vec::new())
    })?;

    let changed = store.reconcile_orphaned_in_flight(["live"], 42)?;

    assert_eq!(changed, 1);
    assert_eq!(
        store.load_run("live")?.status,
        AutoReviewRunStatus::Reviewing
    );
    assert_eq!(store.load_run("orphan")?.status, AutoReviewRunStatus::Lost);
    Ok(())
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
            ..sample_run("run_1", vec![sample_finding("f1", "Title")])
        };

        assert_eq!(run.freshness(&active), AutoReviewFreshness::Stale);
        assert!(
            run.visible_findings(&active, &ReviewTarget::UncommittedChanges)
                .is_empty()
        );
    }
}

#[test]
fn finding_detail_requires_completed_run_status() {
    let run = AutoReviewRun {
        status: AutoReviewRunStatus::Cancelled,
        ..sample_run("run_1", vec![sample_finding("f1", "Title")])
    };

    let err = run
        .finding_detail("f1", DETAIL_MAX_BYTES)
        .expect_err("non-completed runs should not expose finding detail");
    assert!(
        err.to_string()
            .contains("auto review run is not completed: run_1")
    );
}

#[test]
fn commit_review_targets_match_by_sha_even_when_titles_differ() {
    let active = sample_target("main", "abc123", "/repo");
    let run = AutoReviewRun {
        review_target: ReviewTarget::Commit {
            sha: "abc123".to_string(),
            title: Some("Original title".to_string()),
        },
        ..sample_run("run_1", vec![sample_finding("f1", "Title")])
    };
    let active_review_target = ReviewTarget::Commit {
        sha: "abc123".to_string(),
        title: None,
    };

    assert_eq!(
        run.visible_findings(&active, &active_review_target).len(),
        1
    );

    let title_variant = ReviewTarget::Commit {
        sha: "abc123".to_string(),
        title: Some("Different title".to_string()),
    };
    assert_eq!(run.visible_findings(&active, &title_variant).len(), 1);

    let different_commit = ReviewTarget::Commit {
        sha: "def456".to_string(),
        title: Some("Original title".to_string()),
    };
    assert!(run.visible_findings(&active, &different_commit).is_empty());
}

#[test]
fn commit_review_targets_ignore_checkout_metadata_when_reopened() {
    let run = AutoReviewRun {
        target: AutoReviewRunTarget {
            branch: Some("feature-old".to_string()),
            head_sha: Some("abc123".to_string()),
            base_sha: Some("base-old".to_string()),
            worktree_path: Some(PathBuf::from("/repo-old")),
            worktree_diff_fingerprint: Some("sha256:old".to_string()),
        },
        review_target: ReviewTarget::Commit {
            sha: "abc123".to_string(),
            title: Some("Original title".to_string()),
        },
        ..sample_run("run_1", vec![sample_finding("f1", "Title")])
    };
    let reopened_detached = AutoReviewRunTarget {
        branch: None,
        head_sha: Some("abc123".to_string()),
        base_sha: None,
        worktree_path: None,
        worktree_diff_fingerprint: Some("sha256:new".to_string()),
    };
    let reopened_renamed_branch = AutoReviewRunTarget {
        branch: Some("feature-renamed".to_string()),
        head_sha: Some("abc123".to_string()),
        base_sha: Some("base-new".to_string()),
        worktree_path: Some(PathBuf::from("/repo-new")),
        worktree_diff_fingerprint: Some("sha256:new".to_string()),
    };

    assert_eq!(
        run.visible_findings(&reopened_detached, &run.review_target)
            .len(),
        1
    );
    assert_eq!(
        run.visible_findings(&reopened_renamed_branch, &run.review_target)
            .len(),
        1
    );
    assert_eq!(
        run.summary(&reopened_detached, &run.review_target).content,
        "[P1] f1: Title (/tmp/example.rs:7-9)"
    );
    assert_eq!(
        run.summary(&reopened_renamed_branch, &run.review_target)
            .content,
        "[P1] f1: Title (/tmp/example.rs:7-9)"
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
fn store_detail_lookup_reads_completed_output_sidecar() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = sample_run("run_1", vec![sample_finding("f1", &"x".repeat(200))]);
    store.save_run(&run)?;

    let detail = store.finding_detail("run_1", "f1", 120)?;

    assert_eq!(detail.finding_id, "f1");
    assert!(detail.truncated);
    assert!(detail.bytes <= 120);
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
        findings,
    }
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
        worktree_diff_fingerprint: None,
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
