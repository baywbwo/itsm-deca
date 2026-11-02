-- D13 (expand only): an operational export run records its artifact format. Existing runs are
-- CSV (the only format before D13), so the default keeps their meaning. The XLSX artifact uses the
-- same report_artifacts row, hash, expiry, ACL and audit as CSV.
ALTER TABLE report_runs
    ADD COLUMN format text NOT NULL DEFAULT 'csv' CHECK (format IN ('csv', 'xlsx'));
