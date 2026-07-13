use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;
use sqlx::QueryBuilder;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqliteConnection;
use sqlx::SqlitePool;
use tokio::sync::OnceCell;

use crate::THREAD_HISTORY_DB_FILENAME;
use crate::migrations::runtime_thread_history_migrator;
use crate::runtime::open_thread_history_sqlite;

#[allow(dead_code)]
const MAX_KEYSET_PAGE_SIZE: usize = 1_000;

/// Cleanliness state for one projected thread history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadHistoryProjectionStatus {
    Clean,
    Dirty,
}

impl ThreadHistoryProjectionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
        }
    }

    #[allow(dead_code)]
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "clean" => Ok(Self::Clean),
            "dirty" => Ok(Self::Dirty),
            value => bail!("invalid thread history projection status {value:?}"),
        }
    }
}

/// Durable source identity recorded with a projection checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadHistorySource {
    pub rollout_source_id: String,
    pub rollout_fingerprint: String,
}

/// Checkpoint values committed atomically with a projected suffix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadHistoryCheckpointUpdate {
    pub next_rollout_byte_offset: u64,
    pub next_rollout_ordinal: u64,
    pub source: ThreadHistorySource,
}

/// Persisted projection checkpoint for one thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadHistoryCheckpoint {
    pub thread_id: String,
    pub next_rollout_byte_offset: u64,
    pub next_rollout_ordinal: u64,
    pub rollout_source_id: Option<String>,
    pub rollout_fingerprint: Option<String>,
    pub projection_generation: u64,
    pub projection_status: ThreadHistoryProjectionStatus,
}

/// Store-owned status for a projected turn row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadHistoryTurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

impl ThreadHistoryTurnStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::InProgress => "inProgress",
        }
    }

    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "completed" => Ok(Self::Completed),
            "interrupted" => Ok(Self::Interrupted),
            "failed" => Ok(Self::Failed),
            "inProgress" => Ok(Self::InProgress),
            value => bail!("invalid thread history turn status {value:?}"),
        }
    }
}

/// Turn selected by a projected mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadHistoryTurnTarget {
    Id(String),
    Active,
}

/// Insert or update one projected turn while preserving its first ordinal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadHistoryTurnUpsert {
    pub target: ThreadHistoryTurnTarget,
    pub rollout_ordinal: i64,
    pub status: ThreadHistoryTurnStatus,
    pub error_json: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

/// Insert or update one materialized item while preserving its first ordinal and timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadHistoryItemUpsert {
    pub target: ThreadHistoryTurnTarget,
    pub item_id: String,
    pub rollout_ordinal: i64,
    pub item_created_at_ms: i64,
    pub item_json: String,
}

/// Ordered storage mutation produced from one paginated rollout line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadHistoryMutation {
    UpsertTurn(ThreadHistoryTurnUpsert),
    UpsertItem(ThreadHistoryItemUpsert),
    RemoveLatestTurns { rollout_ordinal: i64, count: u32 },
}

impl ThreadHistoryMutation {
    fn rollout_ordinal(&self) -> i64 {
        match self {
            Self::UpsertTurn(turn) => turn.rollout_ordinal,
            Self::UpsertItem(item) => item.rollout_ordinal,
            Self::RemoveLatestTurns {
                rollout_ordinal, ..
            } => *rollout_ordinal,
        }
    }
}

/// An ordered rollout suffix and the checkpoint reached after applying it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadHistorySuffix {
    pub mutations: Vec<ThreadHistoryMutation>,
    pub checkpoint: ThreadHistoryCheckpointUpdate,
}

/// Lazy owner for the dedicated paginated thread-history database.
pub struct ThreadHistoryRepository {
    path: PathBuf,
    pool: OnceCell<SqlitePool>,
}

