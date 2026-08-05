CREATE TABLE external_agent_runs (
    child_thread_id TEXT PRIMARY KEY NOT NULL,
    parent_thread_id TEXT NOT NULL,
    agent_path TEXT,
    routing_kind TEXT NOT NULL,
    requested_selector TEXT,
    effective_selector TEXT NOT NULL,
    routing_reason TEXT NOT NULL,
    skipped_candidates_json TEXT NOT NULL DEFAULT '[]',
    provider_family TEXT,
    command TEXT NOT NULL,
    cli_version TEXT,
    capability_source TEXT NOT NULL,
    capability_freshness TEXT,
    protocol TEXT NOT NULL,
    mode TEXT NOT NULL,
    workspace TEXT NOT NULL,
    model TEXT,
    effort TEXT,
    started_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    duration_ms INTEGER,
    terminal_state TEXT,
    failure_kind TEXT,
    failure_message TEXT
);

CREATE INDEX idx_external_agent_runs_parent
    ON external_agent_runs(parent_thread_id, started_at_ms, child_thread_id);
