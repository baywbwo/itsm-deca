-- D07 intake (expand only). A create stores the SHA-256 fingerprint of its normalized request
-- next to the existing per-user idempotency key, so a replay with the same key and the same
-- payload returns the original ticket and the same key with a different payload is refused.
-- The fingerprint is a hash, never request text, and is immutable like the key.
ALTER TABLE tickets
    ADD COLUMN create_request_hash bytea CHECK (octet_length(create_request_hash) = 32);
ALTER TABLE tickets
    ADD CONSTRAINT tickets_create_key_has_hash
    CHECK (create_idempotency_key IS NULL OR create_request_hash IS NOT NULL) NOT VALID;

CREATE FUNCTION itsm_ticket_request_hash_fixed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.create_request_hash IS DISTINCT FROM OLD.create_request_hash THEN
        RAISE EXCEPTION 'fingerprint create tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER tickets_request_hash_fixed BEFORE UPDATE OF create_request_hash ON tickets
    FOR EACH ROW EXECUTE FUNCTION itsm_ticket_request_hash_fixed();
