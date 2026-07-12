ALTER TABLE thread_history_projection_state
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
