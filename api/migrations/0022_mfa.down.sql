-- Enrolments, recovery codes and their audit rows are security records: down is refused once
-- any exist instead of silently dropping a user's second factor.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM user_mfa_totp) OR EXISTS (SELECT 1 FROM mfa_recovery_codes) THEN
        RAISE EXCEPTION '0022 down refused: MFA enrolments or recovery codes exist';
    END IF;
    IF EXISTS (SELECT 1 FROM auth_events WHERE event = 'recovery_codes_issue') THEN
        RAISE EXCEPTION '0022 down refused: auth_events hold recovery_codes_issue rows';
    END IF;
END $$;
DROP FUNCTION itsm_mfa_required(uuid);
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire'));
DROP TABLE mfa_login_challenges;
DROP TABLE mfa_recovery_codes;
DROP TABLE user_mfa_totp;
