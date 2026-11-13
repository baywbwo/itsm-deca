-- CP8: database-level privileges. Run by the bootstrap superuser while connected to the
-- application database (created with OWNER itsm_migrator). Idempotent.
DO $$
BEGIN
    EXECUTE format('ALTER DATABASE %I OWNER TO itsm_migrator', current_database());
    -- PUBLIC loses CONNECT and TEMP: only the two application roles may connect, and the app
    -- never creates temporary objects.
    EXECUTE format('REVOKE ALL ON DATABASE %I FROM PUBLIC', current_database());
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO itsm_migrator, itsm_app', current_database());
END $$;
-- PostgreSQL 15+: public is owned by pg_database_owner (= itsm_migrator here); only the owner
-- creates objects. The app gets USAGE; table privileges come from itsm_grant_app() in migrations.
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO itsm_app;
-- Grants for tables the migrator creates later, even before the next itsm_grant_app() call.
ALTER DEFAULT PRIVILEGES FOR ROLE itsm_migrator IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO itsm_app;
ALTER DEFAULT PRIVILEGES FOR ROLE itsm_migrator IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO itsm_app;
