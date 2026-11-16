CREATE TABLE workflow_stages (
 workspace_id uuid NOT NULL REFERENCES workspaces(id),
 code text NOT NULL CHECK(code ~ '^[a-z][a-z0-9_]{0,63}$'),
 label text NOT NULL CHECK(length(btrim(label)) BETWEEN 1 AND 100),
 lifecycle text NOT NULL CHECK(lifecycle IN ('new','open','pending_customer','pending_engineering','resolved','closed')),
 sort_order integer NOT NULL CHECK(sort_order BETWEEN 0 AND 10000),
 active boolean NOT NULL DEFAULT true,
 version integer NOT NULL DEFAULT 1,
 PRIMARY KEY(workspace_id,code)
);
CREATE FUNCTION itsm_seed_workflow_stages() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO workflow_stages(workspace_id,code,label,lifecycle,sort_order)
 SELECT NEW.id,code,label,code,ord FROM (VALUES
 ('new','Baru',10),('open','Ditangani',20),('pending_customer','Menunggu pelanggan',30),
 ('pending_engineering','Menunggu pekerjaan eksternal',40),('resolved','Terselesaikan',50),('closed','Ditutup (arsip)',60)) v(code,label,ord);
 RETURN NEW;
END $$;
CREATE TRIGGER workspace_stages AFTER INSERT ON workspaces FOR EACH ROW EXECUTE FUNCTION itsm_seed_workflow_stages();
INSERT INTO workflow_stages(workspace_id,code,label,lifecycle,sort_order)
 SELECT w.id,v.code,v.label,v.code,v.ord FROM workspaces w CROSS JOIN (VALUES
 ('new','Baru',10),('open','Ditangani',20),('pending_customer','Menunggu pelanggan',30),
 ('pending_engineering','Menunggu pekerjaan eksternal',40),('resolved','Terselesaikan',50),('closed','Ditutup (arsip)',60)) v(code,label,ord);
ALTER TABLE tickets ADD COLUMN stage_code text;
ALTER TABLE tickets ADD CONSTRAINT ticket_stage_fk FOREIGN KEY(workspace_id,stage_code) REFERENCES workflow_stages(workspace_id,code);
CREATE INDEX tickets_stage_idx ON tickets(workspace_id,(coalesce(stage_code,workflow_status)),updated_at DESC,id);
CREATE FUNCTION itsm_ticket_stage_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE stage workflow_stages%ROWTYPE;
BEGIN
 -- Older API callers move by lifecycle. Choose the first active stage in that lifecycle.
 IF TG_OP='UPDATE' AND NEW.workflow_status IS DISTINCT FROM OLD.workflow_status AND NEW.stage_code IS NOT DISTINCT FROM OLD.stage_code THEN
  NEW.stage_code := NULL;
 END IF;
 IF NEW.stage_code IS NULL THEN
  SELECT * INTO stage FROM workflow_stages WHERE workspace_id=NEW.workspace_id AND lifecycle=NEW.workflow_status AND active ORDER BY sort_order,code LIMIT 1;
 ELSE
  SELECT * INTO stage FROM workflow_stages WHERE workspace_id=NEW.workspace_id AND code=NEW.stage_code;
 END IF;
 IF stage.code IS NULL OR stage.lifecycle <> NEW.workflow_status THEN
  RAISE EXCEPTION 'stage tidak sesuai status tiket' USING ERRCODE='IT422';
 END IF;
 IF TG_OP='INSERT' THEN
  IF NOT stage.active THEN RAISE EXCEPTION 'stage tidak aktif' USING ERRCODE='IT422'; END IF;
 ELSIF NEW.stage_code IS DISTINCT FROM OLD.stage_code OR NEW.workflow_status IS DISTINCT FROM OLD.workflow_status THEN
  IF NOT stage.active THEN RAISE EXCEPTION 'stage tidak aktif' USING ERRCODE='IT422'; END IF;
 END IF;
 NEW.stage_code := stage.code;
 RETURN NEW;
END $$;
CREATE TRIGGER tickets_stage_guard BEFORE INSERT OR UPDATE ON tickets FOR EACH ROW EXECUTE FUNCTION itsm_ticket_stage_guard();
SELECT itsm_grant_app();
