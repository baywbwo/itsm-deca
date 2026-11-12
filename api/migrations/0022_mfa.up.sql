-- R1 I-2 CP4a (US-1.2, TM-02): TOTP MFA, recovery codes and the second login step.
-- Seeds live only as AEAD ciphertext in encrypted_secrets (purpose totp_seed); recovery codes
-- are stored only as HMAC-SHA256 digests. No plaintext secret reaches this schema.
CREATE TABLE user_mfa_totp (
    user_id          uuid PRIMARY KEY REFERENCES users (id) ON DELETE RESTRICT,
    secret_id        uuid NOT NULL UNIQUE REFERENCES encrypted_secrets (id) ON DELETE RESTRICT,
    algorithm        text NOT NULL DEFAULT 'SHA1' CHECK (algorithm = 'SHA1'),
    digits           smallint NOT NULL DEFAULT 6 CHECK (digits IN (6)),
    period_seconds   smallint NOT NULL DEFAULT 30 CHECK (period_seconds IN (30)),
    enrolled_at      timestamptz NOT NULL DEFAULT now(),
    -- NULL = enrolment started but not proven with a first valid code (it expires).
    confirmed_at     timestamptz,
    -- Anti-replay (ASVS 2.8.4): a code is accepted only for a step above this one.
    last_used_step   bigint CHECK (last_used_step >= 0),
    failed_attempts  integer NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    locked_until     timestamptz,
    version          integer NOT NULL DEFAULT 1 CHECK (version > 0),
    CHECK (confirmed_at IS NULL OR confirmed_at >= enrolled_at)
);

CREATE TABLE mfa_recovery_codes (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    batch_id   uuid NOT NULL,
    -- HMAC-SHA256(ITSM_MFA_RECOVERY_PEPPER, normalised code); codes carry 80 random bits.
    code_hash  bytea NOT NULL UNIQUE CHECK (octet_length(code_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    used_at    timestamptz,
    CHECK (used_at IS NULL OR used_at >= created_at)
);
CREATE INDEX mfa_recovery_codes_unused_idx ON mfa_recovery_codes (user_id) WHERE used_at IS NULL;

-- Second login step: issued after a correct password, consumed once by POST /login/mfa.
-- The id is random (gen_random_uuid) and useless without the second factor itself.
CREATE TABLE mfa_login_challenges (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id            uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    credential_version integer NOT NULL CHECK (credential_version > 0),
    created_at         timestamptz NOT NULL DEFAULT now(),
    expires_at         timestamptz NOT NULL,
    used_at            timestamptz,
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '10 minutes'),
    CHECK (used_at IS NULL OR used_at >= created_at)
);
CREATE INDEX mfa_login_challenges_user_idx ON mfa_login_challenges (user_id);

-- New audit value: a fresh recovery-code batch (the old batch is spent).
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire',
        'recovery_codes_issue'));

-- MFA is mandatory (not a stored flag) for platform admins and anyone holding a live admin.*,
-- integration.* or ai.manage grant through an active membership (TM-02 a).
CREATE FUNCTION itsm_mfa_required(p_user uuid) RETURNS boolean
LANGUAGE sql STABLE AS $$
    SELECT EXISTS (SELECT 1 FROM platform_grants g
                   WHERE g.user_id = p_user AND g.grant_code = 'platform_admin')
        OR EXISTS (SELECT 1 FROM workspace_grants g
                   JOIN workspace_memberships m ON m.id = g.membership_id
                   WHERE m.user_id = p_user AND m.active AND g.revoked_at IS NULL
                     AND (g.grant_code LIKE 'admin.%' OR g.grant_code LIKE 'integration.%'
                          OR g.grant_code = 'ai.manage'))
$$;

SELECT itsm_grant_app();
