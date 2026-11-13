-- CP8 (expand only): least privilege for the runtime role itsm_app, the same helper 0018 added.
-- * Whatever an append-only guard trigger refuses (any itsm_*append_only function, or
--   itsm_workspace_code_history_fixed from 0014; row or statement level), the role is not granted either: derived from pg_trigger, so a table
--   guarded later is covered without editing a list here. A refused write then fails with
--   42501 at the privilege check, before any trigger runs.
-- * Schema bookkeeping (_sqlx_migrations) and the deployment marker are read-only for the app:
--   only the migrator (migrations, init-password) writes them.
-- Roles are still created by the ops bootstrap (deploy/production/db/roles.sql), never here;
-- without itsm_app (dev/CI) the helper is a no-op.
CREATE OR REPLACE FUNCTION itsm_grant_app() RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    t text;
    r record;
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
    -- tgtype bits: 8 = DELETE, 16 = UPDATE.
    FOR r IN
        SELECT c.relname,
               bool_or(tg.tgtype & 16 <> 0) AS upd,
               bool_or(tg.tgtype & 8 <> 0) AS del
        FROM pg_trigger tg
        JOIN pg_proc p ON p.oid = tg.tgfoid
        JOIN pg_class c ON c.oid = tg.tgrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE NOT tg.tgisinternal AND n.nspname = 'public'
          AND (p.proname LIKE 'itsm\_%append\_only'
               OR p.proname = 'itsm_workspace_code_history_fixed')
        GROUP BY c.relname
    LOOP
        IF r.upd THEN
            EXECUTE format('REVOKE UPDATE ON %I FROM itsm_app', r.relname);
        END IF;
        IF r.del THEN
            EXECUTE format('REVOKE DELETE ON %I FROM itsm_app', r.relname);
        END IF;
    END LOOP;
    FOR t IN SELECT unnest(ARRAY['_sqlx_migrations', 'itsm_deployment_marker'])
    LOOP
        IF to_regclass('public.' || t) IS NOT NULL THEN
            EXECUTE format('REVOKE INSERT, UPDATE, DELETE ON %I FROM itsm_app', t);
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