impl ThreadHistoryRepository {
    pub fn new(sqlite_home: PathBuf) -> Self {
        Self {
            path: thread_history_db_path(sqlite_home.as_path()),
            pool: OnceCell::new(),
        }
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub async fn ensure_open(&self) -> anyhow::Result<()> {
        self.pool().await.map(|_| ())
    }

    /// Apply an ordered suffix and advance its checkpoint in one transaction.
    pub async fn apply_suffix(
        &self,
        thread_id: &str,
        expected_generation: u64,
        suffix: &ThreadHistorySuffix,
    ) -> anyhow::Result<()> {
        ensure_nonempty(thread_id, "thread_id")?;
        ensure_nonempty(
            suffix.checkpoint.source.rollout_source_id.as_str(),
            "rollout_source_id",
        )?;
        ensure_nonempty(
            suffix.checkpoint.source.rollout_fingerprint.as_str(),
            "rollout_fingerprint",
        )?;
        let next_byte_offset = sqlite_integer(
            suffix.checkpoint.next_rollout_byte_offset,
            "next_rollout_byte_offset",
        )?;
        let next_ordinal = sqlite_integer(
            suffix.checkpoint.next_rollout_ordinal,
            "next_rollout_ordinal",
        )?;
        ensure_valid_suffix(suffix, next_ordinal)?;

        let pool = self.pool().await?;
        let mut transaction = pool.begin().await?;
        let mut should_apply_mutations = true;
        let existing_checkpoint = sqlx::query(
            r#"
SELECT next_rollout_byte_offset,
       next_rollout_ordinal,
       rollout_source_id,
       rollout_fingerprint,
       projection_generation,
       projection_status
FROM thread_history_projection_state
WHERE thread_id = ?
            "#,
        )
        .bind(thread_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(existing_checkpoint) = existing_checkpoint {
            let existing_byte_offset: i64 =
                existing_checkpoint.try_get("next_rollout_byte_offset")?;
            let existing_ordinal: i64 = existing_checkpoint.try_get("next_rollout_ordinal")?;
            let existing_source_id: Option<String> =
                existing_checkpoint.try_get("rollout_source_id")?;
            let existing_fingerprint: Option<String> =
                existing_checkpoint.try_get("rollout_fingerprint")?;
            let existing_generation: i64 = existing_checkpoint.try_get("projection_generation")?;
            let existing_status: String = existing_checkpoint.try_get("projection_status")?;
            if sqlite_unsigned(existing_generation, "projection_generation")? != expected_generation
            {
                bail!("stale thread history projection generation");
            }
            if existing_byte_offset > 0
                && existing_source_id.is_none()
                && existing_status == ThreadHistoryProjectionStatus::Dirty.as_str()
            {
                bail!("thread history projection replacement is in progress");
            }
            if existing_source_id.as_deref().is_some_and(|source_id| {
                source_id != suffix.checkpoint.source.rollout_source_id.as_str()
            }) {
                bail!("thread history rollout source changed without invalidating the projection");
            }
            if next_byte_offset < existing_byte_offset || next_ordinal < existing_ordinal {
                bail!("thread history checkpoint must not move backwards");
            }
            if next_byte_offset == existing_byte_offset && next_ordinal == existing_ordinal {
                if existing_fingerprint.as_deref()
                    == Some(suffix.checkpoint.source.rollout_fingerprint.as_str())
                {
                    should_apply_mutations = false;
                } else {
                    bail!("thread history checkpoint fingerprint changed without advancing");
                }
            }
        } else if expected_generation != 0 {
            bail!("stale thread history projection generation");
        }
        if should_apply_mutations {
            apply_mutations(&mut transaction, thread_id, suffix).await?;
        }
        let result = sqlx::query(
            r#"
INSERT INTO thread_history_projection_state (
    thread_id,
    next_rollout_byte_offset,
    next_rollout_ordinal,
    rollout_source_id,
    rollout_fingerprint,
    projection_generation,
    projection_status
) VALUES (?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(thread_id) DO UPDATE SET
    next_rollout_byte_offset = excluded.next_rollout_byte_offset,
    next_rollout_ordinal = excluded.next_rollout_ordinal,
    rollout_source_id = excluded.rollout_source_id,
    rollout_fingerprint = excluded.rollout_fingerprint,
    projection_status = excluded.projection_status
WHERE thread_history_projection_state.projection_generation = excluded.projection_generation
            "#,
        )
        .bind(thread_id)
        .bind(next_byte_offset)
        .bind(next_ordinal)
        .bind(suffix.checkpoint.source.rollout_source_id.as_str())
        .bind(suffix.checkpoint.source.rollout_fingerprint.as_str())
        .bind(sqlite_integer(
            expected_generation,
            "projection_generation",
        )?)
        .bind(ThreadHistoryProjectionStatus::Clean.as_str())
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            bail!("stale thread history projection generation");
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn replace_projection(
        &self,
        thread_id: &str,
        expected_generation: u64,
        suffix: &ThreadHistorySuffix,
    ) -> anyhow::Result<()> {
        ensure_nonempty(thread_id, "thread_id")?;
        ensure_nonempty(
            suffix.checkpoint.source.rollout_source_id.as_str(),
            "rollout_source_id",
        )?;
        ensure_nonempty(
            suffix.checkpoint.source.rollout_fingerprint.as_str(),
            "rollout_fingerprint",
        )?;
        let next_byte_offset = sqlite_integer(
            suffix.checkpoint.next_rollout_byte_offset,
            "next_rollout_byte_offset",
        )?;
        let next_ordinal = sqlite_integer(
            suffix.checkpoint.next_rollout_ordinal,
            "next_rollout_ordinal",
        )?;
        ensure_valid_suffix(suffix, next_ordinal)?;
        let generation = sqlite_integer(expected_generation, "projection_generation")?;

        let pool = self.pool().await?;
        let mut transaction = pool.begin().await?;
        let current_generation: Option<i64> = sqlx::query_scalar(
            "SELECT projection_generation FROM thread_history_projection_state WHERE thread_id = ?",
        )
        .bind(thread_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if current_generation != Some(generation) {
            bail!("stale thread history projection generation");
        }
        sqlx::query("DELETE FROM thread_turns WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&mut *transaction)
            .await?;
        apply_mutations(&mut transaction, thread_id, suffix).await?;
        let result = sqlx::query(
            r#"
UPDATE thread_history_projection_state
SET next_rollout_byte_offset = ?,
    next_rollout_ordinal = ?,
    rollout_source_id = ?,
    rollout_fingerprint = ?,
    projection_status = ?
WHERE thread_id = ? AND projection_generation = ?
            "#,
        )
        .bind(next_byte_offset)
        .bind(next_ordinal)
        .bind(suffix.checkpoint.source.rollout_source_id.as_str())
        .bind(suffix.checkpoint.source.rollout_fingerprint.as_str())
        .bind(ThreadHistoryProjectionStatus::Clean.as_str())
        .bind(thread_id)
        .bind(generation)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            bail!("stale thread history projection generation");
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn mark_dirty(&self, thread_id: &str) -> anyhow::Result<()> {
        ensure_nonempty(thread_id, "thread_id")?;
        sqlx::query(
            r#"
INSERT INTO thread_history_projection_state (
    thread_id, next_rollout_byte_offset, next_rollout_ordinal, projection_status
) VALUES (?, 0, 0, ?)
ON CONFLICT(thread_id) DO UPDATE SET projection_status = excluded.projection_status
            "#,
        )
        .bind(thread_id)
        .bind(ThreadHistoryProjectionStatus::Dirty.as_str())
        .execute(self.pool().await?)
        .await?;
        Ok(())
    }

    pub async fn bump_generation(&self, thread_id: &str) -> anyhow::Result<u64> {
        ensure_nonempty(thread_id, "thread_id")?;
        let pool = self.pool().await?;
        let mut transaction = pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO thread_history_projection_state (
    thread_id, next_rollout_byte_offset, next_rollout_ordinal, projection_status
) VALUES (?, 0, 0, ?)
ON CONFLICT(thread_id) DO NOTHING
            "#,
        )
        .bind(thread_id)
        .bind(ThreadHistoryProjectionStatus::Dirty.as_str())
        .execute(&mut *transaction)
        .await?;
        let generation: Option<i64> = sqlx::query_scalar(
            r#"
UPDATE thread_history_projection_state
SET projection_generation = projection_generation + 1,
    projection_status = ?
WHERE thread_id = ? AND projection_generation < ?
RETURNING projection_generation
            "#,
        )
        .bind(ThreadHistoryProjectionStatus::Dirty.as_str())
        .bind(thread_id)
        .bind(i64::MAX)
        .fetch_optional(&mut *transaction)
        .await?;
        let generation = generation.context("thread history projection generation overflow")?;
        transaction.commit().await?;
        sqlite_unsigned(generation, "projection_generation")
    }

    pub async fn begin_replacement(&self, thread_id: &str) -> anyhow::Result<u64> {
        ensure_nonempty(thread_id, "thread_id")?;
        let pool = self.pool().await?;
        let mut transaction = pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO thread_history_projection_state (
    thread_id, next_rollout_byte_offset, next_rollout_ordinal, projection_status
) VALUES (?, 0, 0, ?)
ON CONFLICT(thread_id) DO NOTHING
            "#,
        )
        .bind(thread_id)
        .bind(ThreadHistoryProjectionStatus::Dirty.as_str())
        .execute(&mut *transaction)
        .await?;
        let generation: Option<i64> = sqlx::query_scalar(
            r#"
UPDATE thread_history_projection_state
SET projection_generation = projection_generation + 1,
    rollout_source_id = NULL,
    rollout_fingerprint = NULL,
    projection_status = ?
WHERE thread_id = ? AND projection_generation < ?
RETURNING projection_generation
            "#,
        )
        .bind(ThreadHistoryProjectionStatus::Dirty.as_str())
        .bind(thread_id)
        .bind(i64::MAX)
        .fetch_optional(&mut *transaction)
        .await?;
        let generation = generation.context("thread history projection generation overflow")?;
        transaction.commit().await?;
        sqlite_unsigned(generation, "projection_generation")
    }

    pub async fn checkpoint(
        &self,
        thread_id: &str,
    ) -> anyhow::Result<Option<ThreadHistoryCheckpoint>> {
        ensure_nonempty(thread_id, "thread_id")?;
        let row = sqlx::query(
            r#"
SELECT thread_id,
       next_rollout_byte_offset,
       next_rollout_ordinal,
       rollout_source_id,
       rollout_fingerprint,
       projection_generation,
       projection_status
FROM thread_history_projection_state
WHERE thread_id = ?
            "#,
        )
        .bind(thread_id)
        .fetch_optional(self.pool().await?)
        .await?;
        row.map(|row| {
            let next_byte_offset: i64 = row.try_get("next_rollout_byte_offset")?;
            let next_ordinal: i64 = row.try_get("next_rollout_ordinal")?;
            let generation: i64 = row.try_get("projection_generation")?;
            let status: String = row.try_get("projection_status")?;
            Ok(ThreadHistoryCheckpoint {
                thread_id: row.try_get("thread_id")?,
                next_rollout_byte_offset: sqlite_unsigned(
                    next_byte_offset,
                    "next_rollout_byte_offset",
                )?,
                next_rollout_ordinal: sqlite_unsigned(next_ordinal, "next_rollout_ordinal")?,
                rollout_source_id: row.try_get("rollout_source_id")?,
                rollout_fingerprint: row.try_get("rollout_fingerprint")?,
                projection_generation: sqlite_unsigned(generation, "projection_generation")?,
                projection_status: ThreadHistoryProjectionStatus::parse(status.as_str())?,
            })
        })
        .transpose()
    }

    pub async fn delete_thread(&self, thread_id: &str) -> anyhow::Result<()> {
        ensure_nonempty(thread_id, "thread_id")?;
        let pool = self.pool().await?;
        let mut transaction = pool.begin().await?;
        sqlx::query("DELETE FROM thread_turns WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM thread_items WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM thread_history_projection_state WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) async fn items_keyset(
        &self,
        query: ThreadHistoryItemsQuery<'_>,
    ) -> anyhow::Result<Vec<ThreadHistoryItemRow>> {
        ensure_nonempty(query.thread_id, "thread_id")?;
        let limit = keyset_limit(query.limit)?;
        let cursor = query
            .cursor
            .map(|cursor| sqlite_integer(cursor, "item cursor"))
            .transpose()?;
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT turn_id, item_id, rollout_ordinal, item_created_at_ms, item_json FROM thread_items WHERE thread_id = ",
        );
        builder.push_bind(query.thread_id);
        if let Some(turn_id) = query.turn_id {
            builder.push(" AND turn_id = ").push_bind(turn_id);
        }
        push_keyset_cursor(&mut builder, cursor, query.direction);
        builder.push(" ORDER BY rollout_ordinal ");
        builder.push(query.direction.sql_order());
        builder.push(" LIMIT ").push_bind(limit);
        builder
            .build()
            .fetch_all(self.pool().await?)
            .await?
            .into_iter()
            .map(|row| {
                let item_json: String = row.try_get("item_json")?;
                let item_ordinal: i64 = row.try_get("rollout_ordinal")?;
                Ok(ThreadHistoryItemRow {
                    turn_id: row.try_get("turn_id")?,
                    item_key: row.try_get("item_id")?,
                    item_ordinal: sqlite_unsigned(item_ordinal, "item rollout_ordinal")?,
                    item_created_at_ms: row.try_get("item_created_at_ms")?,
                    materialized_thread_item_json: item_json.into_bytes(),
                })
            })
            .collect()
    }

    #[allow(dead_code)]
    pub(crate) async fn turns_keyset(
        &self,
        query: ThreadHistoryTurnsQuery<'_>,
    ) -> anyhow::Result<Vec<ThreadHistoryTurnRow>> {
        ensure_nonempty(query.thread_id, "thread_id")?;
        let limit = keyset_limit(query.limit)?;
        let cursor = query
            .cursor
            .map(|cursor| sqlite_integer(cursor, "turn cursor"))
            .transpose()?;
        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT turn_id, rollout_ordinal, status, error_json, started_at, completed_at, duration_ms FROM thread_turns WHERE thread_id = ",
        );
        builder.push_bind(query.thread_id);
        push_keyset_cursor(&mut builder, cursor, query.direction);
        builder.push(" ORDER BY rollout_ordinal ");
        builder.push(query.direction.sql_order());
        builder.push(" LIMIT ").push_bind(limit);
        builder
            .build()
            .fetch_all(self.pool().await?)
            .await?
            .into_iter()
            .map(|row| {
                let status: String = row.try_get("status")?;
                let rollout_ordinal: i64 = row.try_get("rollout_ordinal")?;
                Ok(ThreadHistoryTurnRow {
                    turn_id: row.try_get("turn_id")?,
                    rollout_ordinal: sqlite_unsigned(rollout_ordinal, "turn rollout_ordinal")?,
                    status: ThreadHistoryTurnStatus::parse(status.as_str())?,
                    error_json: row.try_get("error_json")?,
                    started_at: row.try_get("started_at")?,
                    completed_at: row.try_get("completed_at")?,
                    duration_ms: row.try_get("duration_ms")?,
                })
            })
            .collect()
    }

    async fn pool(&self) -> anyhow::Result<&SqlitePool> {
        self.pool
            .get_or_try_init(|| async {
                let parent = self
                    .path
                    .parent()
                    .context("thread history database path has no parent")?;
                tokio::fs::create_dir_all(parent).await?;
                open_thread_history_sqlite(self.path.as_path(), &runtime_thread_history_migrator())
                    .await
            })
            .await
    }
}

fn ensure_valid_suffix(suffix: &ThreadHistorySuffix, next_ordinal: i64) -> anyhow::Result<()> {
    let mut previous_ordinal = None;
    for mutation in &suffix.mutations {
        let ordinal = mutation.rollout_ordinal();
        if ordinal < 0 {
            bail!("thread history mutation ordinal must be non-negative: {ordinal}");
        }
        if previous_ordinal.is_some_and(|previous| ordinal <= previous) {
            bail!("thread history suffix mutations must have strictly increasing ordinals");
        }
        if ordinal >= next_ordinal {
            bail!(
                "thread history mutation ordinal {ordinal} must precede checkpoint {next_ordinal}"
            );
        }
        previous_ordinal = Some(ordinal);
    }
    Ok(())
}

async fn apply_mutations(
    connection: &mut SqliteConnection,
    thread_id: &str,
    suffix: &ThreadHistorySuffix,
) -> anyhow::Result<()> {
    for mutation in &suffix.mutations {
        match mutation {
            ThreadHistoryMutation::UpsertTurn(turn) => {
                upsert_turn(connection, thread_id, turn).await?;
            }
            ThreadHistoryMutation::UpsertItem(item) => {
                upsert_item(connection, thread_id, item).await?;
            }
            ThreadHistoryMutation::RemoveLatestTurns { count, .. } => {
                if *count > 0 {
                    sqlx::query(
                        "DELETE FROM thread_turns WHERE rowid IN (SELECT rowid FROM thread_turns WHERE thread_id = ? ORDER BY rollout_ordinal DESC LIMIT ?)",
                    )
                    .bind(thread_id)
                    .bind(i64::from(*count))
                    .execute(&mut *connection)
                    .await?;
                }
            }
        }
    }
    Ok(())
}

async fn upsert_turn(
    connection: &mut SqliteConnection,
    thread_id: &str,
    turn: &ThreadHistoryTurnUpsert,
) -> anyhow::Result<()> {
    let turn_id = match &turn.target {
        ThreadHistoryTurnTarget::Id(turn_id) => turn_id.clone(),
        ThreadHistoryTurnTarget::Active => active_turn_id(connection, thread_id).await?,
    };
    ensure_nonempty(turn_id.as_str(), "turn_id")?;
    sqlx::query(
        r#"
INSERT INTO thread_turns (
    thread_id, turn_id, rollout_ordinal, status, error_json, started_at, completed_at, duration_ms
) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(thread_id, turn_id) DO UPDATE SET
    status = excluded.status,
    error_json = excluded.error_json,
    started_at = COALESCE(thread_turns.started_at, excluded.started_at),
    completed_at = excluded.completed_at,
    duration_ms = excluded.duration_ms
        "#,
    )
    .bind(thread_id)
    .bind(turn_id)
    .bind(turn.rollout_ordinal)
    .bind(turn.status.as_str())
    .bind(turn.error_json.as_deref())
    .bind(turn.started_at)
    .bind(turn.completed_at)
    .bind(turn.duration_ms)
    .execute(connection)
    .await?;
    Ok(())
}

async fn upsert_item(
    connection: &mut SqliteConnection,
    thread_id: &str,
    item: &ThreadHistoryItemUpsert,
) -> anyhow::Result<()> {
    ensure_nonempty(item.item_id.as_str(), "item_id")?;
    let turn_id = match &item.target {
        ThreadHistoryTurnTarget::Id(turn_id) => turn_id.clone(),
        ThreadHistoryTurnTarget::Active => active_turn_id(connection, thread_id).await?,
    };
    let turn_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM thread_turns WHERE thread_id = ? AND turn_id = ?)",
    )
    .bind(thread_id)
    .bind(turn_id.as_str())
    .fetch_one(&mut *connection)
    .await?;
    if !turn_exists {
        bail!("thread history item target {turn_id:?} does not exist");
    }
    sqlx::query(
        r#"
INSERT INTO thread_items (
    thread_id, turn_id, item_id, rollout_ordinal, item_json, item_created_at_ms
) VALUES (?, ?, ?, ?, ?, ?)
ON CONFLICT(thread_id, turn_id, item_id) DO UPDATE SET
    item_json = excluded.item_json
        "#,
    )
    .bind(thread_id)
    .bind(turn_id)
    .bind(item.item_id.as_str())
    .bind(item.rollout_ordinal)
    .bind(item.item_json.as_str())
    .bind(item.item_created_at_ms)
    .execute(connection)
    .await?;
    Ok(())
}

async fn active_turn_id(
    connection: &mut SqliteConnection,
    thread_id: &str,
) -> anyhow::Result<String> {
    sqlx::query_scalar(
        "SELECT turn_id FROM thread_turns WHERE thread_id = ? AND status = ? ORDER BY rollout_ordinal DESC LIMIT 1",
    )
    .bind(thread_id)
    .bind(ThreadHistoryTurnStatus::InProgress.as_str())
    .fetch_optional(connection)
    .await?
    .with_context(|| format!("thread {thread_id} has no active projected turn"))
}

fn ensure_nonempty(value: &str, field: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

fn sqlite_integer(value: u64, field: &str) -> anyhow::Result<i64> {
    i64::try_from(value).with_context(|| format!("{field} exceeds SQLite's signed integer range"))
}

fn sqlite_unsigned(value: i64, field: &str) -> anyhow::Result<u64> {
    u64::try_from(value).with_context(|| format!("{field} is negative in thread history storage"))
}

pub fn thread_history_db_filename() -> String {
    THREAD_HISTORY_DB_FILENAME.to_string()
}

pub fn thread_history_db_path(sqlite_home: &Path) -> PathBuf {
    sqlite_home.join(THREAD_HISTORY_DB_FILENAME)
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub(crate) enum ThreadHistoryPageDirection {
    Ascending,
    Descending,
}

impl ThreadHistoryPageDirection {
    fn sql_order(self) -> &'static str {
        match self {
            Self::Ascending => "ASC",
            Self::Descending => "DESC",
        }
    }
}

#[allow(dead_code)]
pub(crate) struct ThreadHistoryItemsQuery<'a> {
    pub thread_id: &'a str,
    pub turn_id: Option<&'a str>,
    pub cursor: Option<u64>,
    pub direction: ThreadHistoryPageDirection,
    pub limit: usize,
}

#[allow(dead_code)]
pub(crate) struct ThreadHistoryTurnsQuery<'a> {
    pub thread_id: &'a str,
    pub cursor: Option<u64>,
    pub direction: ThreadHistoryPageDirection,
    pub limit: usize,
}

#[allow(dead_code)]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ThreadHistoryItemRow {
    pub turn_id: String,
    pub item_key: String,
    pub item_ordinal: u64,
    pub item_created_at_ms: i64,
    pub materialized_thread_item_json: Vec<u8>,
}

