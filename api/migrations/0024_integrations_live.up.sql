-- R1 I-3 CP5b (expand only): live Jira Cloud / Lark connections.
-- - integration_connections.mode gains 'live' (the server switch ITSM_INTEGRATIONS decides which
--   mode a connection is tested and delivered in; simulation stays for tests/CI/UAT).
-- - The provider secret (Jira API token / Lark app secret) lives in encrypted_secrets (AES-256-GCM,
--   keyring from the environment); the connection only points at it. Non-secret settings
--   (base URL, email, project, issue type, app id, mappings) stay in config jsonb.
-- - Directory rows learnt from the provider are tagged 'jira' / 'lark'; dummy rows stay as they are.
-- - A Jira "link" intake stores the requested key on the intent; the worker validates it at Jira.
ALTER TABLE encrypted_secrets DROP CONSTRAINT encrypted_secrets_purpose_check;
ALTER TABLE encrypted_secrets ADD CONSTRAINT encrypted_secrets_purpose_check
    CHECK (purpose IN ('totp_seed', 'llm_api_key', 'mcp_auth', 'imap_password', 'smtp_password',
                       'jira_api_token', 'lark_app_secret'));

ALTER TABLE integration_connections DROP CONSTRAINT integration_connections_mode_check;
ALTER TABLE integration_connections ADD CONSTRAINT integration_connections_mode_check
    CHECK (mode IN ('simulation', 'live'));
ALTER TABLE integration_connections
    ADD COLUMN credential_secret_id uuid,
    ADD COLUMN credential_updated_at timestamptz,
    ADD COLUMN last_test_ok boolean,
    ADD CONSTRAINT integration_connections_credential_fk
        FOREIGN KEY (workspace_id, credential_secret_id)
        REFERENCES encrypted_secrets (workspace_id, id) ON DELETE RESTRICT,
    ADD CONSTRAINT integration_connections_credential_pair
        CHECK ((credential_secret_id IS NULL) = (credential_updated_at IS NULL));
CREATE UNIQUE INDEX integration_connections_credential_uq
    ON integration_connections (credential_secret_id) WHERE credential_secret_id IS NOT NULL;

ALTER TABLE jira_assignees DROP CONSTRAINT jira_assignees_source_check;
ALTER TABLE jira_assignees ADD CONSTRAINT jira_assignees_source_check
    CHECK (source IN ('dummy', 'jira'));
ALTER TABLE lark_destinations DROP CONSTRAINT lark_destinations_source_check;
ALTER TABLE lark_destinations ADD CONSTRAINT lark_destinations_source_check
    CHECK (source IN ('dummy', 'lark'));

ALTER TABLE integration_outbox ADD COLUMN requested_issue_key text
    CHECK (requested_issue_key IS NULL OR requested_issue_key ~ '^[A-Z][A-Z0-9_]{0,30}-[0-9]{1,12}$');

SELECT itsm_grant_app();
