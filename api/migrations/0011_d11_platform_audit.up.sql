-- D11 platform audit (expand only): refusals of platform administration (companies, no workspace)
-- get their own append-only table. audit_events stays workspace-scoped (workspace_id NOT NULL) and
-- is not touched. Only a safe projection is stored: actor, action, missing capability, method,
-- route template and correlation id. Never IDs from the path, query, body, cookies or tokens.
CREATE TABLE platform_audit_events (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_id       uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    action         text NOT NULL CHECK (action IN ('platform.access.denied')),
    capability     text NOT NULL CHECK (capability ~ '^[a-z][a-z0-9_.]{1,79}$'),
    method         text NOT NULL CHECK (method IN ('GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE')),
    route          text NOT NULL CHECK (route ~ '^/api/v1/[A-Za-z0-9_/{}-]{1,200}$'),
    outcome        text NOT NULL CHECK (outcome = 'denied'),
    correlation_id text CHECK (length(correlation_id) <= 100),
    occurred_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX platform_audit_events_time_idx ON platform_audit_events (occurred_at DESC);
CREATE INDEX platform_audit_events_actor_idx ON platform_audit_events (actor_id, occurred_at DESC);

CREATE TRIGGER platform_audit_events_append_only BEFORE UPDATE OR DELETE ON platform_audit_events
    FOR EACH ROW EXECUTE FUNCTION itsm_append_only();
CREATE TRIGGER platform_audit_events_no_truncate BEFORE TRUNCATE ON platform_audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_append_only();
