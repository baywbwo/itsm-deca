-- Contract of 0012: an older binary knows only CSV. XLSX artifacts are expired first so that
-- binary never serves one as text/csv (it answers 410); nothing is deleted.
UPDATE report_runs SET expires_at = now()
WHERE format = 'xlsx' AND artifact_sha256 IS NOT NULL AND expires_at > now();
ALTER TABLE report_runs DROP COLUMN IF EXISTS format;
