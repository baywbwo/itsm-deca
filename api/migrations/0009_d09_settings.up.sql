-- D09 (expand only): workspace settings, membership administration and the integration
-- connection lifecycle (save -> test -> activate; simulation only, no secrets stored).
--
-- companies.reporter_code: optional reporter code until the official rule is known (PRD §6).
-- Free text with a length bound only; no format is invented.
ALTER TABLE companies
    ADD COLUMN reporter_code text
    CHECK (reporter_code IS NULL OR length(btrim(reporter_code)) BETWEEN 1 AND 40);

-- One connection per provider and workspace (intake resolves "the" Jira/Lark connection).
CREATE UNIQUE INDEX integration_connections_ws_provider_uq
    ON integration_connections (workspace_id, provider);

-- Last simulated test (what the test proved), shown next to tested_at.
ALTER TABLE integration_connections
    ADD COLUMN last_test_note text CHECK (last_test_note IS NULL OR length(last_test_note) <= 500);

-- From D09 on, only `active` connections receive deliveries. Seeded simulation connections of
-- a database explicitly marked as dummy test data were usable while `draft`; promote them so
-- existing UAT behaviour stays the same. A database without the dummy marker is untouched.
UPDATE integration_connections
SET state = 'active',
    tested_at = COALESCE(tested_at, now()),
    last_test_note = 'Diaktifkan saat migrasi D09 (data uji dummy, simulasi).',
    version = version + 1
WHERE mode = 'simulation' AND state = 'draft'
  AND EXISTS (SELECT 1 FROM itsm_deployment_marker WHERE kind = 'test_dummy');
