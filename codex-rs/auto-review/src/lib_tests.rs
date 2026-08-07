use std::path::PathBuf;

use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
use pretty_assertions::assert_eq;

use super::AutoReviewBudget;
use super::AutoReviewCurrentTurnTarget;
use super::AutoReviewDetailKind;
use super::AutoReviewDiagnostics;
use super::AutoReviewDispositionActor;
use super::AutoReviewDuplicateDisposition;
use super::AutoReviewFindingDisposition;
use super::AutoReviewFindingDispositionRecord;
use super::AutoReviewFreshness;
use super::AutoReviewLedgerProjection;
use super::AutoReviewRun;
use super::AutoReviewRunFreshness;
use super::AutoReviewRunSource;
use super::AutoReviewRunState;
use super::AutoReviewRunStatus;
use super::AutoReviewRunTarget;
use super::AutoReviewRunsIndex;
use super::AutoReviewStore;
use super::AutoReviewTerminalReason;
use super::AutoReviewUsage;
use super::DEFAULT_MAX_RUNS;
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
    let metadata_text = std::fs::read_to_string(store.run_metadata_path("run_1")?)?;

    assert_eq!(loaded, run);
    assert_eq!(path, store.runs_path());
    assert!(path.ends_with("auto-review/runs.json"));
    assert!(index_text.contains("finding_count"));
    assert!(index_text.contains("finding_digests"));
    assert!(!index_text.contains("Body Title"));
    assert!(sidecar_text.contains("Body Title"));
    assert!(metadata_text.contains("finding_count"));
    assert!(!metadata_text.contains("Body Title"));
    Ok(())
}

#[test]
fn save_and_load_run_state_preserves_run_schema_compatibility() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Title")]);
    store.save_run(&sample_run("run_1", &output))?;
    let mut state = AutoReviewRunState::new("run_1");
    state.budget = Some(AutoReviewBudget {
        max_scope_bytes: 120_000,
        max_elapsed_ms: 300_000,
        max_total_tokens: 250_000,
        max_output_bytes: 65_536,
        max_findings: 20,
    });
    state.usage = AutoReviewUsage {
        scope_bytes: Some(12_000),
        elapsed_ms: Some(4_000),
        total_tokens: Some(25_000),
        input_tokens: Some(23_000),
        cached_input_tokens: Some(5_000),
        output_tokens: Some(2_000),
        effective_total_token_limit: Some(240_000),
        accounting_tolerance_tokens: Some(10_000),
        projected_total_tokens: Some(28_000),
        request_count: Some(2),
        retry_count: Some(1),
        tool_registry_tokens: Some(1_200),
        tool_registry_pruned_count: Some(3),
        tool_output_tokens: Some(300),
        tool_output_limit_tokens: Some(4_096),
        response_output_limit_tokens: Some(8_192),
        response_output_reservation_tokens: Some(128_000),
        orchestration_skills_suppressed: Some(true),
        output_bytes: Some(2_000),
        finding_count: Some(1),
    };
    state.terminal_reason = Some(AutoReviewTerminalReason::BudgetTotalTokens);

    store.save_run_state(&state)?;

    assert_eq!(store.load_run_state("run_1")?, Some(state));
    let index_text = std::fs::read_to_string(store.runs_path())?;
    assert!(!index_text.contains("max_total_tokens"));
    assert!(!index_text.contains("terminal_reason"));
    assert_eq!(store.load_run("run_1")?.schema_version, SCHEMA_VERSION);
    Ok(())
}

#[test]
fn set_finding_disposition_is_durable_and_audited() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Title")]);
    store.save_run(&sample_run("run_1", &output))?;
    let disposition = AutoReviewFindingDispositionRecord {
        disposition: AutoReviewFindingDisposition::Deferred,
        actor: AutoReviewDispositionActor::User,
        reason: Some("acknowledged for follow-up".to_string()),
        updated_at_unix_secs: 42,
    };

    let state = store.set_finding_disposition("run_1", disposition.clone())?;

    assert_eq!(state.finding_disposition, Some(disposition));
    assert_eq!(store.load_run_state("run_1")?, Some(state));
    Ok(())
}

#[test]
fn obsolete_finding_disposition_requires_reason() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Title")]);
    store.save_run(&sample_run("run_1", &output))?;

    let err = store
        .set_finding_disposition(
            "run_1",
            AutoReviewFindingDispositionRecord {
                disposition: AutoReviewFindingDisposition::Obsolete,
                actor: AutoReviewDispositionActor::Agent,
                reason: None,
                updated_at_unix_secs: 42,
            },
        )
        .expect_err("obsolete disposition without a reason should fail");

    assert!(err.to_string().contains("requires a reason"));
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
fn merge_latest_from_disk_preserves_runs_from_other_writers() {
    let output = sample_output(Vec::new());
    let mut stale_writer = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![sample_run("run_stale_writer", &output)],
    };
    let latest_disk = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![sample_run("run_disk", &output)],
    };

    stale_writer.merge_latest_from_disk(latest_disk, "run_stale_writer");

    assert_eq!(
        run_ids(stale_writer.runs),
        vec!["run_disk".to_string(), "run_stale_writer".to_string(),]
    );
}

