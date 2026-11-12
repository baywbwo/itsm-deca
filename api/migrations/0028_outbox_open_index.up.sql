-- CP7a: the /metrics scrape counts undelivered intents and their age per provider and state.
-- Partial index: delivered (success) rows, the vast majority, are not in it, so the scrape
-- stays an index-only read however large the outbox grows.
CREATE INDEX integration_outbox_open_idx
    ON integration_outbox (provider, state, created_at)
    WHERE state IN ('pending', 'failed', 'unknown');
