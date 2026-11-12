-- R1 I-1 (expand only): DB hardening.
-- * Append-only tables (itsm_append_only, as 0011 does) that only guarded UPDATE/DELETE also refuse TRUNCATE (gap G26).
-- * Least-privilege grants for the runtime role itsm_app. Roles are cluster objects created by the
--   ops bootstrap script, never here; without the role (dev/CI) the grant step is a no-op.
CREATE TRIGGER audit_events_no_truncate BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_append_only();
CREATE TRIGGER ticket_activities_no_truncate BEFORE TRUNCATE ON ticket_activities
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_append_only();
CREATE TRIGGER resolution_events_no_truncate BEFORE TRUNCATE ON ticket_resolution_events
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_append_only();
CREATE TRIGGER workspace_code_history_no_truncate BEFORE TRUNCATE ON workspace_code_history
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_append_only();

-- Re-run at the end of every later migration that adds tables, so grants hold no matter which
-- role ran the migration. Append-only tables never get UPDATE/DELETE for the app role.
CREATE FUNCTION itsm_grant_app() RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    t text;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'itsm_app') THEN
        RETURN;
    END IF;
    EXECUTE 'GRANT USAGE ON SCHEMA public TO itsm_app';
    EXECUTE 'REVOKE CREATE ON SCHEMA public FROM itsm_app';
    EXECUTE 'GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO itsm_app';
    EXECUTE 'REVOKE TRUNCATE, REFERENCES, TRIGGER ON ALL TABLES IN SCHEMA public FROM itsm_app';
    EXECUTE 'GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO itsm_app';
    FOR t IN SELECT unnest(ARRAY['audit_events', 'ticket_activities', 'ticket_resolution_events',
                                 'workspace_code_history', 'platform_audit_events', 'auth_events'])
    LOOP
        IF to_regclass('public.' || t) IS NOT NULL THEN
            EXECUTE format('REVOKE UPDATE, DELETE ON %I FROM itsm_app', t);
        END IF;
    END LOOP;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'itsm_migrator') THEN
        EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE itsm_migrator IN SCHEMA public '
                'GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO itsm_app';
        EXECUTE 'ALTER DEFAULT PRIVILEGES FOR ROLE itsm_migrator IN SCHEMA public '
                'GRANT USAGE, SELECT ON SEQUENCES TO itsm_app';
    END IF;
END $$;

SELECT itsm_grant_app();
