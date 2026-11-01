-- D02a foundation: deployment marker, company/workspace/membership, platform grants, sessions.
-- Expand-only; no data is inserted here. Marking a database as a test database is an
-- explicit operator step (`itsm-api seed-uat`), never a side effect of migrating.

CREATE TABLE itsm_deployment_marker (
    singleton  boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    kind       text NOT NULL CHECK (kind = 'test_dummy'),
    marked_at  timestamptz NOT NULL DEFAULT now(),
    marked_db  text NOT NULL
);

CREATE TABLE companies (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    code        text NOT NULL UNIQUE CHECK (code ~ '^[A-Z0-9]{2,10}$'),
    name        text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    active      boolean NOT NULL DEFAULT true,
    version     integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- One independent company = one workspace (1:1).
CREATE TABLE workspaces (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    company_id  uuid NOT NULL UNIQUE REFERENCES companies (id) ON DELETE RESTRICT,
    code        text NOT NULL UNIQUE CHECK (code ~ '^[A-Z0-9]{2,10}$'),
    name        text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    timezone    text NOT NULL DEFAULT 'Asia/Jakarta',
    active      boolean NOT NULL DEFAULT true,
    version     integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Global accounts. No local password: UAT uses the Superadmin entry, SSO comes later.
CREATE TABLE users (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    display_name     text NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    external_subject text UNIQUE,
    active           boolean NOT NULL DEFAULT true,
    created_at       timestamptz NOT NULL DEFAULT now()
);

-- Platform-level grants, default deny. `uat_superadmin` is honoured only in UAT access mode.
CREATE TABLE platform_grants (
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    grant_code text NOT NULL CHECK (grant_code IN ('uat_superadmin')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, grant_code)
);

-- Business roles are only CS and Lead CS; admin/integration rights are separate grants (later).
CREATE TABLE workspace_memberships (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    role         text NOT NULL CHECK (role IN ('cs', 'lead_cs')),
    active       boolean NOT NULL DEFAULT true,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, user_id),
    -- Target for composite FKs (workspace_id, membership_id) from workspace-owned tables.
    UNIQUE (workspace_id, id)
);
CREATE INDEX workspace_memberships_user_idx ON workspace_memberships (user_id) WHERE active;

-- Server-side sessions. Only SHA-256 digests of the random tokens are stored.
CREATE TABLE sessions (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash  bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    csrf_hash   bytea NOT NULL CHECK (octet_length(csrf_hash) = 32),
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    access_mode text NOT NULL CHECK (access_mode IN ('uat_superadmin')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    revoked_at  timestamptz,
    CHECK (expires_at > created_at)
);
CREATE INDEX sessions_user_idx ON sessions (user_id);
