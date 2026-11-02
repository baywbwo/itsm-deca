-- Rollback of 0007 for UAT data only. Notes, conversations and messages are dropped;
-- attachment rows with the new scan_error status are kept as 'rejected' (never bound).
DROP TRIGGER IF EXISTS attachments_no_delete ON attachments;
DROP TRIGGER IF EXISTS attachments_guard ON attachments;
DROP FUNCTION IF EXISTS itsm_attachment_guard();
ALTER TABLE attachments DROP CONSTRAINT IF EXISTS attachments_bind_same_ticket;
ALTER TABLE attachments DROP CONSTRAINT IF EXISTS attachments_target_ticket_fk;
ALTER TABLE attachments DROP COLUMN IF EXISTS target_ticket_id;
UPDATE attachments SET scan_status = 'rejected' WHERE scan_status = 'scan_error';
ALTER TABLE attachments DROP CONSTRAINT IF EXISTS attachments_scan_status_check;
ALTER TABLE attachments ADD CONSTRAINT attachments_scan_status_check CHECK (scan_status IN
    ('pending', 'clean', 'infected', 'scanner_unavailable', 'rejected'));

DROP TRIGGER IF EXISTS related_work_key_fixed ON related_work;
DROP FUNCTION IF EXISTS itsm_related_work_key_fixed();
DROP INDEX IF EXISTS related_work_create_key_uq;
ALTER TABLE related_work DROP CONSTRAINT IF EXISTS related_work_key_has_hash;
ALTER TABLE related_work DROP COLUMN IF EXISTS create_request_hash;
ALTER TABLE related_work DROP COLUMN IF EXISTS create_idempotency_key;

DROP TABLE IF EXISTS messages;
DROP TABLE IF EXISTS conversations;
DROP TABLE IF EXISTS ticket_activities;
