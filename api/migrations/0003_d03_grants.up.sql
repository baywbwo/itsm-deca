-- D03: separate administration/integration grants per workspace membership (default deny).
-- Business permissions of CS / Lead CS are fixed in code (`authz.rs`); this table holds only
-- the extra grants that no role carries by itself. Expand-only; no grant is seeded.
--
-- A grant belongs to one membership in the same workspace (composite FK), so it can never
-- reach another workspace. It is effective only while it is not revoked AND the membership,
-- its user and its workspace are active. Rows are never deleted or edited, only revoked once.

CREATE TABLE workspace_grants (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id  uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    membership_id uuid NOT NULL,
    grant_code    text NOT NULL CHECK (grant_code IN (
                      'admin.members',
                      'admin.settings',
                      'integration.manage',
                      'integration.credentials',
                      'integration.recovery')),
    granted_by    uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    granted_at    timestamptz NOT NULL DEFAULT now(),
    revoked_by    uuid REFERENCES users (id) ON DELETE RESTRICT,
    revoked_at    timestamptz,
    CHECK ((revoked_at IS NULL) = (revoked_by IS NULL)),
    CHECK (revoked_at IS NULL OR revoked_at >= granted_at),
    FOREIGN KEY (workspace_id, membership_id)
        REFERENCES workspace_memberships (workspace_id, id) ON DELETE RESTRICT,
    UNIQUE (workspace_id, id)
);
-- At most one live grant per membership and code; history stays as revoked rows.
CREATE UNIQUE INDEX workspace_grants_live_uq
    ON workspace_grants (membership_id, grant_code) WHERE revoked_at IS NULL;
CREATE INDEX workspace_grants_ws_idx ON workspace_grants (workspace_id) WHERE revoked_at IS NULL;

CREATE FUNCTION itsm_grant_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.revoked_at IS NOT NULL THEN
            RAISE EXCEPTION 'grant baru tidak boleh langsung dicabut' USING ERRCODE = 'IT422';
        END IF;
        RETURN NEW;
    END IF;
    -- UPDATE: only a single revoke (NULL -> value) is allowed; identity stays fixed.
    IF OLD.revoked_at IS NOT NULL
       OR NEW.revoked_at IS NULL
       OR NEW.id <> OLD.id
       OR NEW.workspace_id <> OLD.workspace_id
       OR NEW.membership_id <> OLD.membership_id
       OR NEW.grant_code <> OLD.grant_code
       OR NEW.granted_by <> OLD.granted_by
       OR NEW.granted_at <> OLD.granted_at THEN
        RAISE EXCEPTION 'grant hanya boleh dicabut satu kali' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER workspace_grants_guard
    BEFORE INSERT OR UPDATE ON workspace_grants
    FOR EACH ROW EXECUTE FUNCTION itsm_grant_guard();
CREATE TRIGGER workspace_grants_no_delete
    BEFORE DELETE ON workspace_grants
    FOR EACH ROW EXECUTE FUNCTION itsm_append_only();

-- A membership keeps its workspace and its user: otherwise a grant (and ticket ownership)
-- would silently move to another account or workspace. Role/active stay editable.
CREATE FUNCTION itsm_membership_identity_fixed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
       OR NEW.user_id IS DISTINCT FROM OLD.user_id THEN
        RAISE EXCEPTION 'workspace/user keanggotaan tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER workspace_memberships_identity_fixed
    BEFORE UPDATE ON workspace_memberships
    FOR EACH ROW EXECUTE FUNCTION itsm_membership_identity_fixed();
