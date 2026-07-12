use std::borrow::Cow;

use pretty_assertions::assert_eq;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;

use super::STATE_MIGRATOR;

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
        no_tx: STATE_MIGRATOR.no_tx,
        table_name: STATE_MIGRATOR.table_name.clone(),
        create_schemas: STATE_MIGRATOR.create_schemas.clone(),
    }
}

#[tokio::test]
async fn visible_thread_indexes_apply_after_fork_migrations() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory database should open");
    migrator_through(/*version*/ 37)
        .run(&pool)
        .await
        .expect("fork migrations should apply");

    let fork_columns = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('threads') WHERE name IN ('history_mode', 'session_provenance') ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("fork columns should load");
    assert_eq!(
        fork_columns,
        vec!["history_mode".to_string(), "session_provenance".to_string()]
    );

    migrator_through(/*version*/ 38)
        .run(&pool)
        .await
        .expect("remapped upstream migration should apply");

    let visible_indexes = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN ('idx_threads_visible_created_at_ms', 'idx_threads_visible_updated_at_ms') ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .expect("visible thread indexes should load");
    assert_eq!(
        visible_indexes,
        vec![
            "idx_threads_visible_created_at_ms".to_string(),
            "idx_threads_visible_updated_at_ms".to_string(),
        ]
    );

    let applied_versions = sqlx::query_scalar::<_, i64>(
        "SELECT version FROM _sqlx_migrations WHERE version >= 36 ORDER BY version",
    )
    .fetch_all(&pool)
    .await
    .expect("applied migration versions should load");
    assert_eq!(applied_versions, vec![36, 37, 38]);

    let mut older_runtime_migrator = migrator_through(/*version*/ 37);
    older_runtime_migrator.ignore_missing = true;
    older_runtime_migrator
        .run(&pool)
        .await
        .expect("older runtime should accept the remapped newer migration");
}
