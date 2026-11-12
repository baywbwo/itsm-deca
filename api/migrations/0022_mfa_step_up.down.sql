-- auth_events is append-only: down is refused once rows depend on 0022 instead of losing them.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM auth_events
               WHERE event IN ('user_unlock','mfa_disable') OR reason IS NOT NULL) THEN
        RAISE EXCEPTION '0022 down refused: auth_events hold user_unlock, mfa_disable or reason rows';
    END IF;
END $$;
ALTER TABLE auth_events DROP COLUMN reason;
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire',
        'recovery_codes_issue'));
