-- D02 domain schema: per-workspace catalogs, tickets with per-workspace numbering and
-- lifecycle guards, related work, Jira/Lark relations (simulation only), outbox,
-- attachments, report runs and audit. Expand-only; no data is inserted here.
--
-- Isolation rule (PRD §8): every workspace-owned table has workspace_id NOT NULL and
-- UNIQUE (workspace_id, id); children reference parents with a composite FK
-- (workspace_id, parent_id) so a cross-workspace parent is rejected by the database.
-- No cascading deletes anywhere.
--
-- Custom SQLSTATEs raised by guards (mapped by the API):
--   IT409 = version not incremented by exactly one (optimistic lock backstop)
--   IT422 = domain rule violated (lifecycle, outbox transition, immutable column)

-- ---------- shared guard functions ----------

CREATE FUNCTION itsm_forbid_workspace_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.workspace_id IS DISTINCT FROM OLD.workspace_id THEN
        RAISE EXCEPTION 'workspace_id tidak boleh diubah (%)', TG_TABLE_NAME
            USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION itsm_require_version_bump() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.version IS DISTINCT FROM OLD.version + 1 THEN
        RAISE EXCEPTION 'version harus naik tepat satu (%)', TG_TABLE_NAME
            USING ERRCODE = 'IT409';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION itsm_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% hanya boleh ditambah (append-only)', TG_TABLE_NAME
        USING ERRCODE = 'IT422';
END $$;

-- ---------- ticket numbering ----------

-- One counter per workspace; incremented under a row lock inside the inserting
-- transaction, so numbers are unique and a rolled-back insert does not consume one.
CREATE TABLE workspace_ticket_counters (
    workspace_id uuid PRIMARY KEY REFERENCES workspaces (id) ON DELETE RESTRICT,
    last_number  bigint NOT NULL DEFAULT 0 CHECK (last_number >= 0)
);

INSERT INTO workspace_ticket_counters (workspace_id) SELECT id FROM workspaces;

CREATE FUNCTION itsm_workspace_after_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO workspace_ticket_counters (workspace_id) VALUES (NEW.id);
    RETURN NEW;
END $$;

CREATE TRIGGER workspaces_counter AFTER INSERT ON workspaces
    FOR EACH ROW EXECUTE FUNCTION itsm_workspace_after_insert();

-- The workspace code is part of the displayed ticket number (CODE-number), so it is
-- frozen once the first number has been issued. The company name stays editable.
CREATE FUNCTION itsm_workspace_code_stable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code IS DISTINCT FROM OLD.code AND EXISTS (
        SELECT 1 FROM workspace_ticket_counters
        WHERE workspace_id = OLD.id AND last_number > 0
    ) THEN
        RAISE EXCEPTION 'kode workspace tidak boleh diubah setelah nomor tiket terbit'
            USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER workspaces_code_stable BEFORE UPDATE OF code ON workspaces
    FOR EACH ROW EXECUTE FUNCTION itsm_workspace_code_stable();

-- ---------- per-workspace catalogs ----------
-- Code is unique per workspace; label is display text and may change. External
-- provider/regulator codes are mapped separately later, never through the label.

CREATE TABLE priorities (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    -- Existing priority values only; no new scale (D01 freeze).
    code         text NOT NULL CHECK (code IN ('low', 'normal', 'high', 'urgent')),
    label        text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 120),
    active       boolean NOT NULL DEFAULT true,
    sort_order   integer NOT NULL DEFAULT 0,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, code),
    UNIQUE (workspace_id, id)
);

CREATE TABLE categories (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    code         text NOT NULL CHECK (code ~ '^[a-z0-9_]{1,40}$'),
    label        text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 120),
    active       boolean NOT NULL DEFAULT true,
    sort_order   integer NOT NULL DEFAULT 0,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, code),
    UNIQUE (workspace_id, id)
);

CREATE TABLE products (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    code         text NOT NULL CHECK (code ~ '^[a-z0-9_]{1,40}$'),
    label        text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 120),
    active       boolean NOT NULL DEFAULT true,
    sort_order   integer NOT NULL DEFAULT 0,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, code),
    UNIQUE (workspace_id, id)
);

