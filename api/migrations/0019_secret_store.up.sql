-- R1 I-1 (expand only): encrypted secrets (TOTP seeds, LLM/MCP/IMAP/SMTP credentials).
-- Only AEAD ciphertext is stored; keys live in the ITSM_KEYRING env and never reach the DB.
-- Replacing a secret inserts a new row and deletes the old one (not append-only by design).
CREATE TABLE encrypted_secrets (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid REFERENCES workspaces (id) ON DELETE RESTRICT,
    purpose      text NOT NULL CHECK (purpose IN ('totp_seed', 'llm_api_key', 'mcp_auth',
                                                  'imap_password', 'smtp_password')),
    key_id       text NOT NULL CHECK (key_id ~ '^[a-z0-9_-]{1,40}$'),
    algorithm    text NOT NULL CHECK (algorithm IN ('aes-256-gcm', 'xchacha20-poly1305')),
    nonce        bytea NOT NULL CHECK (octet_length(nonce) IN (12, 24)),
    ciphertext   bytea NOT NULL CHECK (octet_length(ciphertext) BETWEEN 17 AND 16384),
    fingerprint  bytea CHECK (fingerprint IS NULL OR octet_length(fingerprint) = 32),
    version      integer NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by   uuid REFERENCES users (id) ON DELETE RESTRICT,
    created_at   timestamptz NOT NULL DEFAULT now(),
    rotated_at   timestamptz,
    CONSTRAINT encrypted_secrets_scope CHECK ((purpose = 'totp_seed') = (workspace_id IS NULL)),
    CONSTRAINT encrypted_secrets_nonce_alg CHECK (
        (algorithm = 'aes-256-gcm' AND octet_length(nonce) = 12)
        OR (algorithm = 'xchacha20-poly1305' AND octet_length(nonce) = 24)),
    UNIQUE (workspace_id, id)
);
CREATE INDEX encrypted_secrets_key_idx ON encrypted_secrets (key_id);

SELECT itsm_grant_app();
