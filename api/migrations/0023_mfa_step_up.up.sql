-- R1 I-2 CP4b: audit values for the admin unlock (PD-1) and a self-service MFA removal, and the
-- free-text reason an admin gives for an MFA reset (TM-02 g). Renumber freely when merging main.
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire',
        'recovery_codes_issue','user_unlock','mfa_disable'));
ALTER TABLE auth_events ADD COLUMN reason text
    CHECK (reason IS NULL OR (length(btrim(reason)) BETWEEN 1 AND 500 AND reason = btrim(reason)));