CREATE TABLE receiving_units (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    code         text NOT NULL CHECK (code ~ '^[a-z0-9_]{1,40}$'),
    label        text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 120),
    active       boolean NOT NULL DEFAULT true,
    sort_order   integer NOT NULL DEFAULT 0,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, code),
    UNIQUE (workspace_id, id)
);

CREATE TABLE regions (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    code         text NOT NULL CHECK (code ~ '^[a-z0-9_]{1,40}$'),
    label        text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 120),
    active       boolean NOT NULL DEFAULT true,
    sort_order   integer NOT NULL DEFAULT 0,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, code),
    UNIQUE (workspace_id, id)
);

-- Intake channel (WhatsApp/Email/Website…) is a catalog, not a hard-coded enum.
CREATE TABLE intake_channels (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    code         text NOT NULL CHECK (code ~ '^[a-z0-9_]{1,40}$'),
    label        text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 120),
    active       boolean NOT NULL DEFAULT true,
    sort_order   integer NOT NULL DEFAULT 0,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, code),
    UNIQUE (workspace_id, id)
);

-- ---------- merchants ----------
-- CIF is dummy for now: only source 'dummy' is accepted, and there is no "verified" flag.
CREATE TABLE merchants (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id      uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    source            text NOT NULL CHECK (source IN ('dummy')),
    cif_reference     text CHECK (length(btrim(cif_reference)) BETWEEN 1 AND 64),
    display_name      text NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    source_updated_at timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    -- Same CIF reference in two workspaces stays two separate merchants.
    UNIQUE (workspace_id, source, cif_reference),
    UNIQUE (workspace_id, id)
);

-- ---------- tickets ----------

CREATE TABLE tickets (
    id                       uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id             uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    -- Assigned by trigger from workspace_ticket_counters; any client value is overwritten.
    number                   bigint NOT NULL CHECK (number > 0),
    subject                  text NOT NULL CHECK (length(btrim(subject)) BETWEEN 1 AND 300),
    chronology               text NOT NULL CHECK (length(btrim(chronology)) BETWEEN 1 AND 20000),
    received_at              timestamptz NOT NULL,
    intake_channel_id        uuid,
    priority_id              uuid,
    category_id              uuid,
    product_id               uuid,
    receiving_unit_id        uuid,
    region_id                uuid,
    merchant_id              uuid,
    -- PIC CS: must be an active member of this workspace (FK + trigger). NULL = unassigned.
    owner_user_id            uuid,
    -- Internal codes; UI labels live in the API. 'closed' is accepted only as a
    -- historical archive state: writing it is refused (close final is held).
    workflow_status          text NOT NULL DEFAULT 'new' CHECK (workflow_status IN
                                 ('new', 'open', 'pending_customer', 'pending_engineering',
                                  'resolved', 'closed')),
    cycle_no                 integer NOT NULL DEFAULT 1 CHECK (cycle_no > 0),
    transaction_activity     text CHECK (length(transaction_activity) <= 20000),
    cause                    text CHECK (length(cause) <= 20000),
    foreign_branch_reference text CHECK (length(btrim(foreign_branch_reference)) BETWEEN 1 AND 100),
    create_idempotency_key   text CHECK (length(create_idempotency_key) BETWEEN 8 AND 128),
    version                  integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_by               uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    created_at               timestamptz NOT NULL DEFAULT now(),
    updated_at               timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, number),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, created_by, create_idempotency_key),
    FOREIGN KEY (workspace_id, intake_channel_id) REFERENCES intake_channels (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, priority_id) REFERENCES priorities (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, category_id) REFERENCES categories (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, product_id) REFERENCES products (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, receiving_unit_id) REFERENCES receiving_units (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, region_id) REFERENCES regions (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, merchant_id) REFERENCES merchants (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, owner_user_id)
        REFERENCES workspace_memberships (workspace_id, user_id) ON DELETE RESTRICT
);
CREATE INDEX tickets_status_idx ON tickets (workspace_id, workflow_status, updated_at DESC, id);
CREATE INDEX tickets_owner_idx ON tickets (workspace_id, owner_user_id) WHERE owner_user_id IS NOT NULL;

