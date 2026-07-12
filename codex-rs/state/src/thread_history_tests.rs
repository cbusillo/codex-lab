use std::borrow::Cow;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePoolOptions;

use super::*;
use crate::migrations::THREAD_HISTORY_MIGRATOR;

fn test_sqlite_home() -> PathBuf {
    std::env::temp_dir().join(format!("codex-thread-history-{}", uuid::Uuid::new_v4()))
}

fn source(fingerprint: &str) -> ThreadHistorySource {
    source_with_id("rollout-source-1", fingerprint)
}

fn source_with_id(source_id: &str, fingerprint: &str) -> ThreadHistorySource {
    ThreadHistorySource {
        rollout_source_id: source_id.to_string(),
        rollout_fingerprint: fingerprint.to_string(),
    }
}

fn migrator_through(version: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            THREAD_HISTORY_MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ignore_missing: THREAD_HISTORY_MIGRATOR.ignore_missing,
        locking: THREAD_HISTORY_MIGRATOR.locking,
        no_tx: THREAD_HISTORY_MIGRATOR.no_tx,
        table_name: THREAD_HISTORY_MIGRATOR.table_name.clone(),
        create_schemas: THREAD_HISTORY_MIGRATOR.create_schemas.clone(),
    }
}

fn suffix(
    mutations: Vec<ThreadHistoryMutation>,
    next_rollout_ordinal: u64,
    fingerprint: &str,
) -> ThreadHistorySuffix {
    ThreadHistorySuffix {
        mutations,
        checkpoint: ThreadHistoryCheckpointUpdate {
            next_rollout_byte_offset: next_rollout_ordinal * 100,
            next_rollout_ordinal,
            source: source(fingerprint),
        },
    }
}

fn turn(
    turn_id: &str,
    rollout_ordinal: i64,
    status: ThreadHistoryTurnStatus,
) -> ThreadHistoryMutation {
    ThreadHistoryMutation::UpsertTurn(ThreadHistoryTurnUpsert {
        target: ThreadHistoryTurnTarget::Id(turn_id.to_string()),
        rollout_ordinal,
        status,
        error_json: None,
        started_at: Some(rollout_ordinal * 10),
        completed_at: None,
        duration_ms: None,
    })
}

fn item(
    turn_id: &str,
    item_id: &str,
    rollout_ordinal: i64,
    item_created_at_ms: i64,
    item_json: &str,
) -> ThreadHistoryMutation {
    ThreadHistoryMutation::UpsertItem(ThreadHistoryItemUpsert {
        target: ThreadHistoryTurnTarget::Id(turn_id.to_string()),
        item_id: item_id.to_string(),
        rollout_ordinal,
        item_created_at_ms,
        item_json: item_json.to_string(),
    })
}

#[tokio::test]
async fn repository_opens_lazily_and_applies_integrity_migration() {
    let sqlite_home = test_sqlite_home();
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    assert!(!repository.path().exists());
    assert!(
        crate::runtime::runtime_db_paths(sqlite_home.as_path())
            .into_iter()
            .any(|db| db.path == repository.path())
    );

    repository.ensure_open().await.expect("open history db");
    assert!(repository.path().exists());

    let projection_columns = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('thread_history_projection_state') ORDER BY cid",
    )
    .fetch_all(repository.pool().await.expect("history pool"))
    .await
    .expect("projection columns");
    assert_eq!(
        projection_columns,
        vec![
            "thread_id",
            "next_rollout_byte_offset",
            "next_rollout_ordinal",
            "rollout_source_id",
            "rollout_fingerprint",
            "projection_generation",
            "projection_status",
        ]
    );
    let item_columns = sqlx::query_scalar::<_, String>(
        "SELECT name FROM pragma_table_info('thread_items') ORDER BY cid",
    )
    .fetch_all(repository.pool().await.expect("history pool"))
    .await
    .expect("item columns");
    assert_eq!(
        item_columns.last().map(String::as_str),
        Some("item_created_at_ms")
    );
    let integrity_objects = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE name IN ('idx_thread_history_projection_state_dirty', 'thread_turns_delete_items') ORDER BY name",
    )
    .fetch_all(repository.pool().await.expect("history pool"))
    .await
    .expect("integrity objects");
    assert_eq!(
        integrity_objects,
        vec![
            "idx_thread_history_projection_state_dirty",
            "thread_turns_delete_items",
        ]
    );

    let pool = repository.pool().await.expect("history pool");
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(9_999_i64)
    .bind("future migration")
    .bind(true)
    .bind(vec![1_u8, 2, 3, 4])
    .bind(1_i64)
    .execute(pool)
    .await
    .expect("insert future migration");
    pool.close().await;
    drop(repository);

    ThreadHistoryRepository::new(sqlite_home.clone())
        .ensure_open()
        .await
        .expect("runtime migrator should tolerate a newer applied migration");
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}