#[test]
fn merge_latest_from_disk_keeps_terminal_disk_status_over_stale_same_run_update() {
    let output = sample_output(Vec::new());
    let completed = sample_run("run_1", &output);
    let stale_running = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("run_1", &output)
    };
    let mut stale_writer = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![stale_running],
    };
    let latest_disk = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![completed],
    };

    stale_writer.merge_latest_from_disk(latest_disk, "run_1");

    assert_eq!(stale_writer.runs[0].status, AutoReviewRunStatus::Completed);
}

#[test]
fn merge_latest_from_disk_keeps_terminal_memory_status_over_stale_disk_update() {
    let output = sample_output(Vec::new());
    let completed = sample_run("run_1", &output);
    let stale_running = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("run_1", &output)
    };
    let mut writer = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![completed],
    };
    let latest_disk = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![stale_running],
    };

    writer.merge_latest_from_disk(latest_disk, "other_run");

    assert_eq!(writer.runs[0].status, AutoReviewRunStatus::Completed);
}

#[test]
fn merge_latest_from_disk_prefers_higher_ranked_terminal_status_for_same_run() {
    let output = sample_output(Vec::new());
    let stale_skipped = AutoReviewRun {
        status: AutoReviewRunStatus::Skipped,
        completed_at_unix_secs: Some(2),
        ..sample_run("run_1", &output)
    };
    let completed = AutoReviewRun {
        status: AutoReviewRunStatus::Completed,
        completed_at_unix_secs: Some(2),
        token_count: Some(25_915),
        ..sample_run("run_1", &output)
    };
    let mut writer = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![stale_skipped],
    };
    let latest_disk = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![completed],
    };

    writer.merge_latest_from_disk(latest_disk, "other_run");

    assert_eq!(writer.runs[0].status, AutoReviewRunStatus::Completed);
    assert_eq!(writer.runs[0].token_count, Some(25_915));
}

#[test]
fn merge_latest_from_disk_does_not_replace_preferred_terminal_with_same_timestamp_status() {
    let output = sample_output(Vec::new());
    let completed = AutoReviewRun {
        status: AutoReviewRunStatus::Completed,
        completed_at_unix_secs: Some(2),
        token_count: Some(25_915),
        ..sample_run("run_1", &output)
    };
    let stale_skipped = AutoReviewRun {
        status: AutoReviewRunStatus::Skipped,
        completed_at_unix_secs: Some(2),
        ..sample_run("run_1", &output)
    };
    let mut writer = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![completed],
    };
    let latest_disk = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![stale_skipped],
    };

    writer.merge_latest_from_disk(latest_disk, "run_1");

    assert_eq!(writer.runs[0].status, AutoReviewRunStatus::Completed);
    assert_eq!(writer.runs[0].token_count, Some(25_915));
}

#[test]
fn merge_latest_from_disk_preserves_unrelated_lifecycle_update() {
    let output = sample_output(Vec::new());
    let completed = AutoReviewRun {
        status: AutoReviewRunStatus::Completed,
        completed_at_unix_secs: Some(2),
        token_count: Some(25_915),
        ..sample_run("run_1", &output)
    };
    let superseded = AutoReviewRun {
        status: AutoReviewRunStatus::Superseded,
        freshness: AutoReviewRunFreshness::Superseded,
        completed_at_unix_secs: Some(2),
        token_count: Some(25_915),
        superseded_by: Some("run_2".to_string()),
        ..sample_run("run_1", &output)
    };
    let mut stale_writer = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![completed, sample_run("run_2", &output)],
    };
    let latest_disk = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![superseded],
    };

    stale_writer.merge_latest_from_disk(latest_disk, "run_2");

    let run_1 = stale_writer
        .runs
        .iter()
        .find(|run| run.run_id == "run_1")
        .expect("run_1 preserved");
    assert_eq!(run_1.status, AutoReviewRunStatus::Superseded);
    assert_eq!(run_1.superseded_by.as_deref(), Some("run_2"));
    assert_eq!(run_1.token_count, Some(25_915));
}

#[test]
fn merge_latest_from_disk_does_not_rewind_unrelated_disk_progress() {
    let output = sample_output(Vec::new());
    let run_1_running = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("run_1", &output)
    };
    let run_1_reviewing = AutoReviewRun {
        status: AutoReviewRunStatus::Reviewing,
        completed_at_unix_secs: None,
        ..sample_run("run_1", &output)
    };
    let mut stale_writer = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![run_1_running, sample_run("run_2", &output)],
    };
    let latest_disk = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: vec![run_1_reviewing],
    };

    stale_writer.merge_latest_from_disk(latest_disk, "run_2");

    assert_eq!(
        stale_writer
            .runs
            .iter()
            .find(|run| run.run_id == "run_1")
            .expect("run_1")
            .status,
        AutoReviewRunStatus::Reviewing
    );
    assert_eq!(
        run_ids(stale_writer.runs),
        vec!["run_1".to_string(), "run_2".to_string()]
    );
}

#[test]
fn compact_preserves_preferred_run_when_concurrent_writes_exceed_limit() {
    let output = sample_output(Vec::new());
    let mut index = AutoReviewRunsIndex {
        schema_version: SCHEMA_VERSION,
        runs: (0..(DEFAULT_MAX_RUNS + 1))
            .map(|index| AutoReviewRun {
                run_id: format!("run_{index:03}"),
                started_at_unix_secs: index as i64,
                completed_at_unix_secs: Some(index as i64),
                ..sample_run("unused", &output)
            })
            .collect(),
    };

    index.compact_to_preserving(DEFAULT_MAX_RUNS, "run_000");

    let run_ids = run_ids(index.runs);
    assert_eq!(run_ids.len(), DEFAULT_MAX_RUNS);
    assert!(run_ids.contains(&"run_000".to_string()));
    assert!(!run_ids.contains(&"run_001".to_string()));
    assert!(run_ids.contains(&format!("run_{DEFAULT_MAX_RUNS:03}")));
}