-- Resolve/reopen history per cycle; never overwritten.
CREATE TABLE ticket_resolution_events (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL,
    ticket_id    uuid NOT NULL,
    cycle_no     integer NOT NULL CHECK (cycle_no > 0),
    action       text NOT NULL CHECK (action IN ('resolve', 'reopen')),
    -- resolve: solution summary; reopen: reason.
    summary      text NOT NULL CHECK (length(btrim(summary)) BETWEEN 1 AND 5000),
    actor_id     uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    occurred_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (ticket_id, cycle_no, action),
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, ticket_id) REFERENCES tickets (workspace_id, id) ON DELETE RESTRICT
);

-- ---------- integrations (simulation only; no secrets stored here) ----------

CREATE TABLE integration_connections (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    provider     text NOT NULL CHECK (provider IN ('jira', 'lark')),
    -- Real providers are not connected yet; anything else is refused.
    mode         text NOT NULL DEFAULT 'simulation' CHECK (mode IN ('simulation')),
    display_name text NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    config       jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(config) = 'object'),
    state        text NOT NULL DEFAULT 'draft' CHECK (state IN ('draft', 'tested', 'active', 'disabled')),
    tested_at    timestamptz,
    version      integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    -- Lets children pin the provider of their connection.
    UNIQUE (workspace_id, id, provider)
);

-- Labelled dummy Jira assignee directory per workspace (provider lookup deferred).
CREATE TABLE jira_assignees (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id         uuid NOT NULL,
    connection_id        uuid NOT NULL,
    provider             text NOT NULL DEFAULT 'jira' CHECK (provider = 'jira'),
    external_account_ref text NOT NULL CHECK (length(btrim(external_account_ref)) BETWEEN 1 AND 128),
    display_name         text NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    source               text NOT NULL DEFAULT 'dummy' CHECK (source IN ('dummy')),
    active               boolean NOT NULL DEFAULT true,
    created_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (connection_id, external_account_ref),
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, connection_id, provider)
        REFERENCES integration_connections (workspace_id, id, provider) ON DELETE RESTRICT
);

CREATE TABLE jira_issues (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id        uuid NOT NULL,
    connection_id       uuid NOT NULL,
    provider            text NOT NULL DEFAULT 'jira' CHECK (provider = 'jira'),
    external_issue_id   text NOT NULL CHECK (length(btrim(external_issue_id)) BETWEEN 1 AND 128),
    issue_key           text NOT NULL CHECK (length(btrim(issue_key)) BETWEEN 1 AND 64),
    safe_url            text CHECK (safe_url ~ '^https://'),
    external_status     text CHECK (length(external_status) <= 100),
    external_updated_at timestamptz,
    created_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (connection_id, external_issue_id),
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, connection_id, provider)
        REFERENCES integration_connections (workspace_id, id, provider) ON DELETE RESTRICT
);

-- Lark chat destinations chosen at intake (ticket-created notification only).
CREATE TABLE lark_destinations (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id      uuid NOT NULL,
    connection_id     uuid NOT NULL,
    provider          text NOT NULL DEFAULT 'lark' CHECK (provider = 'lark'),
    external_chat_ref text NOT NULL CHECK (length(btrim(external_chat_ref)) BETWEEN 1 AND 128),
    display_name      text NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    source            text NOT NULL DEFAULT 'dummy' CHECK (source IN ('dummy')),
    active            boolean NOT NULL DEFAULT true,
    created_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (connection_id, external_chat_ref),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, connection_id, id),
    FOREIGN KEY (workspace_id, connection_id, provider)
        REFERENCES integration_connections (workspace_id, id, provider) ON DELETE RESTRICT
);

