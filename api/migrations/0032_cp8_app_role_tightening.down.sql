-- Restores the 0018 helper and re-applies it: the app role gets back DML on the marker,
-- _sqlx_migrations and the delete-guarded tables (attachments, workspace_grants).
CREATE OR REPLACE FUNCTION itsm_grant_app() RETURNS void
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