#[test]
fn save_run_compacts_index_to_most_recent_runs() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(Vec::new());
    for index in 0..(DEFAULT_MAX_RUNS + 5) {
        store.save_run(&AutoReviewRun {
            run_id: format!("run_{index:03}"),
            started_at_unix_secs: index as i64,
            completed_at_unix_secs: Some(index as i64),
            ..sample_run("unused", &output)
        })?;
    }

    let run_ids = run_ids(store.list_runs()?);
    assert_eq!(run_ids.len(), DEFAULT_MAX_RUNS);
    assert!(!run_ids.contains(&"run_000".to_string()));
    assert!(!run_ids.contains(&"run_004".to_string()));
    assert!(run_ids.contains(&"run_005".to_string()));
    assert!(run_ids.contains(&format!("run_{:03}", DEFAULT_MAX_RUNS + 4)));
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
fn corrupt_output_sidecar_does_not_block_store_listing() -> anyhow::Result<()> {
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
fn corrupt_metadata_sidecar_does_not_block_store_listing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    let bad_path = store.run_metadata_path("bad_run")?;
    std::fs::create_dir_all(bad_path.parent().expect("metadata path parent"))?;
    std::fs::write(&bad_path, "not json\n")?;
    corrupt_runs_index(&store)?;

    assert_eq!(run_ids(store.list_runs()?), vec!["run_1".to_string()]);
    Ok(())
}

#[test]
fn corrupt_index_recovers_reads_from_run_metadata() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = sample_run("run_1", &sample_output(vec![sample_finding("Recovered")]));
    store.save_run(&run)?;
    corrupt_runs_index(&store)?;

    assert_eq!(store.load_run("run_1")?, run);
    assert_eq!(run_ids(store.list_runs()?), vec!["run_1".to_string()]);
    Ok(())
}

#[test]
fn corrupt_index_recovers_detail_from_metadata_and_output() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Recovered detail")]);
    let run = sample_run("run_1", &output);
    store.save_run(&run)?;
    store.save_output("run_1", &output)?;
    corrupt_runs_index(&store)?;

    let detail = store.finding_detail("run_1", "f1", DETAIL_MAX_BYTES)?;
    let run_detail = store.detail("run_1", /*finding_id*/ None, DETAIL_MAX_BYTES)?;

    assert_eq!(detail.kind, AutoReviewDetailKind::Finding);
    assert_eq!(detail.finding_id.as_deref(), Some("f1"));
    assert!(detail.content.contains("title: Recovered detail"));
    assert_eq!(run_detail.kind, AutoReviewDetailKind::Run);
    assert!(run_detail.content.contains("title: Recovered detail"));
    Ok(())
}

#[test]
fn corrupt_index_still_blocks_writes() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    corrupt_runs_index(&store)?;

    let error = store
        .save_run(&sample_run("run_2", &sample_output(Vec::new())))
        .expect_err("corrupt canonical index should block writes");

    assert!(
        error
            .to_string()
            .contains("failed to parse auto review runs index"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn corrupt_index_still_blocks_supersede_writes() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    corrupt_runs_index(&store)?;

    let error = store
        .mark_superseded("run_1", "run_2")
        .expect_err("corrupt canonical index should block supersede writes");

    assert!(
        error
            .to_string()
            .contains("failed to parse auto review runs index"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn corrupt_index_still_blocks_supersede_writes_when_metadata_is_missing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    std::fs::remove_file(store.run_metadata_path("run_1")?)?;
    corrupt_runs_index(&store)?;

    let error = store
        .mark_superseded("run_1", "run_2")
        .expect_err("corrupt canonical index should block supersede writes");

    assert!(
        error
            .to_string()
            .contains("failed to parse auto review runs index"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn supersede_writes_recover_when_index_is_missing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    std::fs::remove_file(store.runs_path())?;

    assert!(store.mark_superseded("run_1", "run_2")?);

    let run = store.load_run("run_1")?;
    assert_eq!(run.status, AutoReviewRunStatus::Superseded);
    assert_eq!(run.superseded_by.as_deref(), Some("run_2"));
    Ok(())
}

#[test]
fn corrupt_index_still_blocks_orphan_reconciliation_writes() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("run_1", &sample_output(Vec::new()))
    })?;
    corrupt_runs_index(&store)?;

    let error = store
        .reconcile_orphaned_in_flight(std::iter::empty::<&str>(), /*now_unix_secs*/ 3)
        .expect_err("corrupt canonical index should block reconciliation writes");

    assert!(
        error
            .to_string()
            .contains("failed to parse auto review runs index"),
        "unexpected error: {error:#}"
    );
    Ok(())
}

#[test]
fn save_run_preserves_metadata_runs_when_index_is_missing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    std::fs::remove_file(store.runs_path())?;

    store.save_run(&sample_run("run_2", &sample_output(Vec::new())))?;

    assert_eq!(
        run_ids(store.list_runs()?),
        vec!["run_1".to_string(), "run_2".to_string()]
    );
    Ok(())
}

#[test]
fn orphan_reconciliation_recovers_when_index_is_missing() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("run_1", &sample_output(Vec::new()))
    })?;
    std::fs::remove_file(store.runs_path())?;

    let reconciled =
        store.reconcile_orphaned_in_flight(std::iter::empty::<&str>(), /*now_unix_secs*/ 3)?;
    assert_eq!(run_ids(reconciled), vec!["run_1".to_string()]);

    let run = store.load_run("run_1")?;
    assert_eq!(run.status, AutoReviewRunStatus::Lost);
    Ok(())
}