-- ---------- related work ----------
-- Local completion is separate from the Jira external_status: Jira "Done" never
-- completes it or resolves the ticket by itself.
CREATE TABLE related_work (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id        uuid NOT NULL,
    ticket_id           uuid NOT NULL,
    title               text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 300),
    safe_description    text CHECK (length(safe_description) <= 5000),
    blocking            boolean NOT NULL DEFAULT false,
    requested_by        uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    jira_issue_id       uuid,
    jira_assignee_id    uuid,
    completion_state    text NOT NULL DEFAULT 'open' CHECK (completion_state IN ('open', 'completed')),
    completion_summary  text CHECK (length(btrim(completion_summary)) BETWEEN 1 AND 5000),
    completion_evidence text CHECK (length(btrim(completion_evidence)) BETWEEN 1 AND 5000),
    completed_by        uuid REFERENCES users (id) ON DELETE RESTRICT,
    completed_at        timestamptz,
    version             integer NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    -- Completed requires summary + evidence + actor + time; open carries none of them.
    CHECK (
        (completion_state = 'open' AND completion_summary IS NULL AND completion_evidence IS NULL
            AND completed_by IS NULL AND completed_at IS NULL)
        OR (completion_state = 'completed' AND completion_summary IS NOT NULL
            AND completion_evidence IS NOT NULL AND completed_by IS NOT NULL
            AND completed_at IS NOT NULL)
    ),
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, ticket_id) REFERENCES tickets (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, jira_issue_id) REFERENCES jira_issues (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, jira_assignee_id) REFERENCES jira_assignees (workspace_id, id) ON DELETE RESTRICT
);
CREATE INDEX related_work_ticket_idx ON related_work (workspace_id, ticket_id);

-- ---------- outbox (ticket + intent written in one transaction) ----------
-- Jira and Lark intents are separate rows with separate states. Outbox state never
-- touches the ticket's workflow_status.
CREATE TABLE integration_outbox (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id        uuid NOT NULL,
    ticket_id           uuid NOT NULL,
    connection_id       uuid NOT NULL,
    provider            text NOT NULL CHECK (provider IN ('jira', 'lark')),
    operation           text NOT NULL,
    related_work_id     uuid,
    lark_destination_id uuid,
    idempotency_key     text NOT NULL CHECK (length(idempotency_key) BETWEEN 8 AND 200),
    state               text NOT NULL DEFAULT 'pending'
                            CHECK (state IN ('pending', 'success', 'failed', 'unknown')),
    attempt             integer NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    next_attempt_at     timestamptz,
    safe_error          text CHECK (length(safe_error) <= 1000),
    reconcile_evidence  text CHECK (length(btrim(reconcile_evidence)) BETWEEN 1 AND 2000),
    correlation_id      text CHECK (length(correlation_id) <= 100),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (provider = 'jira' AND operation IN ('jira_create_issue', 'jira_link_issue')
            AND related_work_id IS NOT NULL AND lark_destination_id IS NULL)
        OR (provider = 'lark' AND operation = 'lark_ticket_created'
            AND lark_destination_id IS NOT NULL AND related_work_id IS NULL)
    ),
    UNIQUE (workspace_id, idempotency_key),
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, ticket_id) REFERENCES tickets (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, connection_id, provider)
        REFERENCES integration_connections (workspace_id, id, provider) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, related_work_id) REFERENCES related_work (workspace_id, id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, connection_id, lark_destination_id)
        REFERENCES lark_destinations (workspace_id, connection_id, id) ON DELETE RESTRICT
);
CREATE INDEX integration_outbox_due_idx ON integration_outbox (next_attempt_at) WHERE state = 'pending';
CREATE INDEX integration_outbox_ticket_idx ON integration_outbox (workspace_id, ticket_id);

