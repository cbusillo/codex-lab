use std::borrow::Cow;

use sqlx::SqlStr;
use sqlx::SqlitePool;
use sqlx::migrate::Migration;
use sqlx::migrate::Migrator;

/// Versions 36-38 of the state ledger are frozen: released Codex Lab builds
/// recorded `threads session provenance`, `threads history mode`, and `threads
/// visible sort indexes` under those versions, so their SQL must stay
/// byte-for-byte identical or every upgrade fails with `VersionMismatch`.
/// Migrations imported from upstream snapshots are renumbered onto 39+ instead
/// of taking the upstream version numbers.
///
/// Databases written by a binary that used the raw upstream numbering for 36-44
/// are deliberately not repaired: no such build was released, and remapping
/// them would require rewriting recorded checksums to values the ledger never
/// produced.
///
/// Versions 39 and above are unshipped and may still be rewritten in place.
/// They must not destroy user data: version 43 originally dropped the agent-job
/// tables even though the agent-jobs handlers are still `pending_restore`, and
/// it is now an inert placeholder that holds the slot without deleting rows.
pub(crate) static STATE_MIGRATOR: Migrator = sqlx::migrate!("./migrations");
pub(crate) static LOGS_MIGRATOR: Migrator = sqlx::migrate!("./logs_migrations");
pub(crate) static GOALS_MIGRATOR: Migrator = sqlx::migrate!("./goals_migrations");
pub(crate) static MEMORIES_MIGRATOR: Migrator = sqlx::migrate!("./memory_migrations");
pub(crate) static QUEUE_MIGRATOR: Migrator = sqlx::migrate!("./queue_migrations");
pub(crate) static THREAD_HISTORY_MIGRATOR: Migrator = sqlx::migrate!("./thread_history_migrations");

/// Allow an older Codex binary to open a database that has already been
/// migrated by a newer binary running in parallel.
///
/// We intentionally ignore applied migration versions that are newer than the
/// embedded migration set. Known migration versions are still validated by
/// checksum, so this only relaxes the "database is ahead of me" case.
fn runtime_migrator(base: &'static Migrator) -> Migrator {
    Migrator {
        migrations: Cow::Borrowed(base.migrations.as_ref()),
        ignore_missing: true,
        locking: base.locking,
        no_tx: base.no_tx,
        table_name: base.table_name.clone(),
        create_schemas: base.create_schemas.clone(),
    }
}

pub(crate) fn runtime_state_migrator() -> Migrator {
    runtime_migrator(&STATE_MIGRATOR)
}

pub(crate) fn runtime_logs_migrator() -> Migrator {
    runtime_migrator(&LOGS_MIGRATOR)
}

pub(crate) fn runtime_goals_migrator() -> Migrator {
    runtime_migrator(&GOALS_MIGRATOR)
}

pub(crate) fn runtime_memories_migrator() -> Migrator {
    runtime_migrator(&MEMORIES_MIGRATOR)
}

pub(crate) fn runtime_queue_migrator() -> Migrator {
    runtime_migrator(&QUEUE_MIGRATOR)
}

// The paginated history projector will call this when it takes ownership of opening the database.
#[allow(dead_code)]
pub(crate) fn runtime_thread_history_migrator() -> Migrator {
    runtime_migrator(&THREAD_HISTORY_MIGRATOR)
}

/// Description of the recency migration, used to locate it without pinning a
/// version number that shifts whenever the ledger is renumbered.
const RECENCY_MIGRATION_DESCRIPTION: &str = "threads recency at";

/// Versions the recency migration has been recorded under by binaries that
/// predate the current numbering.
///
/// Version 38 is upstream's original slot; version 39 is the slot it briefly
/// occupied while the upstream snapshot was being reconciled with the Codex Lab
/// ledger. Neither collides with the shipped Codex Lab migrations that now own
/// those versions, because every candidate row is matched by checksum first.
const LEGACY_RECENCY_VERSIONS: [i64; 2] = [38, 39];

pub(crate) async fn repair_legacy_recency_migration_version(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<()> {
    let Some(recency_migration) = migrator
        .migrations
        .iter()
        .find(|migration| migration.description == RECENCY_MIGRATION_DESCRIPTION)
    else {
        return Ok(());
    };
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if !migrations_table_exists {
        return Ok(());
    }

    for legacy_version in LEGACY_RECENCY_VERSIONS {
        if legacy_version == recency_migration.version {
            continue;
        }
        let legacy_recency_needs_repair = sqlx::query_scalar::<_, i64>(
            r#"
SELECT 1
FROM _sqlx_migrations
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
            "#,
        )
        .bind(legacy_version)
        .bind(recency_migration.checksum.as_ref())
        .bind(recency_migration.version)
        .fetch_optional(pool)
        .await?
        .is_some();
        if !legacy_recency_needs_repair {
            continue;
        }

        sqlx::query(
            r#"
UPDATE _sqlx_migrations
SET version = ?, description = ?
WHERE version = ?
  AND checksum = ?
  AND NOT EXISTS (
      SELECT 1 FROM _sqlx_migrations WHERE version = ?
  )
            "#,
        )
        .bind(recency_migration.version)
        .bind(recency_migration.description.as_ref())
        .bind(legacy_version)
        .bind(recency_migration.checksum.as_ref())
        .bind(recency_migration.version)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Description of the version 43 placeholder that replaced the destructive
/// agent-jobs drop, used to locate it without pinning a version number.
const RETAIN_AGENT_JOBS_DESCRIPTION: &str = "retain agent jobs";

/// The exact SQL that version 43 carried before it was reclaimed as an inert
/// placeholder. Databases that applied it recorded its checksum, so the rewrite
/// leaves them failing `Migrator::run` with `VersionMismatch(43)` forever.
///
/// The SQL is embedded rather than the checksum so the gate is derived the same
/// way sqlx derives it, and so a reader can see exactly which migration is being
/// matched. Version 43 was never released -- the shipped ledger stops at 38 --
/// so this only repairs candidate and development databases built from the #428
/// restoration branch.
const LEGACY_DROP_AGENT_JOBS_SQL: &str =
    "DROP TABLE IF EXISTS agent_job_items;\nDROP TABLE IF EXISTS agent_jobs;\n";

/// Re-point a recorded version 43 row from the destructive drop onto the inert
/// placeholder that replaced it.
///
/// The agent-job tables those databases dropped stay dropped: the agent-jobs
/// handlers are still `pending_restore`, so nothing reads them, and recreating
/// them here would invent a schema no migration produced. What the repair buys
/// is that the database opens at all instead of failing every upgrade.
pub(crate) async fn repair_legacy_agent_jobs_migration_checksum(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> anyhow::Result<()> {
    let Some(placeholder) = migrator
        .migrations
        .iter()
        .find(|migration| migration.description == RETAIN_AGENT_JOBS_DESCRIPTION)
    else {
        return Ok(());
    };
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?
    .is_some();
    if !migrations_table_exists {
        return Ok(());
    }

    let legacy = Migration::new(
        placeholder.version,
        Cow::Borrowed("drop agent jobs"),
        placeholder.migration_type,
        SqlStr::from_static(LEGACY_DROP_AGENT_JOBS_SQL),
        placeholder.no_tx,
    );

    sqlx::query(
        r#"
UPDATE _sqlx_migrations
SET checksum = ?, description = ?
WHERE version = ?
  AND checksum = ?
        "#,
    )
    .bind(placeholder.checksum.as_ref())
    .bind(placeholder.description.as_ref())
    .bind(placeholder.version)
    .bind(legacy.checksum.as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