#[tokio::test]
async fn integrity_migration_upgrades_populated_v1_database() {
    let sqlite_home = test_sqlite_home();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("create sqlite home");
    let path = thread_history_db_path(sqlite_home.as_path());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path.as_path())
                .create_if_missing(true),
        )
        .await
        .expect("open v1 history db");
    migrator_through(/*version*/ 1)
        .run(&pool)
        .await
        .expect("apply v1 migration");
    sqlx::query(
        "INSERT INTO thread_turns (thread_id, turn_id, rollout_ordinal, status) VALUES (?, ?, ?, ?)",
    )
    .bind("thread-1")
    .bind("turn-1")
    .bind(1_i64)
    .bind("completed")
    .execute(&pool)
    .await
    .expect("insert v1 turn");
    sqlx::query(
        "INSERT INTO thread_items (thread_id, turn_id, item_id, rollout_ordinal, item_json) VALUES (?, ?, ?, ?, ?)",
    )
    .bind("thread-1")
    .bind("turn-1")
    .bind("item-1")
    .bind(2_i64)
    .bind("{}")
    .execute(&pool)
    .await
    .expect("insert v1 item");
    sqlx::query(
        "INSERT INTO thread_history_projection_state (thread_id, next_rollout_byte_offset, next_rollout_ordinal) VALUES (?, ?, ?)",
    )
    .bind("thread-1")
    .bind(300_i64)
    .bind(3_i64)
    .execute(&pool)
    .await
    .expect("insert v1 checkpoint");
    pool.close().await;

    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    repository.ensure_open().await.expect("upgrade history db");
    assert_eq!(
        repository.checkpoint("thread-1").await.expect("checkpoint"),
        Some(ThreadHistoryCheckpoint {
            thread_id: "thread-1".to_string(),
            next_rollout_byte_offset: 300,
            next_rollout_ordinal: 3,
            rollout_source_id: None,
            rollout_fingerprint: None,
            projection_generation: 0,
            projection_status: ThreadHistoryProjectionStatus::Dirty,
        })
    );
    let item_created_at_ms: i64 = sqlx::query_scalar(
        "SELECT item_created_at_ms FROM thread_items WHERE thread_id = ? AND item_id = ?",
    )
    .bind("thread-1")
    .bind("item-1")
    .fetch_one(repository.pool().await.expect("history pool"))
    .await
    .expect("upgraded item timestamp");
    assert_eq!(item_created_at_ms, 0);
    sqlx::query("DELETE FROM thread_turns WHERE thread_id = ? AND turn_id = ?")
        .bind("thread-1")
        .bind("turn-1")
        .execute(repository.pool().await.expect("history pool"))
        .await
        .expect("delete upgraded turn");
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM thread_items WHERE thread_id = ?")
            .bind("thread-1")
            .fetch_one(repository.pool().await.expect("history pool"))
            .await
            .expect("count cascaded items");
    assert_eq!(item_count, 0);
    drop(repository);
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}

#[tokio::test]
async fn lazy_open_retries_after_path_collision_is_removed() {
    let sqlite_home = test_sqlite_home();
    tokio::fs::create_dir_all(&sqlite_home)
        .await
        .expect("create sqlite home");
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    tokio::fs::create_dir(repository.path())
        .await
        .expect("create path collision");

    repository
        .ensure_open()
        .await
        .expect_err("directory at database path should fail open");
    tokio::fs::remove_dir(repository.path())
        .await
        .expect("remove path collision");
    repository
        .ensure_open()
        .await
        .expect("same repository should retry open");
    drop(repository);
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}

