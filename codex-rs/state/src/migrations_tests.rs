use codex_utils_absolute_path::test_support::PathExt;
use sqlx::Connection;
use sqlx::Row;
use sqlx::SqlStr;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;
use std::borrow::Cow;

use super::STATE_MIGRATOR;
use super::THREAD_HISTORY_MIGRATOR;
use super::repair_legacy_agent_jobs_migration_checksum;
use super::repair_legacy_recency_migration_version;
use super::runtime_state_migrator;

/// Ledger rows recorded by released Codex Lab builds, taken from a production
/// `state_5.sqlite`. A shipped database sits at version 38 with exactly these
/// checksums, so renumbering or editing 36-38 breaks every upgrade with
/// `VersionMismatch`.
const SHIPPED_STATE_LEDGER: [(i64, &str, &str); 3] = [
    (
        36,
        "threads session provenance",
        "b38744bf3efe7b3a4e2e4b0c0c4f02fee8e86f52737d292861c6b3e743862071d455035212574cdd94ac2960569038f0",
    ),
    (
        37,
        "threads history mode",
        "a7a99674f90a43184e43d66595c4f1da50ae8715a5bfcb1579a2b7b10668335d7f7ab6b6f2d7a7a379272773d803a914",
    ),
    (
        38,
        "threads visible sort indexes",
        "1db0d894de7b47b979b02e5a77aa241eba5150007c9a4de75e2754814af8a841dd837874173996d87532ba4e35d06387",
    ),
];

fn checksum_hex(checksum: &[u8]) -> String {
    checksum.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn migrator_through(version: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            STATE_MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ignore_missing: STATE_MIGRATOR.ignore_missing,
        locking: STATE_MIGRATOR.locking,
        table_name: STATE_MIGRATOR.table_name.clone(),
        create_schemas: STATE_MIGRATOR.create_schemas.clone(),
        no_tx: STATE_MIGRATOR.no_tx,
    }
}

#[tokio::test]
async fn pinned_threads_migration_defaults_existing_and_legacy_rows_to_unpinned() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    migrator_through(/*version*/ 43)
        .run(&pool)
        .await
        .expect("pre-pin migrations should apply");

    for thread_id in [
        "00000000-0000-0000-0000-000000000043",
        "00000000-0000-0000-0000-000000000044",
    ] {
        if thread_id.ends_with("44") {
            STATE_MIGRATOR
                .run(&pool)
                .await
                .expect("pin migration should apply");
        }
        sqlx::query(
            r#"
INSERT INTO threads (
    id,
    rollout_path,
    created_at,
    updated_at,
    created_at_ms,
    updated_at_ms,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(thread_id)
        .bind("/tmp/legacy.jsonl")
        .bind(1_700_000_000_i64)
        .bind(1_700_000_000_i64)
        .bind(1_700_000_000_000_i64)
        .bind(1_700_000_000_000_i64)
        .bind("cli")
        .bind("openai")
        .bind("/tmp")
        .bind("")
        .bind("read-only")
        .bind("on-request")
        .execute(&pool)
        .await
        .expect("legacy thread insert should succeed");
    }

    let pinned_values = sqlx::query_scalar::<_, bool>("SELECT is_pinned FROM threads ORDER BY id")
        .fetch_all(&pool)
        .await
        .expect("pin states should load");
    assert_eq!(pinned_values, vec![false, false]);

    pool.close().await;
}

#[tokio::test]
async fn thread_item_update_ordinals_allow_older_writers() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let pre_update_ordinal_migrator = Migrator {
        migrations: Cow::Owned(
            THREAD_HISTORY_MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 4)
                .cloned()
                .collect(),
        ),
        ignore_missing: THREAD_HISTORY_MIGRATOR.ignore_missing,
        locking: THREAD_HISTORY_MIGRATOR.locking,
        table_name: THREAD_HISTORY_MIGRATOR.table_name.clone(),
        create_schemas: THREAD_HISTORY_MIGRATOR.create_schemas.clone(),
        no_tx: THREAD_HISTORY_MIGRATOR.no_tx,
    };
    let pool = sqlite
        .open_thread_history_db(
            &pre_update_ordinal_migrator,
            /*telemetry_override*/ None,
        )
        .await
        .expect("pre-update-ordinal migrations should apply");
    sqlx::query(
        r#"
INSERT INTO thread_items (thread_id, turn_id, item_id, rollout_ordinal, created_at_ms, item_type, item_json) VALUES
    ('thread-1', 'turn-1', 'existing-item-1', 11, 1_100, 'userMessage', '{}'),
    ('thread-1', 'turn-1', 'existing-item-2', 12, 1_200, 'userMessage', '{}')
        "#,
    )
    .execute(&pool)
    .await
    .expect("pre-migration items should be inserted");
    THREAD_HISTORY_MIGRATOR
        .run(&pool)
        .await
        .expect("update-ordinal migration should apply");
    sqlx::query(
        r#"
INSERT INTO thread_items (thread_id, turn_id, item_id, rollout_ordinal, created_at_ms, item_type, item_json) VALUES
    ('thread-1', 'turn-1', 'old-writer-item-1', 13, 1_300, 'userMessage', '{}'),
    ('thread-1', 'turn-1', 'old-writer-item-2', 14, 1_400, 'userMessage', '{}')
        "#,
    )
    .execute(&pool)
    .await
    .expect("older writers should be able to append multiple items after migration");
    let ordinals = sqlx::query_as::<_, (i64, i64)>(
        "SELECT rollout_ordinal, updated_at_ordinal FROM thread_items WHERE thread_id = ? ORDER BY rollout_ordinal",
    )
    .bind("thread-1")
    .fetch_all(&pool)
    .await
    .expect("old-writer items should load");
    assert_eq!(ordinals, vec![(11, 11), (12, 12), (13, 0), (14, 0)]);

    pool.close().await;
}

