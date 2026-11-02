-- Reverts 0008. Leases are dropped; a row that was claimed stays pending and is delivered
-- again by the D07 inline path (simulation only, UAT data).
DROP INDEX IF EXISTS integration_outbox_lease_idx;
ALTER TABLE integration_outbox
    DROP CONSTRAINT IF EXISTS integration_outbox_claim_pending,
    DROP CONSTRAINT IF EXISTS integration_outbox_claim_pair,
    DROP COLUMN IF EXISTS auto_attempts,
    DROP COLUMN IF EXISTS claimed_until,
    DROP COLUMN IF EXISTS claim_token;
ALTER TABLE saved_views DROP COLUMN IF EXISTS columns;