#[tokio::test]
async fn suffix_transaction_preserves_first_ordinals_and_rolls_back_atomically() {
    let sqlite_home = test_sqlite_home();
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-1",
                        /*rollout_ordinal*/ 1,
                        ThreadHistoryTurnStatus::InProgress,
                    ),
                    item(
                        "turn-1",
                        "item-1",
                        /*rollout_ordinal*/ 2,
                        /*item_created_at_ms*/ 200,
                        r#"{"version":1}"#,
                    ),
                ],
                /*next_rollout_ordinal*/ 3,
                "fingerprint-1",
            ),
        )
        .await
        .expect("apply first suffix");
    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-1",
                        /*rollout_ordinal*/ 3,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-1",
                        "item-1",
                        /*rollout_ordinal*/ 4,
                        /*item_created_at_ms*/ 400,
                        r#"{"version":2}"#,
                    ),
                ],
                /*next_rollout_ordinal*/ 5,
                "fingerprint-2",
            ),
        )
        .await
        .expect("apply update suffix");

    let turns = repository
        .turns_keyset(ThreadHistoryTurnsQuery {
            thread_id: "thread-1",
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 10,
        })
        .await
        .expect("query turns");
    assert_eq!(
        turns,
        vec![ThreadHistoryTurnRow {
            turn_id: "turn-1".to_string(),
            rollout_ordinal: 1,
            status: ThreadHistoryTurnStatus::Completed,
            error_json: None,
            started_at: Some(10),
            completed_at: None,
            duration_ms: None,
        }]
    );
    let items = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: None,
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 10,
        })
        .await
        .expect("query items");
    assert_eq!(
        items,
        vec![ThreadHistoryItemRow {
            turn_id: "turn-1".to_string(),
            item_key: "item-1".to_string(),
            item_ordinal: 2,
            item_created_at_ms: 200,
            materialized_thread_item_json: br#"{"version":2}"#.to_vec(),
        }]
    );
    assert_eq!(
        repository.checkpoint("thread-1").await.expect("checkpoint"),
        Some(ThreadHistoryCheckpoint {
            thread_id: "thread-1".to_string(),
            next_rollout_byte_offset: 500,
            next_rollout_ordinal: 5,
            rollout_source_id: Some("rollout-source-1".to_string()),
            rollout_fingerprint: Some("fingerprint-2".to_string()),
            projection_generation: 0,
            projection_status: ThreadHistoryProjectionStatus::Clean,
        })
    );

    let checkpoint = repository
        .checkpoint("thread-1")
        .await
        .expect("checkpoint before rollback");
    let error = repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-2",
                        /*rollout_ordinal*/ 5,
                        ThreadHistoryTurnStatus::InProgress,
                    ),
                    item(
                        "turn-2", "item-2", /*rollout_ordinal*/ 6,
                        /*item_created_at_ms*/ 600, "{}",
                    ),
                    item(
                        "missing-turn",
                        "item-3",
                        /*rollout_ordinal*/ 7,
                        /*item_created_at_ms*/ 700,
                        "{}",
                    ),
                ],
                /*next_rollout_ordinal*/ 8,
                "fingerprint-3",
            ),
        )
        .await
        .expect_err("invalid suffix should roll back");
    assert!(error.to_string().contains("does not exist"));
    assert_eq!(
        repository
            .turns_keyset(ThreadHistoryTurnsQuery {
                thread_id: "thread-1",
                cursor: None,
                direction: ThreadHistoryPageDirection::Ascending,
                limit: 10,
            })
            .await
            .expect("query turns after rollback"),
        turns
    );
    assert_eq!(
        repository
            .items_keyset(ThreadHistoryItemsQuery {
                thread_id: "thread-1",
                turn_id: None,
                cursor: None,
                direction: ThreadHistoryPageDirection::Ascending,
                limit: 10,
            })
            .await
            .expect("query items after rollback"),
        items
    );
    assert_eq!(
        repository
            .checkpoint("thread-1")
            .await
            .expect("checkpoint after rollback"),
        checkpoint
    );
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}

