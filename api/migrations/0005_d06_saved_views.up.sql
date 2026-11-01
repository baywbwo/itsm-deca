-- D06 C32 saved views: private List/Board preferences of one user in one workspace.
-- Additive only (expand). A row stores filters/sort in the List query-string format, never
-- search text, sessions or secrets, and grants nothing: the List/Board endpoints re-check
-- permission and re-validate every filter when a view is applied.
CREATE TABLE saved_views (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    user_id      uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name         text NOT NULL CHECK (name = btrim(name) AND length(name) BETWEEN 1 AND 60),
    view         text NOT NULL CHECK (view IN ('list', 'board')),
    query        jsonb NOT NULL CHECK (jsonb_typeof(query) = 'object' AND NOT query ? 'q'),
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX saved_views_owner_name_uq
    ON saved_views (workspace_id, user_id, lower(name));