#[test]
fn save_run_backfills_metadata_for_existing_index_runs() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = sample_run("run_1", &sample_output(Vec::new()));
    let mut index = AutoReviewRunsIndex::default();
    index.upsert(run.clone());
    let json = serde_json::to_string_pretty(&index)?;
    std::fs::create_dir_all(store.runs_path().parent().expect("runs parent"))?;
    std::fs::write(store.runs_path(), format!("{json}\n"))?;

    store.save_run(&sample_run("run_2", &sample_output(Vec::new())))?;
    corrupt_runs_index(&store)?;

    assert_eq!(
        run_ids(store.list_runs()?),
        vec!["run_1".to_string(), "run_2".to_string()]
    );
    assert_eq!(store.load_run("run_1")?, run);
    Ok(())
}

#[test]
fn save_run_prunes_metadata_for_runs_evicted_from_index() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    for index in 0..=DEFAULT_MAX_RUNS {
        let output = sample_output(Vec::new());
        store.save_run(&AutoReviewRun {
            run_id: format!("run_{index:03}"),
            started_at_unix_secs: index as i64,
            completed_at_unix_secs: Some(index as i64),
            ..sample_run("unused", &output)
        })?;
    }

    let evicted_metadata_path = store.run_metadata_path("run_000")?;
    corrupt_runs_index(&store)?;

    assert!(!evicted_metadata_path.exists());
    assert!(!run_ids(store.list_runs()?).contains(&"run_000".to_string()));
    Ok(())
}

#[test]
fn save_run_backfills_only_missing_metadata_sidecars() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    store.save_run(&sample_run("run_2", &sample_output(Vec::new())))?;

    // Tag an existing sidecar so a rewrite would be observable, and drop another so the
    // backfill has something to restore.
    let retained_path = store.run_metadata_path("run_1")?;
    let mut tagged: AutoReviewRun =
        serde_json::from_str(&std::fs::read_to_string(&retained_path)?)?;
    tagged.started_at_unix_secs = 4242;
    std::fs::write(
        &retained_path,
        format!("{}\n", serde_json::to_string_pretty(&tagged)?),
    )?;
    let backfilled_path = store.run_metadata_path("run_2")?;
    std::fs::remove_file(&backfilled_path)?;

    store.save_run(&sample_run("run_3", &sample_output(Vec::new())))?;

    assert!(
        backfilled_path.exists(),
        "missing sidecar should be backfilled"
    );
    let retained: AutoReviewRun = serde_json::from_str(&std::fs::read_to_string(&retained_path)?)?;
    assert_eq!(
        retained.started_at_unix_secs, 4242,
        "sidecars that already exist should not be rewritten on unrelated saves"
    );

    corrupt_runs_index(&store)?;
    assert_eq!(
        run_ids(store.list_runs()?),
        vec![
            "run_1".to_string(),
            "run_2".to_string(),
            "run_3".to_string()
        ]
    );
    Ok(())
}

#[test]
fn save_run_rewrites_metadata_for_the_changed_run() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;

    let updated = AutoReviewRun {
        started_at_unix_secs: 99,
        ..sample_run("run_1", &sample_output(Vec::new()))
    };
    store.save_run(&updated)?;

    corrupt_runs_index(&store)?;
    assert_eq!(store.load_run("run_1")?, updated);
    Ok(())
}

#[test]
fn save_run_removes_state_sidecars_for_runs_evicted_from_index() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    for index in 0..DEFAULT_MAX_RUNS {
        let output = sample_output(Vec::new());
        store.save_run(&AutoReviewRun {
            run_id: format!("run_{index:03}"),
            started_at_unix_secs: index as i64,
            completed_at_unix_secs: Some(index as i64),
            ..sample_run("unused", &output)
        })?;
    }
    store.save_run_state(&AutoReviewRunState::new("run_000"))?;
    store.save_run_state(&AutoReviewRunState::new("run_001"))?;
    let evicted_state_path = store.run_state_path("run_000")?;
    let retained_state_path = store.run_state_path("run_001")?;
    assert!(evicted_state_path.exists());

    // The next save pushes the index past its cap and evicts the oldest run.
    let output = sample_output(Vec::new());
    store.save_run(&AutoReviewRun {
        run_id: format!("run_{DEFAULT_MAX_RUNS:03}"),
        started_at_unix_secs: DEFAULT_MAX_RUNS as i64,
        completed_at_unix_secs: Some(DEFAULT_MAX_RUNS as i64),
        ..sample_run("unused", &output)
    })?;

    assert!(
        !evicted_state_path.exists(),
        "evicted run state should be removed"
    );
    assert!(
        !store.run_metadata_path("run_000")?.exists(),
        "evicted run metadata should be removed"
    );
    assert!(
        retained_state_path.exists(),
        "retained run state should be kept"
    );
    Ok(())
}

