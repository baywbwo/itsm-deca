-- D17 (expand only): the business workspace code becomes editable without renumbering
-- history. Internal ids and foreign keys never change; only the label used for NEW numbers does.
--
-- 1. Each ticket keeps the prefix it was issued with (`number_prefix`), so an old number
--    (e.g. DUMA-5) stays identical in lists, details, search, history and exports after the
--    workspace is renamed. The per-workspace counter is untouched: new tickets continue it
--    (DECA-6, …) with the new prefix, no reset and no duplicate.
-- 2. Every code a workspace has ever used is reserved for that workspace in
--    `workspace_code_history`. Another workspace can never take it, so no two tickets anywhere
--    can show the same CODE-number; the owning workspace may switch back to its own old code.

ALTER TABLE tickets ADD COLUMN number_prefix text;
-- One-off backfill of a new column: the lifecycle guards (version +1, closed archive, …) are
-- about business edits, not this, so user triggers are off for this statement only. Versions,
-- updated_at and every other column stay as they are.
-- The D07 key/hash CHECK is NOT VALID on purpose (rows from before 0006 have a key without a
-- hash); an UPDATE would re-check it, so it is set aside for the backfill and restored as is.
ALTER TABLE tickets DROP CONSTRAINT tickets_create_key_has_hash;
ALTER TABLE tickets DISABLE TRIGGER USER;
UPDATE tickets t SET number_prefix = w.code FROM workspaces w WHERE w.id = t.workspace_id;
ALTER TABLE tickets ENABLE TRIGGER USER;
ALTER TABLE tickets ADD CONSTRAINT tickets_create_key_has_hash
    CHECK (create_idempotency_key IS NULL OR create_request_hash IS NOT NULL) NOT VALID;
ALTER TABLE tickets
    ALTER COLUMN number_prefix SET NOT NULL,
    ADD CONSTRAINT tickets_number_prefix_format CHECK (number_prefix ~ '^[A-Z0-9]{2,10}$');

CREATE TABLE workspace_code_history (
    code          text PRIMARY KEY CHECK (code ~ '^[A-Z0-9]{2,10}$'),
    workspace_id  uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    first_used_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX workspace_code_history_ws_idx ON workspace_code_history (workspace_id);
INSERT INTO workspace_code_history (code, workspace_id) SELECT code, id FROM workspaces;

-- Reservations are permanent: they are what keeps historical numbers unambiguous.
CREATE FUNCTION itsm_workspace_code_history_fixed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'riwayat kode workspace hanya bisa ditambah' USING ERRCODE = 'IT422';
END $$;
CREATE TRIGGER workspace_code_history_append_only BEFORE UPDATE OR DELETE ON workspace_code_history
    FOR EACH ROW EXECUTE FUNCTION itsm_workspace_code_history_fixed();

-- Reserves the (new) code for this workspace; a code held now or before by ANOTHER workspace
-- is rejected with unique_violation, also under concurrent renames (the primary key serialises).
CREATE FUNCTION itsm_workspace_code_reserve() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    owner uuid;
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.code IS NOT DISTINCT FROM OLD.code THEN
        RETURN NEW;
    END IF;
    INSERT INTO workspace_code_history (code, workspace_id) VALUES (NEW.code, NEW.id)
        ON CONFLICT (code) DO NOTHING;
    SELECT workspace_id INTO owner FROM workspace_code_history WHERE code = NEW.code;
    IF owner IS DISTINCT FROM NEW.id THEN
        RAISE EXCEPTION 'kode workspace sudah/pernah dipakai workspace lain'
            USING ERRCODE = 'unique_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER workspaces_code_reserve AFTER INSERT OR UPDATE OF code ON workspaces
    FOR EACH ROW EXECUTE FUNCTION itsm_workspace_code_reserve();

-- Replaces the D02 rule that froze the code after the first number.
DROP TRIGGER workspaces_code_stable ON workspaces;
DROP FUNCTION itsm_workspace_code_stable();

-- Stamps the prefix from the workspace's current code. FOR SHARE waits for a concurrent rename
-- to commit, so a ticket gets either the old or the new code, never a stale one after commit.
CREATE FUNCTION itsm_ticket_number_prefix() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    SELECT code INTO NEW.number_prefix FROM workspaces WHERE id = NEW.workspace_id FOR SHARE;
    IF NEW.number_prefix IS NULL THEN
        RAISE EXCEPTION 'workspace tiket tidak ada' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tickets_number_prefix BEFORE INSERT ON tickets
    FOR EACH ROW EXECUTE FUNCTION itsm_ticket_number_prefix();

CREATE FUNCTION itsm_ticket_number_prefix_fixed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.number_prefix IS DISTINCT FROM OLD.number_prefix THEN
        RAISE EXCEPTION 'prefix nomor tiket tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tickets_number_prefix_fixed BEFORE UPDATE OF number_prefix ON tickets
    FOR EACH ROW EXECUTE FUNCTION itsm_ticket_number_prefix_fixed();

-- An export keeps the code its file was named with; a later rename does not rename old files.
ALTER TABLE report_runs ADD COLUMN workspace_code text;
ALTER TABLE report_runs DISABLE TRIGGER USER;
UPDATE report_runs r SET workspace_code = w.code FROM workspaces w WHERE w.id = r.workspace_id;
ALTER TABLE report_runs ENABLE TRIGGER USER;
ALTER TABLE report_runs ALTER COLUMN workspace_code SET NOT NULL,
    ADD CONSTRAINT report_runs_workspace_code_format CHECK (workspace_code ~ '^[A-Z0-9]{2,10}$');

CREATE FUNCTION itsm_report_run_workspace_code() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT code INTO NEW.workspace_code FROM workspaces WHERE id = NEW.workspace_id FOR SHARE;
        IF NEW.workspace_code IS NULL THEN
            RAISE EXCEPTION 'workspace laporan tidak ada' USING ERRCODE = 'IT422';
        END IF;
    ELSIF NEW.workspace_code IS DISTINCT FROM OLD.workspace_code THEN
        RAISE EXCEPTION 'kode workspace laporan tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER report_runs_workspace_code BEFORE INSERT OR UPDATE OF workspace_code ON report_runs
    FOR EACH ROW EXECUTE FUNCTION itsm_report_run_workspace_code();
