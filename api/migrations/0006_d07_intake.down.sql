DROP TRIGGER IF EXISTS tickets_request_hash_fixed ON tickets;
DROP FUNCTION IF EXISTS itsm_ticket_request_hash_fixed();
ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_create_key_has_hash;
ALTER TABLE tickets DROP COLUMN IF EXISTS create_request_hash;