#[test]
fn metadata_write_failure_does_not_update_index_or_prune_existing_metadata() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    store.save_run(&sample_run("run_1", &sample_output(Vec::new())))?;
    let before = std::fs::read_to_string(store.runs_path())?;
    let bad_metadata_path = store.run_metadata_path("run_2")?;
    std::fs::create_dir_all(&bad_metadata_path)?;

    let error = store
        .save_run(&sample_run("run_2", &sample_output(Vec::new())))
        .expect_err("metadata write failure should block index update");

    assert!(
        error
            .to_string()
            .contains("failed to write auto review run metadata"),
        "unexpected error: {error:#}"
    );
    assert_eq!(std::fs::read_to_string(store.runs_path())?, before);
    assert!(store.run_metadata_path("run_1")?.exists());
    Ok(())
}

#[test]
fn non_canonical_index_shape_recovers_as_empty() -> anyhow::Result<()> {
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

    assert!(store.list_runs()?.is_empty());

    let error = store
        .save_run(&sample_run("run_2", &sample_output(Vec::new())))
        .expect_err("non-canonical compact index should block writes");

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

    let detail = store.finding_detail("run_1", "f1", /*max_bytes*/ 120)?;

    assert_eq!(detail.kind, AutoReviewDetailKind::Finding);
    assert_eq!(detail.finding_id.as_deref(), Some("f1"));
    assert!(detail.truncated);
    assert!(detail.bytes <= 120);
    Ok(())
}

#[test]
fn detail_formats_finding_for_direct_llm_use() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("Body text")]);
    let run = sample_run("run_1", &output);
    store.save_run(&run)?;
    store.save_output("run_1", &output)?;

    let detail = store.finding_detail("run_1", "f1", DETAIL_MAX_BYTES)?;

    assert_eq!(detail.kind, AutoReviewDetailKind::Finding);
    assert_eq!(detail.finding_id.as_deref(), Some("f1"));
    assert_eq!(detail.finding_count, 1);
    assert_eq!(detail.omitted_findings, 0);
    assert!(detail.content.contains("finding_id=f1 priority=1"));
    assert!(detail.content.contains("location=/tmp/example.rs:7-9"));
    assert!(detail.content.contains("title: Body text"));
    assert!(detail.content.contains("body:\nBody Body text"));
    assert!(!detail.content.contains("code_location"));
    Ok(())
}

#[test]
fn finding_detail_reports_other_findings_without_marking_content_truncated() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(vec![sample_finding("First"), sample_finding("Second")]);
    let run = sample_run("run_1", &output);
    store.save_run(&run)?;
    store.save_output("run_1", &output)?;

    let detail = store.finding_detail("run_1", "f2", DETAIL_MAX_BYTES)?;

    assert_eq!(detail.kind, AutoReviewDetailKind::Finding);
    assert_eq!(detail.finding_id.as_deref(), Some("f2"));
    assert_eq!(detail.finding_count, 2);
    assert_eq!(detail.omitted_findings, 1);
    assert!(!detail.truncated);
    assert!(detail.content.contains("title: Second"));
    assert!(!detail.content.contains("title: First"));
    Ok(())
}

#[test]
fn detail_formats_bounded_run_overview() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let output = sample_output(
        (1..=12)
            .map(|index| sample_finding(&format!("Body {index}")))
            .collect(),
    );
    let run = sample_run("run_1", &output);
    store.save_run(&run)?;
    store.save_output("run_1", &output)?;

    let detail = store.detail("run_1", /*finding_id*/ None, DETAIL_MAX_BYTES)?;

    assert_eq!(detail.kind, AutoReviewDetailKind::Run);
    assert_eq!(detail.finding_id, None);
    assert_eq!(detail.finding_count, 12);
    assert_eq!(detail.omitted_findings, 2);
    assert!(detail.truncated);
    assert!(
        detail
            .content
            .contains("overall_correctness: patch is incorrect")
    );
    assert!(detail.content.contains("finding_id=f1"));
    assert!(detail.content.contains("finding_id=f10"));
    assert!(!detail.content.contains("finding_id=f11"));
    assert!(detail.content.contains(
        "... omitted 2 additional finding(s); request a specific findingId for full detail"
    ));
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
    let stale_clean = AutoReviewRun {
        run_id: "stale_clean".to_string(),
        target: sample_target("main", "head-1", "/repo"),
        finding_count: 0,
        finding_digests: Vec::new(),
        ..sample_run("unused", &sample_output(Vec::new()))
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
        [
            &stale_finding,
            &stale_clean,
            &duplicate_skipped,
            &failed,
            &running,
        ],
        Some(&active_target),
        Some(&ReviewTarget::UncommittedChanges),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.recent_runs, 5);
    assert_eq!(diagnostics.in_flight_runs, 1);
    assert_eq!(diagnostics.terminal_runs, 4);
    assert_eq!(diagnostics.skipped_runs, 1);
    assert_eq!(diagnostics.duplicate_skipped_runs, 1);
    assert_eq!(diagnostics.failed_runs, 1);
    assert_eq!(diagnostics.suppressed_stale_runs, 2);
    assert_eq!(
        diagnostics.compact_line(),
        "recent_runs=5 in_flight=1 terminal=4 suppressed_stale=2 skipped=1 duplicate_skipped=1 failed=1"
    );
}

