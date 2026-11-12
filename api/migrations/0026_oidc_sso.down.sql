-- Refused once SSO data exists (IdP settings, linked identities, OIDC sessions or their audit
-- rows): restore the pre-upgrade backup instead of silently dropping them.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM identity_providers)
       OR EXISTS (SELECT 1 FROM encrypted_secrets WHERE purpose = 'oidc_client_secret')
       OR EXISTS (SELECT 1 FROM auth_events
                  WHERE event IN ('oidc_login','oidc_provision','identity_link','identity_unlink')) THEN
        RAISE EXCEPTION '0026 down refused: SSO data exists; restore the backup instead';
    END IF;
END $$;
ALTER TABLE auth_events DROP CONSTRAINT auth_events_event_check;
ALTER TABLE auth_events ADD CONSTRAINT auth_events_event_check
    CHECK (event IN ('login','logout','logout_all','mfa_enroll','mfa_verify','mfa_reset',
        'recovery_code_used','password_change','password_reset_issue','password_reset_use',
        'user_create','user_deactivate','lockout','step_up','session_expire',
        'recovery_codes_issue','user_unlock','mfa_disable','user_company_set'));
ALTER TABLE sessions
    DROP CONSTRAINT sessions_oidc_idp,
    DROP COLUMN idp_id,
    DROP COLUMN auth_method;
DROP TABLE oidc_login_attempts;
DROP TABLE external_identities;
DROP TABLE identity_providers;
ALTER TABLE encrypted_secrets DROP CONSTRAINT encrypted_secrets_purpose_check;
ALTER TABLE encrypted_secrets ADD CONSTRAINT encrypted_secrets_purpose_check
    CHECK (purpose IN ('totp_seed', 'llm_api_key', 'mcp_auth', 'imap_password', 'smtp_password',
                       'jira_api_token', 'lark_app_secret'));
