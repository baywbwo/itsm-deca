ALTER TABLE itsm_deployment_marker DROP CONSTRAINT itsm_deployment_marker_kind_check;
ALTER TABLE itsm_deployment_marker ADD CONSTRAINT itsm_deployment_marker_kind_check CHECK (kind IN ('test_dummy','operational'));
ALTER TABLE sessions DROP CONSTRAINT sessions_access_mode_check;
ALTER TABLE sessions ADD CONSTRAINT sessions_access_mode_check CHECK (access_mode IN ('uat_superadmin','password'));
CREATE TABLE local_credentials (
 user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE RESTRICT,
 username text NOT NULL UNIQUE CHECK (username = lower(btrim(username)) AND length(username) BETWEEN 3 AND 254),
 password_hash text NOT NULL,
 failed_attempts integer NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
 locked_until timestamptz,
 updated_at timestamptz NOT NULL DEFAULT now()
);