#[test]
fn diagnostics_include_token_and_elapsed_signals() {
    let active_target = sample_target("main", "head-2", "/repo");
    let high_token_run = AutoReviewRun {
        run_id: "high_token".to_string(),
        reasoning_effort: Some("medium".to_string()),
        prompt_token_estimate: Some(12_000),
        token_count: Some(25_915),
        completed_at_unix_secs: Some(45),
        ..sample_run("unused", &sample_output(Vec::new()))
    };
    let long_prompt_run = AutoReviewRun {
        run_id: "long_prompt".to_string(),
        prompt_token_estimate: Some(42_000),
        completed_at_unix_secs: Some(301),
        ..sample_run("unused", &sample_output(Vec::new()))
    };

    let diagnostics = AutoReviewDiagnostics::from_runs(
        [&high_token_run, &long_prompt_run],
        Some(&active_target),
        Some(&ReviewTarget::UncommittedChanges),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.token_count, 25_915);
    assert_eq!(diagnostics.token_runs, 1);
    assert_eq!(diagnostics.prompt_token_estimate, 54_000);
    assert_eq!(diagnostics.prompt_runs, 2);
    assert_eq!(diagnostics.high_burn_runs, 2);
    assert_eq!(diagnostics.longest_elapsed_bucket, Some("lt15m"));
    assert_eq!(
        diagnostics.compact_line(),
        "recent_runs=2 in_flight=0 terminal=2 tokens=25915t token_runs=1 prompt_estimate=54000t prompt_runs=2 high_burn=2 longest_elapsed=lt15m"
    );
}

#[test]
fn diagnostics_do_not_count_off_target_current_runs_as_stale_suppression() {
    let active_target = sample_target("main", "head-2", "/repo");
    let current_commit_finding = AutoReviewRun {
        run_id: "current_commit_finding".to_string(),
        review_target: ReviewTarget::Commit {
            sha: "head-2".to_string(),
            title: None,
        },
        ..sample_run(
            "unused",
            &sample_output(vec![sample_finding("Current commit")]),
        )
    };

    let diagnostics = AutoReviewDiagnostics::from_runs(
        [&current_commit_finding],
        Some(&active_target),
        Some(&ReviewTarget::UncommittedChanges),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.recent_runs, 1);
    assert_eq!(diagnostics.terminal_runs, 1);
    assert_eq!(diagnostics.suppressed_stale_runs, 0);
    assert_eq!(
        diagnostics.compact_line(),
        "recent_runs=1 in_flight=0 terminal=1"
    );
}

#[test]
fn diagnostics_count_stale_current_turn_diff_as_stale_suppression_for_uncommitted_changes() {
    let active_target = sample_target("main", "head-2", "/repo");
    let stale_turn_diff_finding = AutoReviewRun {
        run_id: "stale_turn_diff_finding".to_string(),
        target: sample_target("main", "head-1", "/repo"),
        review_target: ReviewTarget::CurrentTurnDiff {
            fingerprint: "sha256:first-turn".to_string(),
        },
        ..sample_run("unused", &sample_output(vec![sample_finding("Stale turn")]))
    };

    let diagnostics = AutoReviewDiagnostics::from_runs(
        [&stale_turn_diff_finding],
        Some(&active_target),
        Some(&ReviewTarget::UncommittedChanges),
    )
    .expect("diagnostics");

    assert_eq!(diagnostics.recent_runs, 1);
    assert_eq!(diagnostics.terminal_runs, 1);
    assert_eq!(diagnostics.suppressed_stale_runs, 1);
    assert_eq!(
        diagnostics.compact_line(),
        "recent_runs=1 in_flight=0 terminal=1 suppressed_stale=1"
    );
}