/// The last version a released Codex Lab build recorded. Everything above it is
/// still unshipped and may be rewritten; everything at or below it is frozen.
const SHIPPED_STATE_LEDGER_HEAD: i64 = 38;

#[tokio::test]
async fn agent_job_rows_survive_upgrade() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    migrator_through(/*version*/ 15)
        .run(&pool)
        .await
        .expect("agent job migrations should apply");

    sqlx::query(
        r#"
INSERT INTO agent_jobs (
    id,
    name,
    status,
    instruction,
    input_headers_json,
    input_csv_path,
    output_csv_path,
    created_at,
    updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind("job-1")
    .bind("legacy job")
    .bind("running")
    .bind("process rows")
    .bind(r#"["path"]"#)
    .bind("/tmp/input.csv")
    .bind("/tmp/output.csv")
    .bind(1_700_000_000_i64)
    .bind(1_700_000_000_i64)
    .execute(&pool)
    .await
    .expect("legacy agent job should insert");
    sqlx::query(
        r#"
INSERT INTO agent_job_items (
    job_id,
    item_id,
    row_index,
    row_json,
    status,
    result_json,
    created_at,
    updated_at
) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind("job-1")
    .bind("item-1")
    .bind(0_i64)
    .bind(r#"{"path":"secret.csv"}"#)
    .bind("completed")
    .bind(r#"{"result":"legacy"}"#)
    .bind(1_700_000_000_i64)
    .bind(1_700_000_000_i64)
    .execute(&pool)
    .await
    .expect("legacy agent job item should insert");

    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");

    let agent_job_tables = sqlx::query_scalar::<_, String>(
        r#"
SELECT name
FROM sqlite_master
WHERE type = 'table' AND name IN ('agent_jobs', 'agent_job_items')
ORDER BY name
        "#,
    )
    .fetch_all(&pool)
    .await
    .expect("remaining agent job tables should load");
    assert_eq!(
        agent_job_tables,
        vec!["agent_job_items".to_string(), "agent_jobs".to_string()],
        "agent-jobs is still pending_restore, so its tables must survive the upgrade"
    );

    let jobs = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT id, name, status, instruction FROM agent_jobs ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("agent jobs should load");
    assert_eq!(
        jobs,
        vec![(
            "job-1".to_string(),
            "legacy job".to_string(),
            "running".to_string(),
            "process rows".to_string(),
        )]
    );

    let job_items = sqlx::query_as::<_, (String, String, i64, String, String, String)>(
        "SELECT job_id, item_id, row_index, row_json, status, result_json FROM agent_job_items ORDER BY item_id",
    )
    .fetch_all(&pool)
    .await
    .expect("agent job items should load");
    assert_eq!(
        job_items,
        vec![(
            "job-1".to_string(),
            "item-1".to_string(),
            0,
            r#"{"path":"secret.csv"}"#.to_string(),
            "completed".to_string(),
            r#"{"result":"legacy"}"#.to_string(),
        )]
    );

    pool.close().await;
}

/// Unshipped migrations are still editable, so a destructive statement that lands
/// above the shipped head can be removed before anyone upgrades onto it. Shipped
/// versions are excluded: 23, 31, 34, and 35 legitimately dropped tables and their
/// SQL is frozen.
#[test]
fn unshipped_state_migrations_do_not_destroy_data() {
    const DESTRUCTIVE_STATEMENTS: [&str; 3] = ["drop table", "drop column", "delete from"];

    let destructive = STATE_MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version > SHIPPED_STATE_LEDGER_HEAD)
        .filter_map(|migration| {
            let sql = strip_sql_line_comments(migration.sql.as_ref()).to_lowercase();
            DESTRUCTIVE_STATEMENTS
                .iter()
                .find(|statement| sql.contains(**statement))
                .map(|statement| (migration.version, (*statement).to_string()))
        })
        .collect::<Vec<_>>();

    assert_eq!(
        destructive,
        Vec::new(),
        "unshipped migrations must not destroy user data; drop the statement or gate it behind a restore"
    );
}

fn strip_sql_line_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| line.split_once("--").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn recency_migration_backfills_and_seeds_old_binary_inserts() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    migrator_through(/*version*/ 40)
        .run(&pool)
        .await
        .expect("pre-recency migrations should apply");

    sqlx::query(
        r#"
INSERT INTO threads (
    id,
    rollout_path,
    created_at,
    updated_at,
    created_at_ms,
    updated_at_ms,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind("00000000-0000-0000-0000-000000000001")
    .bind("/tmp/first.jsonl")
    .bind(1_700_000_000_i64)
    .bind(1_700_000_100_i64)
    .bind(1_700_000_000_123_i64)
    .bind(1_700_000_100_456_i64)
    .bind("cli")
    .bind("openai")
    .bind("/tmp")
    .bind("")
    .bind("read-only")
    .bind("on-request")
    .execute(&pool)
    .await
    .expect("legacy row should insert");

    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("recency migration should apply");

    let backfilled = sqlx::query(
        "SELECT updated_at, updated_at_ms, recency_at, recency_at_ms FROM threads WHERE id = ?",
    )
    .bind("00000000-0000-0000-0000-000000000001")
    .fetch_one(&pool)
    .await
    .expect("backfilled row should load");
    assert_eq!(backfilled.get::<i64, _>("recency_at"), 1_700_000_100);
    assert_eq!(backfilled.get::<i64, _>("recency_at_ms"), 1_700_000_100_456);

    sqlx::query(
        r#"
INSERT INTO threads (
    id,
    rollout_path,
    created_at,
    updated_at,
    created_at_ms,
    updated_at_ms,
    source,
    model_provider,
    cwd,
    title,
    sandbox_policy,
    approval_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind("00000000-0000-0000-0000-000000000002")
    .bind("/tmp/second.jsonl")
    .bind(1_700_000_200_i64)
    .bind(1_700_000_300_i64)
    .bind(1_700_000_200_123_i64)
    .bind(1_700_000_300_456_i64)
    .bind("cli")
    .bind("openai")
    .bind("/tmp")
    .bind("")
    .bind("read-only")
    .bind("on-request")
    .execute(&pool)
    .await
    .expect("old-binary row should insert");

    let seeded = sqlx::query("SELECT recency_at, recency_at_ms FROM threads WHERE id = ?")
        .bind("00000000-0000-0000-0000-000000000002")
        .fetch_one(&pool)
        .await
        .expect("old-binary row should load");
    assert_eq!(seeded.get::<i64, _>("recency_at"), 1_700_000_300);
    assert_eq!(seeded.get::<i64, _>("recency_at_ms"), 1_700_000_300_456);

    pool.close().await;
}

/// Version 43 was rewritten in place from `drop agent jobs` to an inert placeholder. A candidate
/// or development database that already applied the destructive version recorded its checksum, so
/// without a repair every later `Migrator::run` fails with `VersionMismatch(43)` and the database
/// never opens again.
#[tokio::test]
async fn repairs_agent_jobs_migration_that_was_applied_before_the_rewrite() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");

    let placeholder = STATE_MIGRATOR
        .migrations
        .iter()
        .find(|migration| migration.description == "retain agent jobs")
        .expect("agent-jobs placeholder migration should exist");
    let mut legacy_migrations = STATE_MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version < placeholder.version)
        .cloned()
        .collect::<Vec<_>>();
    legacy_migrations.push(Migration::new(
        placeholder.version,
        Cow::Borrowed("drop agent jobs"),
        placeholder.migration_type,
        SqlStr::from_static(
            "DROP TABLE IF EXISTS agent_job_items;\nDROP TABLE IF EXISTS agent_jobs;\n",
        ),
        placeholder.no_tx,
    ));
    Migrator::with_migrations(legacy_migrations)
        .run(&pool)
        .await
        .expect("the pre-rewrite ledger should apply");

    // The rewrite is exactly what an unrepaired upgrade cannot get past.
    let unrepaired = STATE_MIGRATOR.run(&pool).await;
    assert!(
        matches!(
            unrepaired,
            Err(sqlx::migrate::MigrateError::VersionMismatch(version)) if version == placeholder.version
        ),
        "expected VersionMismatch({}), got {unrepaired:?}",
        placeholder.version
    );

    repair_legacy_agent_jobs_migration_checksum(&pool, &STATE_MIGRATOR)
        .await
        .expect("legacy agent-jobs checksum should be repaired");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply after repair");

    let repaired =
        sqlx::query("SELECT description, checksum FROM _sqlx_migrations WHERE version = ?")
            .bind(placeholder.version)
            .fetch_one(&pool)
            .await
            .expect("the repaired row should load");
    assert_eq!(
        (
            repaired.get::<String, _>("description"),
            repaired.get::<Vec<u8>, _>("checksum"),
        ),
        (
            placeholder.description.to_string(),
            placeholder.checksum.to_vec(),
        )
    );

    pool.close().await;
}

/// The repair is gated on the one checksum the destructive version produced, so a row recorded by
/// any other migration at that version is left for `Migrator::run` to reject.
#[tokio::test]
async fn leaves_an_unrelated_version_43_checksum_untouched() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");

    let placeholder = STATE_MIGRATOR
        .migrations
        .iter()
        .find(|migration| migration.description == "retain agent jobs")
        .expect("agent-jobs placeholder migration should exist");
    let mut legacy_migrations = STATE_MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version < placeholder.version)
        .cloned()
        .collect::<Vec<_>>();
    legacy_migrations.push(Migration::new(
        placeholder.version,
        Cow::Borrowed("some other migration"),
        placeholder.migration_type,
        SqlStr::from_static("SELECT 2;\n"),
        placeholder.no_tx,
    ));
    let unrelated = legacy_migrations
        .last()
        .expect("the seeded migration should be present")
        .clone();
    Migrator::with_migrations(legacy_migrations)
        .run(&pool)
        .await
        .expect("the seeded ledger should apply");

    repair_legacy_agent_jobs_migration_checksum(&pool, &STATE_MIGRATOR)
        .await
        .expect("the repair should succeed without matching anything");

    let row = sqlx::query("SELECT description, checksum FROM _sqlx_migrations WHERE version = ?")
        .bind(placeholder.version)
        .fetch_one(&pool)
        .await
        .expect("the seeded row should load");
    assert_eq!(
        (
            row.get::<String, _>("description"),
            row.get::<Vec<u8>, _>("checksum"),
        ),
        (
            unrelated.description.to_string(),
            unrelated.checksum.to_vec(),
        )
    );

    pool.close().await;
}

async fn assert_recency_repair_from_legacy_version(legacy_version: i64) {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    migrator_through(legacy_version - 1)
        .run(&pool)
        .await
        .expect("pre-recency migrations should apply");

    let recency_migration = STATE_MIGRATOR
        .migrations
        .iter()
        .find(|migration| migration.description == "threads recency at")
        .expect("recency migration should exist");
    let mut legacy_migrations = STATE_MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version < legacy_version)
        .cloned()
        .collect::<Vec<_>>();
    legacy_migrations.push(Migration::new(
        legacy_version,
        recency_migration.description.clone(),
        recency_migration.migration_type,
        recency_migration.sql.clone(),
        recency_migration.no_tx,
    ));
    let legacy_recency_migrator = Migrator::with_migrations(legacy_migrations);
    legacy_recency_migrator
        .run(&pool)
        .await
        .expect("legacy recency migration should apply under its legacy version");

    repair_legacy_recency_migration_version(&pool, &STATE_MIGRATOR)
        .await
        .expect("legacy migration history should be repaired");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply after repair");

    let applied = sqlx::query(
        "SELECT version, checksum FROM _sqlx_migrations WHERE version >= ? ORDER BY version",
    )
    .bind(legacy_version)
    .fetch_all(&pool)
    .await
    .expect("applied migrations should load")
    .into_iter()
    .map(|row| {
        (
            row.get::<i64, _>("version"),
            row.get::<Vec<u8>, _>("checksum"),
        )
    })
    .collect::<Vec<_>>();
    let expected = STATE_MIGRATOR
        .migrations
        .iter()
        .filter(|migration| migration.version >= legacy_version)
        .map(|migration| (migration.version, migration.checksum.to_vec()))
        .collect::<Vec<_>>();
    assert_eq!(applied, expected);

    pool.close().await;
}

#[tokio::test]
async fn repairs_recency_migration_that_was_applied_as_version_38() {
    assert_recency_repair_from_legacy_version(/*legacy_version*/ 38).await;
}

#[tokio::test]
async fn repairs_recency_migration_that_was_applied_as_version_39() {
    assert_recency_repair_from_legacy_version(/*legacy_version*/ 39).await;
}

#[tokio::test]
async fn repair_recency_migration_succeeds_while_another_connection_holds_writer_slot() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("database should open");
    STATE_MIGRATOR
        .run(&pool)
        .await
        .expect("current migrations should apply");
    let read_pool = sqlite
        .open_read_only_pool(&state_path)
        .await
        .expect("read-only pool should open");
    let mut write_connection = pool.acquire().await.expect("write connection should open");
    let write_transaction = write_connection
        .begin_with("BEGIN IMMEDIATE")
        .await
        .expect("write transaction should acquire the writer slot");

    let repair_result = repair_legacy_recency_migration_version(&read_pool, &STATE_MIGRATOR).await;

    write_transaction
        .rollback()
        .await
        .expect("write transaction should roll back");
    drop(write_connection);
    read_pool.close().await;
    pool.close().await;
    repair_result.expect("current migration history should not need the writer slot");
}

#[test]
fn shipped_state_ledger_versions_stay_frozen() {
    for (version, description, checksum) in SHIPPED_STATE_LEDGER {
        let migration = STATE_MIGRATOR
            .migrations
            .iter()
            .find(|migration| migration.version == version)
            .unwrap_or_else(|| panic!("state migration {version} should exist"));
        assert_eq!(
            (
                migration.description.as_ref(),
                checksum_hex(&migration.checksum).as_str()
            ),
            (description, checksum),
            "state migration {version} must stay byte-for-byte identical to the shipped release"
        );
    }
}

#[tokio::test]
async fn upgrades_database_shipped_at_state_version_38() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());
    let state_path = sqlite.state_db_path();
    let pool = sqlite
        .open_read_write_pool(&state_path)
        .await
        .expect("sqlite database should open");
    migrator_through(/*version*/ 38)
        .run(&pool)
        .await
        .expect("shipped migrations should apply");

    let shipped_ledger = sqlx::query(
        "SELECT version, description, checksum FROM _sqlx_migrations WHERE version >= 36 ORDER BY version",
    )
    .fetch_all(&pool)
    .await
    .expect("shipped ledger should load")
    .into_iter()
    .map(|row| {
        (
            row.get::<i64, _>("version"),
            row.get::<String, _>("description"),
            checksum_hex(&row.get::<Vec<u8>, _>("checksum")),
        )
    })
    .collect::<Vec<_>>();
    let expected_ledger = SHIPPED_STATE_LEDGER
        .into_iter()
        .map(|(version, description, checksum)| {
            (version, description.to_string(), checksum.to_string())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shipped_ledger, expected_ledger,
        "fixture should reproduce the ledger of a released Codex Lab database"
    );

    sqlx::query(
        r#"
INSERT INTO threads (
    id,
    rollout_path,
    created_at,
    updated_at,
    created_at_ms,
    updated_at_ms,
    source,
    model_provider,
    cwd,
    title,
    preview,
    sandbox_policy,
    approval_mode
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind("00000000-0000-0000-0000-000000000038")
    .bind("/tmp/shipped.jsonl")
    .bind(1_700_000_000_i64)
    .bind(1_700_000_000_i64)
    .bind(1_700_000_000_000_i64)
    .bind(1_700_000_000_000_i64)
    .bind("cli")
    .bind("openai")
    .bind("/tmp")
    .bind("")
    .bind("shipped preview")
    .bind("read-only")
    .bind("on-request")
    .execute(&pool)
    .await
    .expect("shipped thread insert should succeed");

    let migrator = runtime_state_migrator();
    repair_legacy_recency_migration_version(&pool, &migrator)
        .await
        .expect("shipped migration history should need no repair");
    migrator
        .run(&pool)
        .await
        .expect("shipped database should upgrade without a version mismatch");

    let applied = sqlx::query_scalar::<_, i64>("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .expect("ledger head should load");
    let head = STATE_MIGRATOR
        .migrations
        .iter()
        .map(|migration| migration.version)
        .max()
        .expect("state migrator should have migrations");
    assert_eq!(applied, head);

    let upgraded = sqlx::query(
        r#"
SELECT session_provenance, history_mode, recency_at_ms, is_pinned, name
FROM threads
WHERE id = ?
        "#,
    )
    .bind("00000000-0000-0000-0000-000000000038")
    .fetch_one(&pool)
    .await
    .expect("shipped thread should survive the upgrade");
    assert_eq!(
        upgraded.get::<Option<String>, _>("session_provenance"),
        None
    );
    assert_eq!(upgraded.get::<String, _>("history_mode"), "legacy");
    assert_eq!(
        upgraded.get::<i64, _>("recency_at_ms"),
        1_700_000_000_000_i64
    );
    assert!(!upgraded.get::<bool, _>("is_pinned"));
    assert_eq!(upgraded.get::<Option<String>, _>("name"), None);

    let visible_indexes = sqlx::query_scalar::<_, String>(
        r#"
SELECT name
FROM sqlite_master
WHERE type = 'index' AND name IN (?, ?)
ORDER BY name
        "#,
    )
    .bind("idx_threads_visible_created_at_ms")
    .bind("idx_threads_visible_updated_at_ms")
    .fetch_all(&pool)
    .await
    .expect("visible sort indexes should load");
    assert_eq!(
        visible_indexes,
        vec![
            "idx_threads_visible_created_at_ms".to_string(),
            "idx_threads_visible_updated_at_ms".to_string(),
        ]
    );

    pool.close().await;
}