-- ---------- attachments (quarantine first, bind only when clean) ----------
CREATE TABLE attachments (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id   uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    ticket_id      uuid,
    uploaded_by    uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    object_key     text NOT NULL UNIQUE CHECK (length(object_key) BETWEEN 1 AND 300),
    file_name      text NOT NULL CHECK (length(btrim(file_name)) BETWEEN 1 AND 255),
    inspected_mime text CHECK (length(inspected_mime) <= 100),
    size_bytes     bigint NOT NULL CHECK (size_bytes > 0),
    sha256         bytea CHECK (octet_length(sha256) = 32),
    scan_status    text NOT NULL DEFAULT 'pending' CHECK (scan_status IN
                       ('pending', 'clean', 'infected', 'scanner_unavailable', 'rejected')),
    visibility     text NOT NULL DEFAULT 'internal' CHECK (visibility IN ('public', 'internal')),
    created_at     timestamptz NOT NULL DEFAULT now(),
    bound_at       timestamptz,
    -- Bound to a ticket only after a clean scan.
    CHECK ((ticket_id IS NULL AND bound_at IS NULL)
        OR (ticket_id IS NOT NULL AND bound_at IS NOT NULL AND scan_status = 'clean')),
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id, ticket_id) REFERENCES tickets (workspace_id, id) ON DELETE RESTRICT
);
CREATE INDEX attachments_ticket_idx ON attachments (workspace_id, ticket_id) WHERE ticket_id IS NOT NULL;

-- ---------- reports (operational only; official BI export held) ----------
CREATE TABLE report_runs (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id    uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    requested_by    uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    report_type     text NOT NULL CHECK (report_type IN ('operational')),
    period_start    date NOT NULL,
    period_end      date NOT NULL,
    filters         jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(filters) = 'object'),
    mapping_version text CHECK (length(mapping_version) <= 50),
    state           text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'succeeded', 'failed')),
    generated_at    timestamptz,
    artifact_ref    text CHECK (length(artifact_ref) BETWEEN 1 AND 300),
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (period_end >= period_start),
    CHECK ((state = 'succeeded') = (artifact_ref IS NOT NULL AND generated_at IS NOT NULL)),
    UNIQUE (workspace_id, id)
);
CREATE INDEX report_runs_ws_idx ON report_runs (workspace_id, created_at DESC);

-- ---------- audit (append-only, no secrets) ----------
CREATE TABLE audit_events (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id   uuid NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    actor_id       uuid REFERENCES users (id) ON DELETE RESTRICT,
    action         text NOT NULL CHECK (action ~ '^[a-z][a-z0-9_.]{1,79}$'),
    entity_type    text NOT NULL CHECK (entity_type ~ '^[a-z][a-z0-9_]{1,39}$'),
    entity_id      uuid,
    safe_change    jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(safe_change) = 'object'),
    outcome        text NOT NULL CHECK (outcome IN ('success', 'denied', 'failed')),
    occurred_at    timestamptz NOT NULL DEFAULT now(),
    correlation_id text CHECK (length(correlation_id) <= 100),
    UNIQUE (workspace_id, id)
);
CREATE INDEX audit_events_ws_idx ON audit_events (workspace_id, occurred_at DESC);

-- ---------- ticket lifecycle guards ----------

