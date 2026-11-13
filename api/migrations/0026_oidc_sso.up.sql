-- R1 I-3 CP5c (expand only): company SSO through OpenID Connect (Authorization Code + PKCE).
-- - identity_providers: one IdP per company workspace. The client secret lives in encrypted_secrets
--   (purpose oidc_client_secret, keyring AEAD); non-secret settings stay in columns.
-- - external_identities: a user is linked to an IdP by (idp_id, subject), never by e-mail, so a
--   local account with the same address is not taken over by a new IdP login.
-- - oidc_login_attempts: one row per started sign-in, consumed once by the callback. Only the
--   SHA-256 of state and nonce are stored; the PKCE verifier is sealed with the keyring.
-- - sessions.auth_method tells password and OIDC sessions apart (ITSM_AUTH=sso keeps password
--   sign-in for break-glass platform admins only).
ALTER TABLE encrypted_secrets DROP CONSTRAINT encrypted_secrets_purpose_check;
ALTER TABLE encrypted_secrets ADD CONSTRAINT encrypted_secrets_purpose_check
    CHECK (purpose IN ('totp_seed', 'llm_api_key', 'mcp_auth', 'imap_password', 'smtp_password',
                       'jira_api_token', 'lark_app_secret', 'oidc_client_secret'));

CREATE TABLE identity_providers (
    id                       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id             uuid NOT NULL UNIQUE REFERENCES workspaces (id) ON DELETE RESTRICT,
    display_name             text NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 80),
    issuer                   text NOT NULL CHECK (issuer ~ '^https?://[^\s/?#@]+(/[^\s?#]*)?$'
                                                  AND length(issuer) <= 512),
    client_id                text NOT NULL CHECK (length(client_id) BETWEEN 1 AND 255
                                                  AND client_id !~ '\s'),
    client_secret_id         uuid,
    client_secret_updated_at timestamptz,
    -- Lower-case e-mail domains whose verified addresses may be provisioned just in time.
    email_domains            text[] NOT NULL DEFAULT '{}' CHECK (cardinality(email_domains) <= 20),
    -- Extra endpoint hosts allowed besides the issuer host (e.g. oauth2.googleapis.com).
    extra_hosts              text[] NOT NULL DEFAULT '{}' CHECK (cardinality(extra_hosts) <= 10),
    group_claim              text NOT NULL DEFAULT 'groups'
                                 CHECK (group_claim ~ '^[A-Za-z0-9_.:/-]{1,100}$'),
    -- [{"group": "<IdP group id/name>", "role": "cs"|"lead_cs"}]; validated by the API.
    role_mapping             jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(role_mapping) = 'array'),
    sync_roles               boolean NOT NULL DEFAULT false,
    enabled                  boolean NOT NULL DEFAULT false,
    last_test_ok             boolean,
    last_tested_at           timestamptz,
    version                  integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_by               uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    created_at               timestamptz NOT NULL DEFAULT now(),
    updated_by               uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    updated_at               timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT identity_providers_secret_fk FOREIGN KEY (workspace_id, client_secret_id)
        REFERENCES encrypted_secrets (workspace_id, id) ON DELETE RESTRICT,
    CONSTRAINT identity_providers_secret_pair
        CHECK ((client_secret_id IS NULL) = (client_secret_updated_at IS NULL)),
    -- An IdP is enabled only with a client secret and a passing discovery test.
    CONSTRAINT identity_providers_enabled_ready
        CHECK (NOT enabled OR (client_secret_id IS NOT NULL AND last_test_ok IS TRUE)),
    UNIQUE (workspace_id, id)
);
CREATE UNIQUE INDEX identity_providers_secret_uq
    ON identity_providers (client_secret_id) WHERE client_secret_id IS NOT NULL;
CREATE TRIGGER identity_providers_workspace_fixed BEFORE UPDATE ON identity_providers
    FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER identity_providers_version BEFORE UPDATE ON identity_providers
    FOR EACH ROW EXECUTE FUNCTION itsm_require_version_bump();

CREATE TABLE external_identities (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    idp_id        uuid NOT NULL REFERENCES identity_providers (id) ON DELETE RESTRICT,
    subject       text NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    -- NULL = linked by just-in-time provisioning; otherwise the admin who linked it.
    linked_by     uuid REFERENCES users (id) ON DELETE RESTRICT,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz,
    UNIQUE (idp_id, subject),
    UNIQUE (idp_id, user_id)
);
CREATE INDEX external_identities_user_idx ON external_identities (user_id);

CREATE TABLE oidc_login_attempts (
    state_hash     bytea PRIMARY KEY CHECK (octet_length(state_hash) = 32),
    idp_id         uuid NOT NULL REFERENCES identity_providers (id) ON DELETE RESTRICT,
    nonce_hash     bytea NOT NULL CHECK (octet_length(nonce_hash) = 32),
    verifier_key   text NOT NULL CHECK (verifier_key ~ '^[a-z0-9_-]{1,40}$'),
    verifier_nonce bytea NOT NULL CHECK (octet_length(verifier_nonce) = 12),
    verifier_ct    bytea NOT NULL CHECK (octet_length(verifier_ct) BETWEEN 17 AND 256),
    created_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL,
    CHECK (expires_at > created_at AND expires_at <= created_at + interval '15 minutes')
);
CREATE INDEX oidc_login_attempts_expiry_idx ON oidc_login_attempts (expires_at);

ALTER TABLE sessions
    ADD COLUMN auth_method text NOT NULL DEFAULT 'password'
        CHECK (auth_method IN ('password', 'oidc')),
    ADD COLUMN idp_id uuid REFERENCES identity_providers (id) ON DELETE RESTRICT,
    ADD CONSTRAINT sessions_oidc_idp CHECK ((auth_method = 'oidc') = (idp_id IS NOT NULL));

ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire',
        'recovery_codes_issue','user_unlock','mfa_disable','user_company_set',
        'oidc_login','oidc_provision','identity_link','identity_unlink'));

SELECT itsm_grant_app();
