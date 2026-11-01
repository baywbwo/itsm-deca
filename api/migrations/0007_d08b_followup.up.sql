-- D08b follow-up (expand only): operational notes, customer conversation records (adapter
-- not connected yet: rows are simulation only), related-work create idempotency and the
-- attachment upload lifecycle (quarantine → scan → bind to the same ticket only when clean).

-- ---------- operational notes (not team chat) ----------
CREATE TABLE ticket_activities (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id      uuid NOT NULL,
    ticket_id         uuid NOT NULL,
    actor_id          uuid NOT NULL REFERENCES users (id) ON DELETE RESTRICT,
    activity_type     text NOT NULL DEFAULT 'note' CHECK (activity_type IN ('note')),
    body              text NOT NULL CHECK (length(btrim(body)) BETWEEN 1 AND 5000),
    idempotency_key   text NOT NULL CHECK (length(idempotency_key) BETWEEN 8 AND 128),
    request_hash      bytea NOT NULL CHECK (octet_length(request_hash) = 32),
    created_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, actor_id, idempotency_key),
    FOREIGN KEY (workspace_id, ticket_id) REFERENCES tickets (workspace_id, id) ON DELETE RESTRICT
);
CREATE INDEX ticket_activities_ticket_idx ON ticket_activities (workspace_id, ticket_id, created_at DESC, id DESC);
CREATE TRIGGER ticket_activities_append_only BEFORE UPDATE OR DELETE ON ticket_activities
    FOR EACH ROW EXECUTE FUNCTION itsm_append_only();

-- ---------- customer conversations (omnichannel adapter not connected) ----------
-- Only labelled simulation rows can exist until a real adapter is approved; there is no
-- write path in the API.
CREATE TABLE conversations (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id       uuid NOT NULL,
    ticket_id          uuid NOT NULL,
    source             text NOT NULL DEFAULT 'simulation' CHECK (source IN ('simulation')),
    channel_label      text NOT NULL CHECK (length(btrim(channel_label)) BETWEEN 1 AND 100),
    external_thread_id text NOT NULL CHECK (length(external_thread_id) BETWEEN 1 AND 200),
    created_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, source, channel_label, external_thread_id),
    FOREIGN KEY (workspace_id, ticket_id) REFERENCES tickets (workspace_id, id) ON DELETE RESTRICT
);
CREATE INDEX conversations_ticket_idx ON conversations (workspace_id, ticket_id);

CREATE TABLE messages (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id        uuid NOT NULL,
    conversation_id     uuid NOT NULL,
    direction           text NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    external_message_id text NOT NULL CHECK (length(external_message_id) BETWEEN 1 AND 200),
    body                text NOT NULL CHECK (length(body) BETWEEN 1 AND 5000),
    -- queued is not delivered; unknown stays unknown until reconciled.
    delivery_state      text NOT NULL CHECK (delivery_state IN
                            ('received', 'queued', 'sent', 'delivered', 'failed', 'unknown')),
    occurred_at         timestamptz NOT NULL,
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, conversation_id, external_message_id),
    CHECK ((direction = 'inbound') = (delivery_state = 'received')),
    FOREIGN KEY (workspace_id, conversation_id) REFERENCES conversations (workspace_id, id) ON DELETE RESTRICT
);
CREATE INDEX messages_conversation_idx ON messages (workspace_id, conversation_id, occurred_at, id);

CREATE TRIGGER conversations_ws_fixed BEFORE UPDATE ON conversations FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();
CREATE TRIGGER messages_ws_fixed BEFORE UPDATE ON messages FOR EACH ROW EXECUTE FUNCTION itsm_forbid_workspace_change();

-- ---------- related work create idempotency ----------
ALTER TABLE related_work
    ADD COLUMN create_idempotency_key text CHECK (length(create_idempotency_key) BETWEEN 8 AND 128),
    ADD COLUMN create_request_hash bytea CHECK (octet_length(create_request_hash) = 32),
    ADD CONSTRAINT related_work_key_has_hash
        CHECK ((create_idempotency_key IS NULL) = (create_request_hash IS NULL));
CREATE UNIQUE INDEX related_work_create_key_uq
    ON related_work (workspace_id, requested_by, create_idempotency_key)
    WHERE create_idempotency_key IS NOT NULL;

CREATE FUNCTION itsm_related_work_key_fixed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.create_idempotency_key IS DISTINCT FROM OLD.create_idempotency_key
       OR NEW.create_request_hash IS DISTINCT FROM OLD.create_request_hash THEN
        RAISE EXCEPTION 'kunci create pekerjaan terkait tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER related_work_key_fixed BEFORE UPDATE ON related_work
    FOR EACH ROW EXECUTE FUNCTION itsm_related_work_key_fixed();

-- ---------- attachment upload lifecycle ----------
-- scan_error = the scanner answered but the scan did not complete; distinct from infected
-- and from scanner_unavailable. None of them is ever bound.
ALTER TABLE attachments DROP CONSTRAINT attachments_scan_status_check;
ALTER TABLE attachments ADD CONSTRAINT attachments_scan_status_check CHECK (scan_status IN
    ('pending', 'clean', 'infected', 'scan_error', 'scanner_unavailable', 'rejected'));
-- The ticket the upload was made for; binding is only allowed to that same ticket.
ALTER TABLE attachments
    ADD COLUMN target_ticket_id uuid,
    ADD CONSTRAINT attachments_target_ticket_fk FOREIGN KEY (workspace_id, target_ticket_id)
        REFERENCES tickets (workspace_id, id) ON DELETE RESTRICT,
    ADD CONSTRAINT attachments_bind_same_ticket
        CHECK (ticket_id IS NULL OR target_ticket_id IS NULL OR ticket_id = target_ticket_id);

CREATE FUNCTION itsm_attachment_guard() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.object_key IS DISTINCT FROM OLD.object_key
       OR NEW.sha256 IS DISTINCT FROM OLD.sha256
       OR NEW.size_bytes IS DISTINCT FROM OLD.size_bytes
       OR NEW.uploaded_by IS DISTINCT FROM OLD.uploaded_by
       OR NEW.target_ticket_id IS DISTINCT FROM OLD.target_ticket_id THEN
        RAISE EXCEPTION 'identitas lampiran tidak boleh diubah' USING ERRCODE = 'IT422';
    END IF;
    -- A scan verdict is final: only pending may change, and a bound file stays bound.
    IF OLD.scan_status <> 'pending' AND NEW.scan_status IS DISTINCT FROM OLD.scan_status THEN
        RAISE EXCEPTION 'hasil pemindaian lampiran sudah final' USING ERRCODE = 'IT422';
    END IF;
    IF OLD.ticket_id IS NOT NULL AND NEW.ticket_id IS DISTINCT FROM OLD.ticket_id THEN
        RAISE EXCEPTION 'lampiran terikat tidak boleh dipindah' USING ERRCODE = 'IT422';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER attachments_guard BEFORE UPDATE ON attachments
    FOR EACH ROW EXECUTE FUNCTION itsm_attachment_guard();
CREATE TRIGGER attachments_no_delete BEFORE DELETE ON attachments
    FOR EACH ROW EXECUTE FUNCTION itsm_append_only();
