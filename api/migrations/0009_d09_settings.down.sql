-- Reverts 0009. Connection states stay as they are (all values were already valid in 0002);
-- D08c code delivers to any connection that is not disabled.
ALTER TABLE integration_connections DROP COLUMN IF EXISTS last_test_note;
DROP INDEX IF EXISTS integration_connections_ws_provider_uq;
ALTER TABLE companies DROP COLUMN IF EXISTS reporter_code;
