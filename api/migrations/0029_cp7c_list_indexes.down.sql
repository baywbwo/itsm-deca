DROP INDEX IF EXISTS tickets_subject_trgm_idx;
DROP EXTENSION IF EXISTS pg_trgm;
DROP INDEX IF EXISTS tickets_received_idx;
CREATE INDEX tickets_received_idx ON tickets (workspace_id, received_at);
DROP INDEX IF EXISTS tickets_updated_idx;
DROP INDEX IF EXISTS tickets_status_idx;
CREATE INDEX tickets_status_idx ON tickets (workspace_id, workflow_status, updated_at DESC, id);
