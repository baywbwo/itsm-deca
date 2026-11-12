-- R1 I-3 CP6a (expand only): two-way Jira sync.
-- - Inbound: Jira Cloud webhooks signed with a per-connection secret (HMAC-SHA256, purpose
--   jira_webhook_secret, sealed with the keyring). Each delivery is recorded once per connection
--   (replay/idempotency); the Jira status and assignee land on jira_issues and, with Jira comments,
--   in the ticket timeline (ticket_activities without an ITSM actor). The ticket workflow status is
--   never changed by Jira (D02).
-- - Outbound: an ITSM ticket transition queues jira_transition_issue with the ITSM status it
--   targets; the worker maps it through the connection's status map at delivery time.
ALTER TABLE encrypted_secrets DROP CONSTRAINT encrypted_secrets_purpose_check;
ALTER TABLE encrypted_secrets ADD CONSTRAINT encrypted_secrets_purpose_check
    CHECK (purpose IN ('totp_seed', 'llm_api_key', 'mcp_auth', 'imap_password', 'smtp_password',
                       'jira_api_token', 'lark_app_secret', 'oidc_client_secret',
                       'jira_webhook_secret'));

ALTER TABLE integration_connections
    ADD COLUMN webhook_secret_id uuid,
    ADD COLUMN webhook_updated_at timestamptz,
    ADD CONSTRAINT integration_connections_webhook_fk
        FOREIGN KEY (workspace_id, webhook_secret_id)
        REFERENCES encrypted_secrets (workspace_id, id) ON DELETE RESTRICT,
    ADD CONSTRAINT integration_connections_webhook_pair
        CHECK ((webhook_secret_id IS NULL) = (webhook_updated_at IS NULL)),
    ADD CONSTRAINT integration_connections_webhook_jira
        CHECK (webhook_secret_id IS NULL OR provider = 'jira');
CREATE UNIQUE INDEX integration_connections_webhook_uq
    ON integration_connections (webhook_secret_id) WHERE webhook_secret_id IS NOT NULL;

ALTER TABLE jira_issues
    ADD COLUMN external_status_id text CHECK (length(external_status_id) <= 64),
    ADD COLUMN external_assignee_ref text CHECK (length(external_assignee_ref) <= 128),
    ADD COLUMN external_assignee_name text CHECK (length(external_assignee_name) <= 200);

-- Timeline entries written by the Jira webhook carry no ITSM actor, only the Jira display name.
ALTER TABLE ticket_activities DROP CONSTRAINT ticket_activities_activity_type_check;
ALTER TABLE ticket_activities
    ADD CONSTRAINT ticket_activities_activity_type_check
        CHECK (activity_type IN ('note', 'jira_comment', 'jira_status', 'jira_assignee')),
    ALTER COLUMN actor_id DROP NOT NULL,
    ADD COLUMN external_author text CHECK (length(btrim(external_author)) BETWEEN 1 AND 200),
    ADD CONSTRAINT ticket_activities_source CHECK (
        (activity_type = 'note' AND actor_id IS NOT NULL AND external_author IS NULL)
        OR (activity_type <> 'note' AND actor_id IS NULL));
CREATE UNIQUE INDEX ticket_activities_external_uq
    ON ticket_activities (workspace_id, idempotency_key) WHERE actor_id IS NULL;

CREATE TABLE jira_webhook_events (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id      uuid NOT NULL,
    connection_id     uuid NOT NULL,
    provider          text NOT NULL DEFAULT 'jira' CHECK (provider = 'jira'),
    delivery_id       text NOT NULL CHECK (length(delivery_id) BETWEEN 8 AND 200),
    body_sha256       bytea NOT NULL CHECK (octet_length(body_sha256) = 32),
    event             text NOT NULL CHECK (length(event) BETWEEN 1 AND 100),
    external_issue_id text CHECK (length(external_issue_id) <= 128),
    sent_at           timestamptz,
    outcome           text NOT NULL CHECK (outcome IN ('applied', 'ignored', 'unknown_issue', 'stale')),
    received_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (connection_id, delivery_id),
    FOREIGN KEY (workspace_id, connection_id, provider)
        REFERENCES integration_connections (workspace_id, id, provider) ON DELETE RESTRICT
);
CREATE INDEX jira_webhook_events_received_idx ON jira_webhook_events (received_at);

ALTER TABLE integration_outbox DROP CONSTRAINT integration_outbox_check;
ALTER TABLE integration_outbox
    ADD COLUMN target_ticket_status text CHECK (target_ticket_status IN
        ('new', 'open', 'pending_customer', 'pending_engineering', 'resolved', 'closed')),
    ADD CONSTRAINT integration_outbox_check CHECK (
        (provider = 'jira' AND operation IN ('jira_create_issue', 'jira_link_issue')
            AND related_work_id IS NOT NULL AND lark_destination_id IS NULL
            AND target_ticket_status IS NULL)
        OR (provider = 'jira' AND operation = 'jira_transition_issue'
            AND related_work_id IS NOT NULL AND lark_destination_id IS NULL
            AND target_ticket_status IS NOT NULL)
        OR (provider = 'lark' AND operation = 'lark_ticket_created'
            AND lark_destination_id IS NOT NULL AND related_work_id IS NULL
            AND target_ticket_status IS NULL));

SELECT itsm_grant_app();
