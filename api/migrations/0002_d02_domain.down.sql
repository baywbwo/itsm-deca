-- Reverts 0002 on dummy/test databases only (D02 rollback check). Drops D02 tables and
-- guards; 0001 objects (companies, workspaces, memberships, sessions) are left intact.

DROP TRIGGER IF EXISTS workspaces_code_stable ON workspaces;
DROP TRIGGER IF EXISTS workspaces_counter ON workspaces;

DROP TABLE audit_events;
DROP TABLE report_runs;
DROP TABLE attachments;
DROP TABLE integration_outbox;
DROP TABLE related_work;
DROP TABLE lark_destinations;
DROP TABLE jira_issues;
DROP TABLE jira_assignees;
DROP TABLE integration_connections;
DROP TABLE ticket_resolution_events;
DROP TABLE tickets;
DROP TABLE merchants;
DROP TABLE intake_channels;
DROP TABLE regions;
DROP TABLE receiving_units;
DROP TABLE products;
DROP TABLE categories;
DROP TABLE priorities;
DROP TABLE workspace_ticket_counters;

DROP FUNCTION itsm_outbox_guard();
DROP FUNCTION itsm_resolution_event_before_insert();
DROP FUNCTION itsm_ticket_before_update();
DROP FUNCTION itsm_ticket_before_insert();
DROP FUNCTION itsm_ticket_owner_active();
DROP FUNCTION itsm_workspace_code_stable();
DROP FUNCTION itsm_workspace_after_insert();
DROP FUNCTION itsm_append_only();
DROP FUNCTION itsm_require_version_bump();
DROP FUNCTION itsm_forbid_workspace_change();
