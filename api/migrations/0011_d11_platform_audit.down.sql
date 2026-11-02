-- Contract of 0011: removes the platform audit table and its rows (take a backup first,
-- RUNBOOK §6). Workspace audit_events is unaffected.
DROP TABLE IF EXISTS platform_audit_events;
