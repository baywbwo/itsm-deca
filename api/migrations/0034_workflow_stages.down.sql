DROP TRIGGER tickets_stage_guard ON tickets;
DROP FUNCTION itsm_ticket_stage_guard();
ALTER TABLE tickets DROP COLUMN stage_code;
DROP TRIGGER workspace_stages ON workspaces;
DROP FUNCTION itsm_seed_workflow_stages();
DROP TABLE workflow_stages;
