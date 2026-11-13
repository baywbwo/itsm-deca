-- CP8: cluster roles for production. Idempotent; run by the bootstrap superuser (postgres)
-- before the first `migrate`. Passwords are set separately from secret files
-- (init-db.sh), never written here. No role is a member of another, so the app can never
-- SET ROLE to the schema owner.
--   itsm_migrator  owns the database and every table; runs `migrate` and `init-password`.
--   itsm_app       API and worker at runtime: DML only, per itsm_grant_app() (0018, 0032).
--   itsm_backup    physical base backups only (REPLICATION, no database CONNECT).
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'itsm_migrator') THEN
        CREATE ROLE itsm_migrator LOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'itsm_app') THEN
        CREATE ROLE itsm_app LOGIN;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'itsm_backup') THEN
        CREATE ROLE itsm_backup LOGIN REPLICATION;
    END IF;
END $$;
ALTER ROLE itsm_migrator NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT
    CONNECTION LIMIT 5;
-- 2 API replicas + 1 worker at ITSM_DB_MAX_CONNECTIONS=10 each, plus headroom for a rolling restart.
ALTER ROLE itsm_app NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT
    CONNECTION LIMIT 60;
ALTER ROLE itsm_backup NOSUPERUSER NOCREATEDB NOCREATEROLE REPLICATION NOBYPASSRLS NOINHERIT
    CONNECTION LIMIT 3;
