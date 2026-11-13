-- CP7c (1 juta tiket): ticket counts per status without counting the tickets.
--
-- Every List / Board page and the Dashboard report the workspace's tickets per status. On
-- 500k tickets that GROUP BY reads the whole status index (~130 ms under load) and was half
-- of all database time in the 300-user test (evidence/i3/cp7c/). The counts for an
-- unfiltered workspace now come from this table, kept by triggers in the same transaction
-- as the ticket change, so a request sees counts and rows from one snapshot. Filtered scopes
-- (owner, channel, search, ...) still count the matching tickets.
--
-- 16 shards per (workspace, status), chosen by ticket id: concurrent changes to different
-- tickets rarely wait on the same counter row. Readers sum the shards. A counter row stays
-- locked until the writer commits, so writes of one shard and status queue behind each other.
SET LOCAL statement_timeout = 0;

-- Writes wait for the backfill (a few seconds on 1M tickets) so no change is missed between
-- the trigger and the initial counts. Reads continue.
LOCK TABLE tickets IN SHARE ROW EXCLUSIVE MODE;

CREATE TABLE ticket_status_counts (
    workspace_id    uuid     NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    workflow_status text     NOT NULL,
    shard           smallint NOT NULL CHECK (shard BETWEEN 0 AND 15),
    n               bigint   NOT NULL,
    PRIMARY KEY (workspace_id, workflow_status, shard)
);

-- Statement-level triggers: one ordered upsert per statement. Every writer takes the
-- counter rows in key order, so two transactions moving tickets in opposite directions
-- (new -> open and open -> new on the same shard) cannot deadlock on them. Row-level
-- triggers locked them in row order and deadlocked in the CP7c concurrency test.
CREATE FUNCTION itsm_ticket_status_count() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO ticket_status_counts AS c (workspace_id, workflow_status, shard, n)
        SELECT workspace_id, workflow_status, hashtext(id::text) & 15, count(*)
        FROM new_rows GROUP BY 1, 2, 3 ORDER BY 1, 2, 3
        ON CONFLICT (workspace_id, workflow_status, shard) DO UPDATE SET n = c.n + EXCLUDED.n;
    ELSIF TG_OP = 'DELETE' THEN
        INSERT INTO ticket_status_counts AS c (workspace_id, workflow_status, shard, n)
        SELECT workspace_id, workflow_status, hashtext(id::text) & 15, -count(*)
        FROM old_rows GROUP BY 1, 2, 3 ORDER BY 1, 2, 3
        ON CONFLICT (workspace_id, workflow_status, shard) DO UPDATE SET n = c.n + EXCLUDED.n;
    ELSE
        INSERT INTO ticket_status_counts AS c (workspace_id, workflow_status, shard, n)
        SELECT ws, st, sh, sum(d) FROM (
            SELECT o.workspace_id AS ws, o.workflow_status AS st, hashtext(o.id::text) & 15 AS sh, -1 AS d
            FROM old_rows o JOIN new_rows n ON n.id = o.id
            WHERE (o.workspace_id, o.workflow_status) IS DISTINCT FROM (n.workspace_id, n.workflow_status)
            UNION ALL
            SELECT n.workspace_id, n.workflow_status, hashtext(n.id::text) & 15, 1
            FROM old_rows o JOIN new_rows n ON n.id = o.id
            WHERE (o.workspace_id, o.workflow_status) IS DISTINCT FROM (n.workspace_id, n.workflow_status)
        ) moved
        GROUP BY 1, 2, 3 HAVING sum(d) <> 0 ORDER BY 1, 2, 3
        ON CONFLICT (workspace_id, workflow_status, shard) DO UPDATE SET n = c.n + EXCLUDED.n;
    END IF;
    RETURN NULL;
END $$;

CREATE TRIGGER tickets_status_count_ins AFTER INSERT ON tickets
    REFERENCING NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_ticket_status_count();
CREATE TRIGGER tickets_status_count_del AFTER DELETE ON tickets
    REFERENCING OLD TABLE AS old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_ticket_status_count();
-- Transition tables do not allow a column list (UPDATE OF ...): every UPDATE statement runs
-- the trigger, which finds no moved rows when the status and workspace stay the same.
CREATE TRIGGER tickets_status_count_upd AFTER UPDATE ON tickets
    REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_ticket_status_count();
-- Also under session_replication_role = replica (imports and legacy archive rows are written
-- that way), otherwise the counts would drift from the tickets.
ALTER TABLE tickets ENABLE ALWAYS TRIGGER tickets_status_count_ins;
ALTER TABLE tickets ENABLE ALWAYS TRIGGER tickets_status_count_del;
ALTER TABLE tickets ENABLE ALWAYS TRIGGER tickets_status_count_upd;
-- No TRUNCATE trigger: tickets are referenced by foreign keys, and TRUNCATE ... CASCADE reaches
-- the append-only tables, which refuse it.

INSERT INTO ticket_status_counts (workspace_id, workflow_status, shard, n)
SELECT workspace_id, workflow_status, hashtext(id::text) & 15, count(*)
FROM tickets GROUP BY 1, 2, 3;

-- Dashboard "perlu perhatian": the oldest active tickets of each attention priority. Read
-- per priority in received order and stop at the limit, instead of sorting every active
-- ticket of the workspace (~25k rows, ~120 ms). The predicate is dashboard.rs ACTIVE_SQL.
CREATE INDEX tickets_active_priority_idx ON tickets (workspace_id, priority_id, received_at, number)
    WHERE workflow_status IN ('new', 'open', 'pending_customer', 'pending_engineering');

SELECT itsm_grant_app();
