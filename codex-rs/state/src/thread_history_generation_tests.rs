//! Regression coverage for the `thread_history_1.sqlite` -> `thread_history_2.sqlite`
//! generation bump.
//!
//! Released builds shipped a `thread_history_1.sqlite` whose ledger records the
//! original migrations 1 and 2. Both were rewritten in place afterwards, so the
//! embedded migration set can never validate against those files again. The
//! projection is derived from rollout files, so the fix is a filename generation
//! bump: the shipped file is left untouched and a fresh database is rebuilt.

use codex_utils_absolute_path::test_support::PathExt;
use sqlx::SqlStr;
use sqlx::migrate::Migration;
use sqlx::migrate::MigrationType;
use sqlx::migrate::Migrator;
use std::borrow::Cow;

use crate::migrations::runtime_thread_history_migrator;

/// `thread_history_migrations/0001_thread_history.sql` exactly as released.
const SHIPPED_THREAD_HISTORY_V1_SQL: &str = r#"CREATE TABLE thread_turns (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    status TEXT NOT NULL,
    error_json TEXT,
    started_at INTEGER,
    completed_at INTEGER,
    duration_ms INTEGER,
    first_user_item_id TEXT,
    final_agent_item_id TEXT,
    PRIMARY KEY (thread_id, turn_id)
);

CREATE UNIQUE INDEX idx_thread_turns_page
    ON thread_turns(thread_id, rollout_ordinal);

CREATE TABLE thread_items (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    item_json TEXT NOT NULL,
    PRIMARY KEY (thread_id, turn_id, item_id)
);

CREATE UNIQUE INDEX idx_thread_items_page
    ON thread_items(thread_id, rollout_ordinal);

CREATE INDEX idx_thread_items_by_turn_page
    ON thread_items(thread_id, turn_id, rollout_ordinal);

CREATE TABLE thread_history_projection_state (
    thread_id TEXT PRIMARY KEY,
    next_rollout_byte_offset INTEGER NOT NULL,
    next_rollout_ordinal INTEGER NOT NULL
);
"#;

/// `thread_history_migrations/0002_projection_integrity.sql` exactly as released.
const SHIPPED_THREAD_HISTORY_V2_SQL: &str = r#"ALTER TABLE thread_history_projection_state
    ADD COLUMN rollout_source_id TEXT;

ALTER TABLE thread_history_projection_state
    ADD COLUMN rollout_fingerprint TEXT;

ALTER TABLE thread_history_projection_state
    ADD COLUMN projection_generation INTEGER NOT NULL DEFAULT 0
        CHECK (projection_generation >= 0);

ALTER TABLE thread_history_projection_state
    ADD COLUMN projection_status TEXT NOT NULL DEFAULT 'dirty'
        CHECK (projection_status IN ('clean', 'dirty'));

ALTER TABLE thread_items
    ADD COLUMN item_created_at_ms INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_thread_history_projection_state_dirty
    ON thread_history_projection_state(projection_status, next_rollout_ordinal, thread_id);

CREATE TRIGGER thread_turns_delete_items
AFTER DELETE ON thread_turns
BEGIN
    DELETE FROM thread_items
    WHERE thread_id = OLD.thread_id AND turn_id = OLD.turn_id;
END;
"#;

fn shipped_thread_history_migrator() -> Migrator {
    Migrator::with_migrations(vec![
        Migration::new(
            1,
            Cow::Borrowed("thread history"),
            MigrationType::Simple,
            SqlStr::from_static(SHIPPED_THREAD_HISTORY_V1_SQL),
            /*no_tx*/ false,
        ),
        Migration::new(
            2,
            Cow::Borrowed("projection integrity"),
            MigrationType::Simple,
            SqlStr::from_static(SHIPPED_THREAD_HISTORY_V2_SQL),
            /*no_tx*/ false,
        ),
    ])
}

#[tokio::test]
async fn shipped_thread_history_v1_database_cannot_block_opening_the_store() {
    let sqlite_home = crate::runtime::test_support::unique_temp_dir();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("sqlite home should be created");
    let _cleanup = scopeguard::guard(sqlite_home.clone(), |sqlite_home| {
        let _ = std::fs::remove_dir_all(sqlite_home);
    });
    let sqlite = crate::SqliteConfig::new_for_testing(sqlite_home.as_path().abs());

    let shipped_path = sqlite_home.join("thread_history_1.sqlite");
    let shipped_pool = sqlite
        .open_read_write_pool(&shipped_path)
        .await
        .expect("shipped thread history database should open");
    shipped_thread_history_migrator()
        .run(&shipped_pool)
        .await
        .expect("shipped thread history migrations should apply");
    sqlx::query(
        r#"
INSERT INTO thread_items (thread_id, turn_id, item_id, rollout_ordinal, item_json)
VALUES ('thread-1', 'turn-1', 'item-1', 1, '{}')
        "#,
    )
    .execute(&shipped_pool)
    .await
    .expect("shipped projection row should insert");

    // The rewritten migrations 1 and 2 can never validate against this ledger again,
    // which is exactly why the filename generation was bumped instead of migrated.
    let mismatch = runtime_thread_history_migrator()
        .run(&shipped_pool)
        .await
        .expect_err("rewritten migrations must not validate against the shipped ledger");
    assert!(
        matches!(mismatch, sqlx::migrate::MigrateError::VersionMismatch(1)),
        "expected a checksum mismatch on version 1, got {mismatch:?}"
    );
    shipped_pool.close().await;

    let pool = crate::open_thread_history_db(&sqlite)
        .await
        .expect("current thread history database should open alongside the shipped file");
    pool.close().await;

    assert_eq!(
        sqlite.thread_history_db_path(),
        sqlite_home.join("thread_history_2.sqlite")
    );

    let preserved_pool = sqlite
        .open_read_only_pool(&shipped_path, /*busy_timeout*/ None)
        .await
        .expect("shipped thread history database should still be readable");
    let preserved_ledger = sqlx::query_as::<_, (i64, String)>(
        "SELECT version, description FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(&preserved_pool)
    .await
    .expect("shipped ledger should load");
    assert_eq!(
        preserved_ledger,
        vec![
            (1, "thread history".to_string()),
            (2, "projection integrity".to_string()),
        ],
        "the shipped file must be left exactly as the released build wrote it"
    );
    let preserved_items = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT thread_id, turn_id, item_id, rollout_ordinal FROM thread_items ORDER BY item_id",
    )
    .fetch_all(&preserved_pool)
    .await
    .expect("shipped projection rows should load");
    assert_eq!(
        preserved_items,
        vec![(
            "thread-1".to_string(),
            "turn-1".to_string(),
            "item-1".to_string(),
            1,
        )]
    );
    preserved_pool.close().await;
}
