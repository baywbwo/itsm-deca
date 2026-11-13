-- CP5d: the operational DOB company carries its legal name, and platform admins assign a user's
-- home company (audited as user_company_set). Data-only rename, scoped to the bootstrap value:
-- a name an admin already changed is left alone.
UPDATE companies SET name = 'PT Delta Omega Beta', version = version + 1, updated_at = now()
 WHERE code = 'DOB' AND name = 'DOB';
UPDATE workspaces SET name = 'PT Delta Omega Beta', version = version + 1, updated_at = now()
 WHERE code = 'DOB' AND name = 'DOB';
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire',
        'recovery_codes_issue','user_unlock','mfa_disable','user_company_set'));