CREATE FUNCTION itsm_ticket_owner_active() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.owner_user_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM workspace_memberships m JOIN users u ON u.id = m.user_id
        WHERE m.workspace_id = NEW.workspace_id AND m.user_id = NEW.owner_user_id
          AND m.active AND u.active
    ) THEN
        RAISE EXCEPTION 'PIC harus anggota aktif workspace yang sama' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION itsm_ticket_before_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.workflow_status IN ('resolved', 'closed') THEN
        RAISE EXCEPTION 'tiket baru tidak boleh berstatus %', NEW.workflow_status
            USING ERRCODE = 'IT422';
    END IF;
    IF NEW.cycle_no <> 1 OR NEW.version <> 1 THEN
        RAISE EXCEPTION 'tiket baru harus cycle 1 dan version 1' USING ERRCODE = 'IT422';
    END IF;
    UPDATE workspace_ticket_counters SET last_number = last_number + 1
    WHERE workspace_id = NEW.workspace_id
    RETURNING last_number INTO NEW.number;
    IF NEW.number IS NULL THEN
        RAISE EXCEPTION 'counter nomor tiket workspace tidak ada' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION itsm_ticket_before_update() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.number IS DISTINCT FROM OLD.number
       OR NEW.created_by IS DISTINCT FROM OLD.created_by
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.create_idempotency_key IS DISTINCT FROM OLD.create_idempotency_key THEN
        RAISE EXCEPTION 'nomor/pembuat tiket tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    IF OLD.workflow_status = 'closed' THEN
        RAISE EXCEPTION 'tiket closed historis hanya arsip baca' USING ERRCODE = 'IT422';
    END IF;
    IF NEW.workflow_status = 'closed' THEN
        RAISE EXCEPTION 'close final belum tersedia' USING ERRCODE = 'IT422';
    END IF;

    IF OLD.workflow_status = 'resolved' AND NEW.workflow_status <> 'resolved' THEN
        -- Leaving resolved is only a reopen to 'open' that starts the next cycle.
        IF NEW.workflow_status <> 'open' OR NEW.cycle_no <> OLD.cycle_no + 1 THEN
            RAISE EXCEPTION 'dari resolved hanya reopen ke open dengan cycle berikutnya'
                USING ERRCODE = 'IT422';
        END IF;
        IF NOT EXISTS (
            SELECT 1 FROM ticket_resolution_events
            WHERE ticket_id = NEW.id AND cycle_no = NEW.cycle_no AND action = 'reopen'
        ) THEN
            RAISE EXCEPTION 'reopen wajib alasan tercatat' USING ERRCODE = 'IT422';
        END IF;
    ELSIF NEW.cycle_no <> OLD.cycle_no THEN
        RAISE EXCEPTION 'cycle hanya berubah saat reopen' USING ERRCODE = 'IT422';
    END IF;

    IF NEW.workflow_status = 'resolved' AND OLD.workflow_status <> 'resolved' THEN
        IF NOT EXISTS (
            SELECT 1 FROM ticket_resolution_events
            WHERE ticket_id = NEW.id AND cycle_no = NEW.cycle_no AND action = 'resolve'
        ) THEN
            RAISE EXCEPTION 'resolve wajib ringkasan penyelesaian' USING ERRCODE = 'IT422';
        END IF;
        IF EXISTS (
            SELECT 1 FROM related_work
            WHERE workspace_id = NEW.workspace_id AND ticket_id = NEW.id
              AND blocking AND completion_state = 'open'
        ) THEN
            RAISE EXCEPTION 'masih ada pekerjaan terkait blocking yang belum selesai'
                USING ERRCODE = 'IT422';
        END IF;
    END IF;

    NEW.updated_at := now();
    RETURN NEW;
END $$;

CREATE FUNCTION itsm_resolution_event_before_insert() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    t_status text;
    t_cycle  integer;
BEGIN
    SELECT workflow_status, cycle_no INTO t_status, t_cycle
    FROM tickets WHERE workspace_id = NEW.workspace_id AND id = NEW.ticket_id
    FOR UPDATE;
    IF NEW.action = 'resolve' AND (NEW.cycle_no <> t_cycle OR t_status IN ('resolved', 'closed')) THEN
        RAISE EXCEPTION 'event resolve harus untuk cycle aktif tiket yang belum resolved'
            USING ERRCODE = 'IT422';
    END IF;
    IF NEW.action = 'reopen' AND (t_status <> 'resolved' OR NEW.cycle_no <> t_cycle + 1) THEN
        RAISE EXCEPTION 'event reopen hanya untuk tiket resolved, cycle berikutnya'
            USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER tickets_before_insert BEFORE INSERT ON tickets
    FOR EACH ROW EXECUTE FUNCTION itsm_ticket_before_insert();
CREATE TRIGGER tickets_owner_insert BEFORE INSERT ON tickets
    FOR EACH ROW EXECUTE FUNCTION itsm_ticket_owner_active();
CREATE TRIGGER tickets_owner_update BEFORE UPDATE OF owner_user_id ON tickets
    FOR EACH ROW WHEN (NEW.owner_user_id IS DISTINCT FROM OLD.owner_user_id)
    EXECUTE FUNCTION itsm_ticket_owner_active();
CREATE TRIGGER tickets_before_update BEFORE UPDATE ON tickets
    FOR EACH ROW EXECUTE FUNCTION itsm_ticket_before_update();
CREATE TRIGGER tickets_version BEFORE UPDATE ON tickets
    FOR EACH ROW EXECUTE FUNCTION itsm_require_version_bump();
