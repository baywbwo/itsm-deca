-- Contract of 0010: drops the D10 export artifacts and columns. Runs created by D10 keep their
-- base columns (report_runs itself predates D10).
DROP TABLE IF EXISTS report_artifacts;
ALTER TABLE report_runs
    DROP CONSTRAINT IF EXISTS report_runs_artifact_ck,
    DROP CONSTRAINT IF EXISTS report_runs_failure_ck,
    DROP CONSTRAINT IF EXISTS report_runs_idem_uq,
    DROP COLUMN IF EXISTS failure_code,
    DROP COLUMN IF EXISTS expires_at,
    DROP COLUMN IF EXISTS artifact_bytes,
    DROP COLUMN IF EXISTS artifact_sha256,
    DROP COLUMN IF EXISTS row_count,
    DROP COLUMN IF EXISTS request_hash,
    DROP COLUMN IF EXISTS idempotency_key,
    DROP COLUMN IF EXISTS basis;