#[tokio::test]
async fn checkpoint_fencing_rejects_rewind_and_source_replacement() {
    let sqlite_home = test_sqlite_home();
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-1",
                        /*rollout_ordinal*/ 1,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-1", "item-1", /*rollout_ordinal*/ 2,
                        /*item_created_at_ms*/ 20, "{}",
                    ),
                ],
                /*next_rollout_ordinal*/ 3,
                "fingerprint-1",
            ),
        )
        .await
        .expect("apply first suffix");
    let destructive_suffix = suffix(
        vec![
            turn(
                "turn-2",
                /*rollout_ordinal*/ 3,
                ThreadHistoryTurnStatus::Completed,
            ),
            item(
                "turn-2", "item-2", /*rollout_ordinal*/ 4, /*item_created_at_ms*/ 40,
                "{}",
            ),
            ThreadHistoryMutation::RemoveLatestTurns {
                rollout_ordinal: 5,
                count: 1,
            },
        ],
        /*next_rollout_ordinal*/ 6,
        "fingerprint-2",
    );
    repository
        .apply_suffix("thread-1", &destructive_suffix)
        .await
        .expect("apply destructive suffix");
    repository
        .apply_suffix("thread-1", &destructive_suffix)
        .await
        .expect("exact replay should be idempotent");

    let turns = repository
        .turns_keyset(ThreadHistoryTurnsQuery {
            thread_id: "thread-1",
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 10,
        })
        .await
        .expect("turns after replay");
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1"]
    );
    let checkpoint = repository
        .checkpoint("thread-1")
        .await
        .expect("checkpoint after replay");

    let rewind = repository
        .apply_suffix(
            "thread-1",
            &suffix(Vec::new(), /*next_rollout_ordinal*/ 5, "rewind"),
        )
        .await
        .expect_err("checkpoint rewind should fail");
    assert!(rewind.to_string().contains("must not move backwards"));
    let changed_fingerprint = repository
        .apply_suffix(
            "thread-1",
            &suffix(Vec::new(), /*next_rollout_ordinal*/ 6, "changed"),
        )
        .await
        .expect_err("same checkpoint with new fingerprint should fail");
    assert!(
        changed_fingerprint
            .to_string()
            .contains("fingerprint changed")
    );
    let replacement = ThreadHistorySuffix {
        mutations: Vec::new(),
        checkpoint: ThreadHistoryCheckpointUpdate {
            next_rollout_byte_offset: 700,
            next_rollout_ordinal: 7,
            source: source_with_id("rollout-source-2", "replacement"),
        },
    };
    let source_error = repository
        .apply_suffix("thread-1", &replacement)
        .await
        .expect_err("source replacement should require invalidation");
    assert!(source_error.to_string().contains("rollout source changed"));
    assert_eq!(
        repository
            .checkpoint("thread-1")
            .await
            .expect("checkpoint after rejected suffixes"),
        checkpoint
    );
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}

