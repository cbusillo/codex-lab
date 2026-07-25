-- Adapted from upstream migration 0036. Codex Lab reserves 0036 and 0037 for
-- session provenance and the legacy thread-history-mode compatibility floor.
CREATE INDEX idx_threads_visible_created_at_ms
    ON threads(archived, created_at_ms DESC)
    WHERE preview <> '';

CREATE INDEX idx_threads_visible_updated_at_ms
    ON threads(archived, updated_at_ms DESC)
    WHERE preview <> '';
