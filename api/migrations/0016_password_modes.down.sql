DO $$ BEGIN
 IF EXISTS (SELECT 1 FROM local_credentials) OR EXISTS (SELECT 1 FROM itsm_deployment_marker WHERE kind='operational') OR EXISTS (SELECT 1 FROM sessions WHERE access_mode='password') THEN
 RAISE EXCEPTION 'password mode contains accounts or sessions; downgrade refused';
 END IF;
END $$;
DROP TABLE local_credentials;
ALTER TABLE sessions DROP CONSTRAINT sessions_access_mode_check;
ALTER TABLE sessions ADD CONSTRAINT sessions_access_mode_check CHECK(access_mode IN ('uat_superadmin'));
ALTER TABLE itsm_deployment_marker DROP CONSTRAINT itsm_deployment_marker_kind_check;
ALTER TABLE itsm_deployment_marker ADD CONSTRAINT itsm_deployment_marker_kind_check CHECK(kind='test_dummy');