#[allow(dead_code)]
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ThreadHistoryTurnRow {
    pub turn_id: String,
    pub rollout_ordinal: u64,
    pub status: ThreadHistoryTurnStatus,
    pub error_json: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

#[allow(dead_code)]
fn keyset_limit(limit: usize) -> anyhow::Result<i64> {
    if limit == 0 || limit > MAX_KEYSET_PAGE_SIZE {
        bail!("thread history page size must be between 1 and {MAX_KEYSET_PAGE_SIZE}");
    }
    let fetch_limit = limit
        .checked_add(1)
        .context("thread history page size overflow")?;
    i64::try_from(fetch_limit).context("thread history page size exceeds SQLite integer range")
}

#[allow(dead_code)]
fn push_keyset_cursor(
    builder: &mut QueryBuilder<Sqlite>,
    cursor: Option<i64>,
    direction: ThreadHistoryPageDirection,
) {
    if let Some(cursor) = cursor {
        match direction {
            ThreadHistoryPageDirection::Ascending => builder.push(" AND rollout_ordinal > "),
            ThreadHistoryPageDirection::Descending => builder.push(" AND rollout_ordinal < "),
        };
        builder.push_bind(cursor);
    }
}

#[cfg(test)]
#[path = "thread_history_tests.rs"]
mod tests;
