-- D18 (expand only): setelan koneksi LLM (OpenAI / Anthropic Claude) dan MCP server.
--
-- Aturan yang dipegang sama seperti integrasi D09:
--  * TIDAK ADA rahasia yang disimpan di basis data. Kolom `key_env`/`auth_env` hanya menyimpan
--    NAMA variabel lingkungan (atau nama secret di service secret manager). API melaporkan
--    "terpasang/belum" berdasarkan keberadaan variabel itu saat proses berjalan, tidak pernah
--    nilainya, dan tidak pernah mengirimkannya ke browser.
--  * Satu baris per penyedia per workspace, sehingga resolusi penyedia selalu tunggal.
--  * Perubahan menaikkan `version` (dipakai untuk konflik 409), setiap aksi menulis audit.

CREATE TABLE ai_providers (
    workspace_id  uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    provider      text NOT NULL CHECK (provider IN ('openai', 'anthropic')),
    model         text CHECK (model IS NULL OR length(btrim(model)) BETWEEN 1 AND 100),
    base_url      text CHECK (base_url IS NULL OR base_url ~ '^https://'),
    -- Nama variabel lingkungan yang memegang kunci; nilainya tidak pernah masuk database.
    key_env       text CHECK (key_env IS NULL OR key_env ~ '^[A-Z][A-Z0-9_]{2,60}$'),
    state         text NOT NULL DEFAULT 'draft' CHECK (state IN ('draft', 'tested', 'active', 'disabled')),
    tested_at     timestamptz,
    last_test_note text CHECK (last_test_note IS NULL OR length(last_test_note) <= 500),
    version       integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, provider),
    -- tested/active hanya bermakna bila model & sumber kunci sudah diisi.
    CHECK (state = 'draft' OR (model IS NOT NULL AND key_env IS NOT NULL))
);

CREATE TABLE mcp_servers (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    name          text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
    transport     text NOT NULL CHECK (transport IN ('http', 'stdio')),
    endpoint      text CHECK (endpoint IS NULL OR endpoint ~ '^https://'),
    command       text CHECK (command IS NULL OR length(btrim(command)) BETWEEN 1 AND 200),
    auth_env      text CHECK (auth_env IS NULL OR auth_env ~ '^[A-Z][A-Z0-9_]{2,60}$'),
    enabled       boolean NOT NULL DEFAULT false,
    tested_at     timestamptz,
    last_test_note text CHECK (last_test_note IS NULL OR length(last_test_note) <= 500),
    version       integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name),
    -- http butuh endpoint, stdio butuh command; keduanya tidak boleh terisi bersamaan.
    CHECK (
        (transport = 'http' AND endpoint IS NOT NULL AND command IS NULL)
        OR (transport = 'stdio' AND command IS NOT NULL AND endpoint IS NULL)
    )
);

CREATE INDEX mcp_servers_ws_idx ON mcp_servers (workspace_id, name);
