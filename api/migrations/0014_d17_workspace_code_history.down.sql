-- Contract of 0014. Refused when any workspace has been renamed since: the older binary shows
-- CODE-number from the current workspace code, so rolling back would silently renumber the
-- history of renamed workspaces. Rename those workspaces back to their tickets' prefix first.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM tickets t JOIN workspaces w ON w.id = t.workspace_id
        WHERE t.number_prefix <> w.code
    ) OR EXISTS (
        SELECT 1 FROM report_runs r JOIN workspaces w ON w.id = r.workspace_id
        WHERE r.workspace_code <> w.code
    ) THEN
        RAISE EXCEPTION 'rollback 0014 ditolak: ada tiket/laporan dengan kode berbeda dari kode workspace';
    END IF;
END $$;

DROP TRIGGER IF EXISTS report_runs_workspace_code ON report_runs;
DROP FUNCTION IF EXISTS itsm_report_run_workspace_code();
ALTER TABLE report_runs DROP COLUMN IF EXISTS workspace_code;
DROP TRIGGER IF EXISTS tickets_number_prefix_fixed ON tickets;
DROP FUNCTION IF EXISTS itsm_ticket_number_prefix_fixed();
DROP TRIGGER IF EXISTS tickets_number_prefix ON tickets;
DROP FUNCTION IF EXISTS itsm_ticket_number_prefix();
DROP TRIGGER IF EXISTS workspaces_code_reserve ON workspaces;
DROP FUNCTION IF EXISTS itsm_workspace_code_reserve();
DROP TABLE IF EXISTS workspace_code_history;
DROP FUNCTION IF EXISTS itsm_workspace_code_history_fixed();
ALTER TABLE tickets DROP COLUMN IF EXISTS number_prefix;

CREATE FUNCTION itsm_workspace_code_stable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code IS DISTINCT FROM OLD.code AND EXISTS (
        SELECT 1 FROM workspace_ticket_counters
        WHERE workspace_id = OLD.id AND last_number > 0
    ) THEN
        RAISE EXCEPTION 'kode workspace tidak boleh diubah setelah nomor tiket terbit'
            USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER workspaces_code_stable BEFORE UPDATE OF code ON workspaces
    FOR EACH ROW EXECUTE FUNCTION itsm_workspace_code_stable();
