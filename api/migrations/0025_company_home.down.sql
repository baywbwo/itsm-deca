-- auth_events is append-only: once a company assignment is audited the old CHECK cannot hold.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM auth_events WHERE event = 'user_company_set') THEN
        RAISE EXCEPTION '0025 down refused: user_company_set audit rows exist; restore the backup instead';
    END IF;
END $$;
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire',
        'recovery_codes_issue','user_unlock','mfa_disable'));
UPDATE workspaces SET name = 'DOB', version = version + 1, updated_at = now()
 WHERE code = 'DOB' AND name = 'PT Delta Omega Beta';
UPDATE companies SET name = 'DOB', version = version + 1, updated_at = now()
 WHERE code = 'DOB' AND name = 'PT Delta Omega Beta';