CREATE TRIGGER resolution_events_before_insert BEFORE INSERT ON ticket_resolution_events
    FOR EACH ROW EXECUTE FUNCTION itsm_resolution_event_before_insert();
CREATE TRIGGER resolution_events_append_only BEFORE UPDATE OR DELETE ON ticket_resolution_events
    FOR EACH ROW EXECUTE FUNCTION itsm_append_only();
CREATE TRIGGER audit_events_append_only BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION itsm_append_only();
CREATE TRIGGER related_work_version BEFORE UPDATE ON related_work
    FOR EACH ROW EXECUTE FUNCTION itsm_require_version_bump();

-- ---------- outbox transitions ----------
-- pending → success | failed | unknown; failed → pending (controlled retry);
-- unknown → success | failed only with reconcile evidence (never straight to retry);
-- success is terminal.
CREATE FUNCTION itsm_outbox_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.state <> 'pending' OR NEW.attempt <> 0 THEN
            RAISE EXCEPTION 'outbox baru harus pending, attempt 0' USING ERRCODE = 'IT422';
        END IF;
        RETURN NEW;
    END IF;
    IF (NEW.ticket_id, NEW.connection_id, NEW.provider, NEW.operation, NEW.idempotency_key, NEW.created_at)
       IS DISTINCT FROM (OLD.ticket_id, OLD.connection_id, OLD.provider, OLD.operation, OLD.idempotency_key, OLD.created_at)
       OR NEW.related_work_id IS DISTINCT FROM OLD.related_work_id
       OR NEW.lark_destination_id IS DISTINCT FROM OLD.lark_destination_id THEN
        RAISE EXCEPTION 'identitas intent outbox tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    IF NEW.attempt < OLD.attempt THEN
        RAISE EXCEPTION 'attempt tidak boleh turun' USING ERRCODE = 'IT422';
    END IF;
    IF OLD.state = 'success' THEN
        RAISE EXCEPTION 'outbox success bersifat final' USING ERRCODE = 'IT422';
    END IF;
    IF NEW.state <> OLD.state AND NOT (
        (OLD.state = 'pending' AND NEW.state IN ('success', 'failed', 'unknown'))
        OR (OLD.state = 'failed' AND NEW.state = 'pending')
        OR (OLD.state = 'unknown' AND NEW.state IN ('success', 'failed')
            AND NEW.reconcile_evidence IS NOT NULL)
    ) THEN
        RAISE EXCEPTION 'transisi outbox % -> % ditolak', OLD.state, NEW.state
            USING ERRCODE = 'IT422';
    END IF;
    NEW.updated_at := now();
    RETURN NEW;
END $$;

CREATE TRIGGER integration_outbox_guard BEFORE INSERT OR UPDATE ON integration_outbox
    FOR EACH ROW EXECUTE FUNCTION itsm_outbox_guard();

-- ---------- workspace_id is immutable on every workspace-owned table ----------
CREATE TRIGGER priorities_ws_fixed BEFORE UPDATE ON priorities FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER categories_ws_fixed BEFORE UPDATE ON categories FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER products_ws_fixed BEFORE UPDATE ON products FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER receiving_units_ws_fixed BEFORE UPDATE ON receiving_units FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER regions_ws_fixed BEFORE UPDATE ON regions FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER intake_channels_ws_fixed BEFORE UPDATE ON intake_channels FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER merchants_ws_fixed BEFORE UPDATE ON merchants FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER tickets_ws_fixed BEFORE UPDATE ON tickets FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER integration_connections_ws_fixed BEFORE UPDATE ON integration_connections FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER jira_assignees_ws_fixed BEFORE UPDATE ON jira_assignees FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER jira_issues_ws_fixed BEFORE UPDATE ON jira_issues FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER lark_destinations_ws_fixed BEFORE UPDATE ON lark_destinations FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER related_work_ws_fixed BEFORE UPDATE ON related_work FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER integration_outbox_ws_fixed BEFORE UPDATE ON integration_outbox FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER attachments_ws_fixed BEFORE UPDATE ON attachments FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER report_runs_ws_fixed BEFORE UPDATE ON report_runs FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
