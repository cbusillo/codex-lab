-- Version 43 previously dropped `agent_jobs` and `agent_job_items`. The agent-jobs
-- handlers are still `pending_restore` for #428, so dropping the tables would delete
-- rows that the restored feature is expected to read back. Version 43 was never part
-- of a release (the shipped ledger stops at 38), so the slot is reclaimed as an inert
-- placeholder rather than renumbered: keeping it occupied stops a future migration
-- from reusing version 43 with different SQL, and stops the destructive statements
-- from reappearing under a number that dev databases have already applied.
SELECT 1;
