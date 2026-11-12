-- auth_events is append-only: down is refused once 0021 data exists instead of rewriting audit.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM auth_events WHERE actor_id IS NOT NULL OR event = 'session_expire') THEN
        RAISE EXCEPTION '0021 down refused: auth_events hold actor_id or session_expire rows';
    END IF;
    IF EXISTS (SELECT 1 FROM sessions WHERE revoked_reason = 'expired') THEN
        RAISE EXCEPTION '0021 down refused: sessions revoked as expired exist';
    END IF;
END $$;
ALTER TABLE sessions DROP CONSTRAINT sessions_revoked_reason_check;
ALTER TABLE sessions ADD CONSTRAINT sessions_revoked_reason_check
    CHECK (revoked_reason IN ('logout','logout_all','password_change','admin_revoke','idle','mfa_reset'));
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up'));
DROP INDEX auth_events_actor_idx;
ALTER TABLE auth_events DROP COLUMN actor_id;
