-- Removes the TRUNCATE guards and the grant helper. Privileges already granted to itsm_app stay
-- (revoking them would break a running app); ops revokes them by hand if the role is retired.
DROP FUNCTION itsm_grant_app();
DROP TRIGGER workspace_code_history_no_truncate ON workspace_code_history;
DROP TRIGGER resolution_events_no_truncate ON ticket_resolution_events;
DROP TRIGGER ticket_activities_no_truncate ON ticket_activities;
DROP TRIGGER audit_events_no_truncate ON audit_events;
