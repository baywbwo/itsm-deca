-- CP7d: rate limits shared by every API replica, for the endpoints where per-process
-- limits are not enough (sign-in, AI, outbound integration calls, Jira webhook).
-- One row per (scope, hashed identity, fixed window). The identity is a SHA-256 digest:
-- client addresses, user ids and workspace ids are never stored in clear.
-- UNLOGGED: counters are throwaway state; a crash empties them (limits restart from zero),
-- and they are not written to WAL, so brute-force bursts do not grow WAL or replicas.
CREATE UNLOGGED TABLE rate_limit_windows (
    scope text NOT NULL CHECK (scope ~ '^[a-z_]{1,40}$'),
    key bytea NOT NULL CHECK (octet_length(key) = 32),
    window_start timestamptz NOT NULL,
    hits integer NOT NULL CHECK (hits > 0),
    PRIMARY KEY (scope, key, window_start)
);
-- The worker prunes expired windows by age.
CREATE INDEX rate_limit_windows_start_idx ON rate_limit_windows (window_start);

SELECT itsm_grant_app();