#[tokio::test]
async fn keyset_cleanup_dirty_state_and_integer_boundaries_are_safe() {
    let sqlite_home = test_sqlite_home();
    let repository = ThreadHistoryRepository::new(sqlite_home.clone());
    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![
                    turn(
                        "turn-1",
                        /*rollout_ordinal*/ 1,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-1", "item-1", /*rollout_ordinal*/ 2,
                        /*item_created_at_ms*/ 20, "{}",
                    ),
                    turn(
                        "turn-2",
                        /*rollout_ordinal*/ 11,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-2", "item-2", /*rollout_ordinal*/ 12,
                        /*item_created_at_ms*/ 120, "{}",
                    ),
                    turn(
                        "turn-3",
                        /*rollout_ordinal*/ 21,
                        ThreadHistoryTurnStatus::InProgress,
                    ),
                    item(
                        "turn-3", "item-3", /*rollout_ordinal*/ 22,
                        /*item_created_at_ms*/ 220, "{}",
                    ),
                ],
                /*next_rollout_ordinal*/ 30,
                "fingerprint-1",
            ),
        )
        .await
        .expect("apply gapped suffix");
    let checkpoint = repository
        .checkpoint("thread-1")
        .await
        .expect("checkpoint")
        .expect("checkpoint row");
    assert_eq!(checkpoint.projection_generation, 0);
    assert_eq!(
        checkpoint.projection_status,
        ThreadHistoryProjectionStatus::Clean
    );
    assert_eq!(
        repository.bump_generation("thread-1").await.expect("bump"),
        1
    );
    let invalidated = repository
        .checkpoint("thread-1")
        .await
        .expect("invalidated checkpoint")
        .expect("invalidated checkpoint row");
    assert_eq!(invalidated.projection_generation, 1);
    assert_eq!(
        invalidated.projection_status,
        ThreadHistoryProjectionStatus::Dirty
    );

    let forward = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: None,
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 2,
        })
        .await
        .expect("forward items");
    assert_eq!(
        forward
            .iter()
            .map(|item| item.item_ordinal)
            .collect::<Vec<_>>(),
        vec![2, 12, 22]
    );
    let backward = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: None,
            cursor: Some(22),
            direction: ThreadHistoryPageDirection::Descending,
            limit: 1,
        })
        .await
        .expect("backward items");
    assert_eq!(
        backward
            .iter()
            .map(|item| item.item_ordinal)
            .collect::<Vec<_>>(),
        vec![12, 2]
    );
    let filtered = repository
        .items_keyset(ThreadHistoryItemsQuery {
            thread_id: "thread-1",
            turn_id: Some("turn-2"),
            cursor: None,
            direction: ThreadHistoryPageDirection::Ascending,
            limit: 10,
        })
        .await
        .expect("filtered items");
    assert_eq!(
        filtered
            .iter()
            .map(|item| item.item_key.as_str())
            .collect::<Vec<_>>(),
        vec!["item-2"]
    );

    repository
        .apply_suffix(
            "thread-1",
            &suffix(
                vec![ThreadHistoryMutation::RemoveLatestTurns {
                    rollout_ordinal: 30,
                    count: 2,
                }],
                /*next_rollout_ordinal*/ 31,
                "fingerprint-2",
            ),
        )
        .await
        .expect("remove latest turns");
    assert_eq!(
        repository
            .items_keyset(ThreadHistoryItemsQuery {
                thread_id: "thread-1",
                turn_id: None,
                cursor: None,
                direction: ThreadHistoryPageDirection::Ascending,
                limit: 10,
            })
            .await
            .expect("remaining items")
            .into_iter()
            .map(|item| item.item_key)
            .collect::<Vec<_>>(),
        vec!["item-1"]
    );

    repository
        .apply_suffix(
            "thread-2",
            &suffix(
                vec![
                    turn(
                        "turn-a",
                        /*rollout_ordinal*/ 1,
                        ThreadHistoryTurnStatus::Completed,
                    ),
                    item(
                        "turn-a",
                        "item-a",
                        /*rollout_ordinal*/ 2,
                        /*item_created_at_ms*/ 20,
                        r#"{"thread":2}"#,
                    ),
                ],
                /*next_rollout_ordinal*/ 3,
                "thread-2-fingerprint",
            ),
        )
        .await
        .expect("seed second thread");
    let second_thread_checkpoint = repository
        .checkpoint("thread-2")
        .await
        .expect("second thread checkpoint");

    repository
        .delete_thread("thread-1")
        .await
        .expect("delete thread");
    repository
        .delete_thread("thread-1")
        .await
        .expect("repeat delete");
    assert_eq!(
        repository.checkpoint("thread-1").await.expect("checkpoint"),
        None
    );
    assert!(
        repository
            .turns_keyset(ThreadHistoryTurnsQuery {
                thread_id: "thread-1",
                cursor: None,
                direction: ThreadHistoryPageDirection::Ascending,
                limit: 10,
            })
            .await
            .expect("deleted turns")
            .is_empty()
    );
    assert!(
        repository
            .items_keyset(ThreadHistoryItemsQuery {
                thread_id: "thread-1",
                turn_id: None,
                cursor: None,
                direction: ThreadHistoryPageDirection::Ascending,
                limit: 10,
            })
            .await
            .expect("deleted items")
            .is_empty()
    );
    assert_eq!(
        repository
            .checkpoint("thread-2")
            .await
            .expect("second thread after delete"),
        second_thread_checkpoint
    );
    let overflow = repository
        .apply_suffix(
            "thread-1",
            &ThreadHistorySuffix {
                mutations: Vec::new(),
                checkpoint: ThreadHistoryCheckpointUpdate {
                    next_rollout_byte_offset: u64::MAX,
                    next_rollout_ordinal: 0,
                    source: source("overflow"),
                },
            },
        )
        .await
        .expect_err("oversized offset must fail before storage");
    assert!(overflow.to_string().contains("signed integer range"));
    assert_eq!(
        repository.checkpoint("thread-1").await.expect("checkpoint"),
        None
    );
    let _ = tokio::fs::remove_dir_all(sqlite_home).await;
}
