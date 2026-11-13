-- Refused once live data exists (stored provider secrets, live connections or provider directory
-- rows): restore the pre-upgrade backup instead of silently dropping them.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM integration_connections
               WHERE mode = 'live' OR credential_secret_id IS NOT NULL)
       OR EXISTS (SELECT 1 FROM encrypted_secrets WHERE purpose IN ('jira_api_token', 'lark_app_secret'))
       OR EXISTS (SELECT 1 FROM jira_assignees WHERE source = 'jira')
       OR EXISTS (SELECT 1 FROM lark_destinations WHERE source = 'lark')
       OR EXISTS (SELECT 1 FROM integration_outbox WHERE requested_issue_key IS NOT NULL) THEN
        RAISE EXCEPTION '0024 down refused: live integration data exists; restore the backup instead';
    END IF;
END $$;
ALTER TABLE integration_outbox DROP COLUMN requested_issue_key;
ALTER TABLE lark_destinations DROP CONSTRAINT lark_destinations_source_check;
ALTER TABLE lark_destinations ADD CONSTRAINT lark_destinations_source_check CHECK (source IN ('dummy'));
ALTER TABLE jira_assignees DROP CONSTRAINT jira_assignees_source_check;
ALTER TABLE jira_assignees ADD CONSTRAINT jira_assignees_source_check CHECK (source IN ('dummy'));
DROP INDEX integration_connections_credential_uq;
ALTER TABLE integration_connections
    DROP CONSTRAINT integration_connections_credential_pair,
    DROP CONSTRAINT integration_connections_credential_fk,
    DROP COLUMN last_test_ok,
    DROP COLUMN credential_updated_at,
    DROP COLUMN credential_secret_id;
ALTER TABLE integration_connections DROP CONSTRAINT integration_connections_mode_check;
ALTER TABLE integration_connections ADD CONSTRAINT integration_connections_mode_check
    CHECK (mode IN ('simulation'));
ALTER TABLE encrypted_secrets DROP CONSTRAINT encrypted_secrets_purpose_check;
ALTER TABLE encrypted_secrets ADD CONSTRAINT encrypted_secrets_purpose_check
    CHECK (purpose IN ('totp_seed', 'llm_api_key', 'mcp_auth', 'imap_password', 'smtp_password'));
