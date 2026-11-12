-- R1 I-1 CP2: user lifecycle, password change/reset, session idle/absolute limits, auth audit.
ALTER TABLE users
    ADD COLUMN email text CHECK (email = lower(btrim(email)) AND length(email) BETWEEN 3 AND 254),
    ADD COLUMN created_by uuid REFERENCES users (id) ON DELETE RESTRICT,
    ADD COLUMN deactivated_at timestamptz;
CREATE UNIQUE INDEX users_email_uq ON users (email) WHERE email IS NOT NULL;

ALTER TABLE local_credentials
    ADD COLUMN must_change_password boolean NOT NULL DEFAULT false,
    ADD COLUMN password_changed_at timestamptz,
    ADD COLUMN credential_version integer NOT NULL DEFAULT 1 CHECK (credential_version > 0);

-- Only SHA-256 digests of the random 256-bit reset tokens are stored.
CREATE TABLE password_reset_tokens (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    issued_by  uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at    timestamptz,
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '24 hours')
);
CREATE UNIQUE INDEX password_reset_tokens_one_active ON password_reset_tokens (user_id) WHERE used_at IS NULL;

ALTER TABLE sessions
    ADD COLUMN last_seen_at timestamptz,
    ADD COLUMN idle_expires_at timestamptz,
    ADD COLUMN mfa_verified_at timestamptz,
    ADD COLUMN step_up_at timestamptz,
    ADD COLUMN credential_version integer CHECK (credential_version > 0),
    ADD COLUMN revoked_reason text CHECK (revoked_reason IN ('logout','logout_all','password_change','admin_revoke','idle','mfa_reset')),
    ADD COLUMN user_agent_hash bytea CHECK (octet_length(user_agent_hash) = 32);
CREATE INDEX sessions_user_active_idx ON sessions (user_id) WHERE revoked_at IS NULL;

-- Append-only platform auth audit. Unknown usernames are stored only as a SHA-256 digest.
CREATE TABLE auth_events (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id        uuid REFERENCES users (id) ON DELETE RESTRICT,
    username_hash  bytea CHECK (octet_length(username_hash) = 32),
    event          text NOT NULL CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
                       'recovery_code_used','password_change','password_reset_issue','password_reset_use',
                       'user_create','user_deactivate','lockout','step_up')),
    outcome        text NOT NULL CHECK (outcome IN ('success','failed','denied')),
    failure_code   text CHECK (failure_code ~ '^[a-z0-9_]{1,60}$'),
    peer_ip        inet,
    correlation_id text CHECK (length(correlation_id) <= 100),
    occurred_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX auth_events_user_idx ON auth_events (user_id, occurred_at DESC);
CREATE INDEX auth_events_time_idx ON auth_events (occurred_at DESC);

CREATE FUNCTION itsm_auth_events_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'auth_events is append-only' USING ERRCODE = '42501';
END $$;
CREATE TRIGGER auth_events_no_update BEFORE UPDATE OR DELETE ON auth_events
    FOR EACH ROW EXECUTE FUNCTION itsm_auth_events_append_only();
CREATE TRIGGER auth_events_no_truncate BEFORE TRUNCATE ON auth_events
    FOR EACH STATEMENT EXECUTE FUNCTION itsm_auth_events_append_only();

ALTER TABLE platform_grants DROP CONSTRAINT platform_grants_grant_code_check;
ALTER TABLE platform_grants ADD CONSTRAINT platform_grants_grant_code_check
    CHECK (grant_code IN ('uat_superadmin','platform_admin'));
