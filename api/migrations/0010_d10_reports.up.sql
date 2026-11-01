-- D10 (expand only): operational report exports (O06). Official BI/regulator export stays
-- held: report_type keeps its 'operational'-only check and mapping_version stays NULL.
--
-- A run is requested idempotently per user + workspace (Idempotency-Key). It records the date
-- basis, the result size and the SHA-256 of the exact CSV bytes, and expires; the CSV itself
-- lives in report_artifacts (same workspace, never moved) and is only served after the ACL is
-- checked again. failure_code explains a failed run (e.g. the row limit), never with data.
ALTER TABLE report_runs
    ADD COLUMN basis text NOT NULL DEFAULT 'received' CHECK (basis IN ('received', 'resolved')),
    ADD COLUMN idempotency_key text CHECK (length(idempotency_key) BETWEEN 8 AND 128),
    ADD COLUMN request_hash bytea CHECK (length(request_hash) = 32),
    ADD COLUMN row_count integer CHECK (row_count >= 0),
    ADD COLUMN artifact_sha256 bytea CHECK (length(artifact_sha256) = 32),
    ADD COLUMN artifact_bytes integer CHECK (artifact_bytes >= 0),
    ADD COLUMN expires_at timestamptz,
    ADD COLUMN failure_code text CHECK (failure_code IN ('row_limit', 'internal')),
    ADD CONSTRAINT report_runs_idem_uq UNIQUE (workspace_id, requested_by, idempotency_key),
    ADD CONSTRAINT report_runs_failure_ck CHECK (failure_code IS NULL OR state = 'failed'),
    ADD CONSTRAINT report_runs_artifact_ck CHECK (artifact_sha256 IS NULL OR (state = 'succeeded'
        AND row_count IS NOT NULL AND artifact_bytes IS NOT NULL AND expires_at IS NOT NULL));

CREATE TABLE report_artifacts (
    run_id       uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    content      bytea NOT NULL CHECK (length(content) <= 104857600),
    sha256       bytea NOT NULL CHECK (length(sha256) = 32),
    created_at   timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (workspace_id, run_id) REFERENCES report_runs (workspace_id, id) ON DELETE RESTRICT
);

CREATE TRIGGER report_artifacts_ws_fixed BEFORE UPDATE ON report_artifacts
    FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
