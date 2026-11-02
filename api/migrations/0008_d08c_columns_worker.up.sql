-- D08c (expand only): saved-view List columns and the outbox worker lease.
--
-- saved_views.columns: optional List columns of a private view, from a fixed allowlist of
-- non-PII display properties. NULL = the default column set. Number and title are always
-- shown and are therefore not part of the list.
ALTER TABLE saved_views
    ADD COLUMN columns text[]
    CHECK (columns IS NULL OR (
        cardinality(columns) BETWEEN 1 AND 6
        AND columns <@ ARRAY['status', 'priority', 'owner', 'channel', 'received', 'updated']::text[]
        AND array_position(columns, NULL) IS NULL
    ));

-- Worker lease: a delivery is claimed (claim_token + claimed_until committed) BEFORE the
-- provider is called and settled only by the holder of that token. A lease that expires while
-- still pending means the outcome is unknown; the sweeper marks it `unknown` (never retried
-- blindly). auto_attempts counts automatic attempts of the current round; a manual retry
-- starts a new round.
ALTER TABLE integration_outbox
    ADD COLUMN claim_token uuid,
    ADD COLUMN claimed_until timestamptz,
    ADD COLUMN auto_attempts integer NOT NULL DEFAULT 0 CHECK (auto_attempts >= 0),
    ADD CONSTRAINT integration_outbox_claim_pair CHECK ((claim_token IS NULL) = (claimed_until IS NULL)),
    ADD CONSTRAINT integration_outbox_claim_pending CHECK (claim_token IS NULL OR state = 'pending');

CREATE INDEX integration_outbox_lease_idx ON integration_outbox (claimed_until)
    WHERE state = 'pending' AND claim_token IS NOT NULL;
