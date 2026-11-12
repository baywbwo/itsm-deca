-- DESTRUCTIVE once used: drops auth audit and reset tokens. Refuses if platform_admin grants exist.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM platform_grants WHERE grant_code = 'platform_admin') THEN
        RAISE EXCEPTION '0019 down refused: platform_admin grants exist';
    END IF;
END $$;
ALTER TABLE platform_grants DROP CONSTRAINT platform_grants_grant_code_check;
ALTER TABLE platform_grants ADD CONSTRAINT platform_grants_grant_code_check CHECK (grant_code IN ('uat_superadmin'));
DROP TABLE auth_events;
DROP FUNCTION itsm_auth_events_append_only();
DROP INDEX sessions_user_active_idx;
ALTER TABLE sessions DROP COLUMN last_seen_at, DROP COLUMN idle_expires_at, DROP COLUMN mfa_verified_at,
    DROP COLUMN step_up_at, DROP COLUMN credential_version, DROP COLUMN revoked_reason, DROP COLUMN user_agent_hash;
DROP TABLE password_reset_tokens;
ALTER TABLE local_credentials DROP COLUMN must_change_password, DROP COLUMN password_changed_at, DROP COLUMN credential_version;
DROP INDEX users_email_uq;
ALTER TABLE users DROP COLUMN email, DROP COLUMN created_by, DROP COLUMN deactivated_at;
