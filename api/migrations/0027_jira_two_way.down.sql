-- Refused once CP6a data exists (webhook secrets or deliveries, Jira timeline entries, transition
-- intents): restore the pre-upgrade backup instead of silently dropping them.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM encrypted_secrets WHERE purpose = 'jira_webhook_secret')
       OR EXISTS (SELECT 1 FROM jira_webhook_events)
       OR EXISTS (SELECT 1 FROM ticket_activities WHERE activity_type <> 'note')
       OR EXISTS (SELECT 1 FROM integration_outbox WHERE operation = 'jira_transition_issue') THEN
        RAISE EXCEPTION '0027 down refused: Jira sync data exists; restore the backup instead';
    END IF;
END $$;
ALTER TABLE integration_outbox DROP CONSTRAINT integration_outbox_check;
ALTER TABLE integration_outbox DROP COLUMN target_ticket_status;
ALTER TABLE integration_outbox ADD CONSTRAINT integration_outbox_check CHECK (
    (provider = 'jira' AND operation IN ('jira_create_issue', 'jira_link_issue')
        AND related_work_id IS NOT NULL AND lark_destination_id IS NULL)
    OR (provider = 'lark' AND operation = 'lark_ticket_created'
        AND lark_destination_id IS NOT NULL AND related_work_id IS NULL));
DROP TABLE jira_webhook_events;
DROP INDEX ticket_activities_external_uq;
ALTER TABLE ticket_activities
    DROP CONSTRAINT ticket_activities_source,
    DROP COLUMN external_author,
    ALTER COLUMN actor_id SET NOT NULL,
    DROP CONSTRAINT ticket_activities_activity_type_check;
ALTER TABLE ticket_activities ADD CONSTRAINT ticket_activities_activity_type_check
    CHECK (activity_type IN ('note'));
ALTER TABLE jira_issues
    DROP COLUMN external_assignee_name,
    DROP COLUMN external_assignee_ref,
    DROP COLUMN external_status_id;
DROP INDEX integration_connections_webhook_uq;
ALTER TABLE integration_connections
    DROP CONSTRAINT integration_connections_webhook_jira,
    DROP CONSTRAINT integration_connections_webhook_pair,
    DROP CONSTRAINT integration_connections_webhook_fk,
    DROP COLUMN webhook_updated_at,
    DROP COLUMN webhook_secret_id;
ALTER TABLE encrypted_secrets DROP CONSTRAINT encrypted_secrets_purpose_check;
ALTER TABLE encrypted_secrets ADD CONSTRAINT encrypted_secrets_purpose_check
    CHECK (purpose IN ('totp_seed', 'llm_api_key', 'mcp_auth', 'imap_password', 'smtp_password',
                       'jira_api_token', 'lark_app_secret', 'oidc_client_secret'));
