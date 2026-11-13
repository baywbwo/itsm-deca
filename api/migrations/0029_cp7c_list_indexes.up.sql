-- CP7c (1 juta tiket): indexes that match how the list, board, search and dashboard read
-- tickets. Found with EXPLAIN ANALYZE on the perf seed (evidence/i3/cp7c/).
--
-- Building these on 1M tickets takes ~10-20 s, longer than the 5 s statement_timeout every
-- connection gets (db.rs); lifted for this migration transaction only. lock_timeout (3 s)
-- stays: under write traffic the migration fails and can be retried instead of queueing
-- every request behind the DROP INDEX lock. Writes to tickets wait while an index builds
-- (CREATE INDEX holds a SHARE lock): run it in a quiet window (deploy/RUNBOOK-scaling.md).
SET LOCAL statement_timeout = 0;

-- 1. Keyset pages order by (sort column, id) in ONE direction. The D02 status index ended in
--    `id` ASC while every page orders `updated_at DESC, id DESC`, so a board column sorted all
--    tickets of a status (458k rows, 3 s) to return 21. Same columns, matching direction; a
--    backward scan serves the ascending order.
DROP INDEX tickets_status_idx;
CREATE INDEX tickets_status_idx ON tickets (workspace_id, workflow_status, updated_at DESC, id DESC);
-- 2. The default list (no status filter) orders the whole workspace by updated_at.
CREATE INDEX tickets_updated_idx ON tickets (workspace_id, updated_at DESC, id DESC);
-- 3. Sort by received_at pages on (received_at, id); the dashboard range reads keep working.
DROP INDEX tickets_received_idx;
CREATE INDEX tickets_received_idx ON tickets (workspace_id, received_at, id);
-- 4. Search is `subject ILIKE '%term%'` (plus an exact number). A trigram index answers it
--    without reading every subject of the workspace; terms under 3 characters still scan.
--    pg_trgm ships with PostgreSQL (contrib, trusted extension).
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX tickets_subject_trgm_idx ON tickets USING gin (subject gin_trgm_ops);