#[test]
fn diagnostics_are_absent_for_empty_runs() {
    assert_eq!(
        AutoReviewDiagnostics::from_runs(
            [],
            /*active_target*/ None,
            /*active_review_target*/ None
        ),
        None
    );
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
        .finding_detail("run_1", "missing", /*max_bytes*/ 120)
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
fn ledger_projection_selects_latest_and_current_runs() {
    let active_target = sample_target("main", "head-2", "/repo");
    let stale_latest = AutoReviewRun {
        run_id: "stale_latest".to_string(),
        target: sample_target("main", "head-1", "/repo"),
        started_at_unix_secs: 30,
        completed_at_unix_secs: Some(40),
        ..sample_run("unused", &sample_output(vec![sample_finding("Stale")]))
    };
    let current_older = AutoReviewRun {
        run_id: "current_older".to_string(),
        started_at_unix_secs: 10,
        completed_at_unix_secs: Some(20),
        ..sample_run("unused", &sample_output(vec![sample_finding("Current")]))
    };

    let projection = AutoReviewLedgerProjection::from_runs(
        [&stale_latest, &current_older],
        &active_target,
        &ReviewTarget::UncommittedChanges,
    );

    assert_eq!(
        projection.latest.as_ref().map(|run| run.run_id.as_str()),
        Some("stale_latest")
    );
    assert_eq!(
        projection.current.as_ref().map(|run| run.run_id.as_str()),
        Some("current_older")
    );
    assert_eq!(
        projection
            .current
            .as_ref()
            .map(|run| run.summary.content.as_str()),
        Some("[P1] f1: Current (/tmp/example.rs:7-9)")
    );
}

#[test]
fn ledger_projection_groups_status_counts_by_target_match() {
    let active_target = sample_target("main", "head-2", "/repo");
    let current_completed = sample_run("current_completed", &sample_output(Vec::new()));
    let stale_completed = AutoReviewRun {
        run_id: "stale_completed".to_string(),
        target: sample_target("main", "head-1", "/repo"),
        ..sample_run("unused", &sample_output(Vec::new()))
    };
    let running = AutoReviewRun {
        run_id: "running".to_string(),
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("unused", &sample_output(Vec::new()))
    };

    let projection = AutoReviewLedgerProjection::from_runs(
        [&current_completed, &stale_completed, &running],
        &active_target,
        &ReviewTarget::UncommittedChanges,
    );

    assert_eq!(projection.status_counts.len(), 3);
    assert!(projection.status_counts.iter().any(|count| {
        count.status == AutoReviewRunStatus::Completed
            && count.freshness == AutoReviewFreshness::Current
            && count.target_matches
            && count.count == 1
    }));
    assert!(projection.status_counts.iter().any(|count| {
        count.status == AutoReviewRunStatus::Completed
            && count.freshness == AutoReviewFreshness::Stale
            && !count.target_matches
            && count.count == 1
    }));
    assert!(projection.status_counts.iter().any(|count| {
        count.status == AutoReviewRunStatus::Running
            && count.freshness == AutoReviewFreshness::Current
            && count.target_matches
            && count.count == 1
    }));
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
    assert_eq!(duplicate.token_count, None);
    assert_eq!(duplicate.prompt_token_estimate, None);
    assert_eq!(
        duplicate.disposition,
        AutoReviewDuplicateDisposition::ReuseTerminal
    );
    Ok(())
}

#[test]
fn duplicate_lookup_uses_current_turn_diff_fingerprint_for_clean_target() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let review_target = ReviewTarget::CurrentTurnDiff {
        fingerprint: "sha256:committed-turn".to_string(),
    };
    let completed = AutoReviewRun {
        target: sample_target("main", "head-2", "/repo"),
        review_target: review_target.clone(),
        ..sample_run("completed-clean-turn", &sample_output(Vec::new()))
    };
    store.save_run(&completed)?;

    let duplicate = store
        .find_duplicate_by_fingerprint_with_target_proof_and_filter(
            "sha256:committed-turn",
            Some(&sample_target("main", "head-2", "/repo")),
            Some(&review_target),
            |_| true,
        )?
        .expect("clean committed turn duplicate should be found");

    assert_eq!(duplicate.run_id, "completed-clean-turn");
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
fn mark_superseded_by_fingerprint_respects_candidate_filter() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    for run_id in ["same-owner", "foreign-owner"] {
        store.save_run(&AutoReviewRun {
            target: target_with_fingerprint("sha256:abc"),
            ..sample_run(run_id, &sample_output(Vec::new()))
        })?;
    }

    let changed = store.mark_superseded_by_fingerprint_with_target_and_filter(
        "sha256:abc",
        "new_run",
        Some("main"),
        Some("head-2"),
        /*active_review_target*/ None,
        |run| run.run_id == "same-owner",
    )?;

    assert_eq!(changed, 1);
    assert_eq!(
        store.load_run("same-owner")?.status,
        AutoReviewRunStatus::Superseded
    );
    assert_eq!(
        store.load_run("foreign-owner")?.status,
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
        "sha256:second-turn",
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
fn duplicate_identity_prefers_current_turn_diff_over_worktree_fingerprint() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let review_target = ReviewTarget::CurrentTurnDiff {
        fingerprint: "sha256:exact-turn".to_string(),
    };
    let mut stored_target = target_with_fingerprint("sha256:unrelated-worktree");
    stored_target.current_turn = Some(sample_current_turn_target(
        "turn/stored 1",
        "sha256:exact-turn",
    ));
    let run = AutoReviewRun {
        target: stored_target,
        review_target: review_target.clone(),
        ..sample_run("exact-turn", &sample_output(Vec::new()))
    };
    store.save_run(&run)?;
    let mut active_target = target_with_fingerprint("sha256:different-worktree");
    active_target.current_turn = Some(sample_current_turn_target(
        "turn/active 2",
        "sha256:exact-turn",
    ));

    let duplicate = store.find_duplicate_by_fingerprint_with_target_proof_and_filter(
        "sha256:exact-turn",
        Some(&active_target),
        Some(&review_target),
        |_| true,
    )?;

    assert_eq!(
        duplicate.map(|duplicate| duplicate.run_id),
        Some("exact-turn".to_string())
    );
    assert_eq!(
        store.find_duplicate_by_fingerprint_with_target_proof_and_filter(
            "sha256:unrelated-worktree",
            Some(&active_target),
            Some(&review_target),
            |_| true,
        )?,
        None
    );
    Ok(())
}

#[test]
fn current_turn_target_tolerates_newer_persisted_fields() -> anyhow::Result<()> {
    let target: AutoReviewCurrentTurnTarget = serde_json::from_value(serde_json::json!({
        "turn_id": "tools/call 7",
        "diff_fingerprint": "sha256:exact-turn",
        "changed_path_count": 1,
        "diff_bytes": 128,
        "construction_error": null,
        "newer_field": "ignored by older binaries"
    }))?;

    assert_eq!(target.turn_id, "tools/call 7");
    Ok(())
}

#[test]
fn current_turn_duplicate_does_not_cross_branch_target_proof() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let review_target = ReviewTarget::CurrentTurnDiff {
        fingerprint: "sha256:exact-turn".to_string(),
    };
    let mut stored_target = sample_target("main", "head-2", "/repo");
    stored_target.current_turn = Some(sample_current_turn_target("turn-main", "sha256:exact-turn"));
    let run = AutoReviewRun {
        target: stored_target,
        review_target: review_target.clone(),
        ..sample_run("stale-branch-turn", &sample_output(Vec::new()))
    };
    store.save_run(&run)?;
    let mut active_target = sample_target("other-branch", "head-2", "/repo");
    active_target.current_turn = Some(sample_current_turn_target(
        "turn-other",
        "sha256:exact-turn",
    ));

    let duplicate = store.find_duplicate_by_fingerprint_with_target_proof_and_filter(
        "sha256:exact-turn",
        Some(&active_target),
        Some(&review_target),
        |_| true,
    )?;

    assert_eq!(duplicate, None);
    Ok(())
}

