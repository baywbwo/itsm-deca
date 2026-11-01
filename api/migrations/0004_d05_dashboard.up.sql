-- D05 Dashboard: period aggregates read tickets by received_at and resolve events by
-- occurred_at inside one workspace. Additive only (expand); no data changes.
CREATE INDEX tickets_received_idx ON tickets (workspace_id, received_at);
CREATE INDEX resolution_events_period_idx
    ON ticket_resolution_events (workspace_id, occurred_at) WHERE action = 'resolve';