#[test]
fn mark_superseded_by_fingerprint_uses_current_turn_diff_for_clean_target() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let review_target = ReviewTarget::CurrentTurnDiff {
        fingerprint: "sha256:committed-turn".to_string(),
    };
    let clean = AutoReviewRun {
        target: sample_target("main", "head-2", "/repo"),
        review_target: review_target.clone(),
        ..sample_run("clean-committed-turn", &sample_output(Vec::new()))
    };
    store.save_run(&clean)?;

    let changed = store.mark_superseded_by_fingerprint_with_target(
        "sha256:committed-turn",
        "new-run",
        Some("main"),
        Some("head-2"),
        Some(&review_target),
    )?;

    assert_eq!(changed, 1);
    assert_eq!(
        store.load_run("clean-committed-turn")?.status,
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

    let changed = store
        .reconcile_orphaned_in_flight(std::iter::empty::<&str>(), /*now_unix_secs*/ 99)?;

    let running = store.load_run("running")?;
    let completed = store.load_run("completed")?;
    assert_eq!(run_ids(changed), vec!["running".to_string()]);
    assert_eq!(running.status, AutoReviewRunStatus::Lost);
    assert_eq!(running.freshness, AutoReviewRunFreshness::Lost);
    assert_eq!(running.completed_at_unix_secs, Some(99));
    assert_eq!(
        running.error_summary.as_deref(),
        Some("review did not survive process restart")
    );
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

    let changed = store.reconcile_orphaned_in_flight(["live_manual"], /*now_unix_secs*/ 99)?;

    let manual = store.load_run("manual")?;
    let background = store.load_run("background")?;
    let live_manual = store.load_run("live_manual")?;
    assert_eq!(
        run_ids(changed),
        vec!["background".to_string(), "manual".to_string()]
    );
    assert_eq!(manual.status, AutoReviewRunStatus::Lost);
    assert_eq!(manual.freshness, AutoReviewRunFreshness::Lost);
    assert_eq!(manual.completed_at_unix_secs, Some(99));
    assert_eq!(
        manual.error_summary.as_deref(),
        Some("review did not survive process restart")
    );
    assert_eq!(background.status, AutoReviewRunStatus::Lost);
    assert_eq!(background.freshness, AutoReviewRunFreshness::Lost);
    assert_eq!(background.completed_at_unix_secs, Some(99));
    assert_eq!(
        background.error_summary.as_deref(),
        Some("background review did not survive process restart")
    );
    assert_eq!(live_manual.status, AutoReviewRunStatus::Running);
    assert_eq!(live_manual.freshness, AutoReviewRunFreshness::Current);
    assert_eq!(live_manual.completed_at_unix_secs, None);
    Ok(())
}

#[test]
fn reconcile_orphaned_in_flight_respects_source_filter() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let manual = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        ..sample_run("manual", &sample_output(Vec::new()))
    };
    let background = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        source: AutoReviewRunSource::Background,
        completed_at_unix_secs: None,
        ..sample_run("background", &sample_output(Vec::new()))
    };
    store.save_run(&manual)?;
    store.save_run(&background)?;

    let changed = store.reconcile_orphaned_in_flight_with_filter(
        std::iter::empty::<&str>(),
        /*now_unix_secs*/ 99,
        |run| run.source == AutoReviewRunSource::Background,
    )?;

    assert_eq!(run_ids(changed), vec!["background".to_string()]);
    assert_eq!(
        store.load_run("background")?.status,
        AutoReviewRunStatus::Lost
    );
    assert_eq!(
        store.load_run("manual")?.status,
        AutoReviewRunStatus::Running
    );
    Ok(())
}

#[test]
fn reconcile_orphaned_in_flight_preserves_existing_error_summary() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let scope = tempfile::tempdir()?;
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());
    let run = AutoReviewRun {
        status: AutoReviewRunStatus::Running,
        completed_at_unix_secs: None,
        error_summary: Some("provider disconnected".to_string()),
        ..sample_run("manual_with_error", &sample_output(Vec::new()))
    };
    store.save_run(&run)?;

    store.reconcile_orphaned_in_flight(std::iter::empty::<&str>(), /*now_unix_secs*/ 99)?;

    assert_eq!(
        store
            .load_run("manual_with_error")?
            .error_summary
            .as_deref(),
        Some("provider disconnected")
    );
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
        (
            AutoReviewRunFreshness::Obsolete,
            AutoReviewRunStatus::Completed,
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

fn sample_current_turn_target(turn_id: &str, fingerprint: &str) -> AutoReviewCurrentTurnTarget {
    AutoReviewCurrentTurnTarget {
        turn_id: turn_id.to_string(),
        diff_fingerprint: Some(fingerprint.to_string()),
        changed_path_count: Some(1),
        diff_bytes: Some(128),
        construction_error: None,
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
        current_turn: None,
        build_provenance: None,
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
