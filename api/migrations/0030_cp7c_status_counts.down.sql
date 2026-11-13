DROP INDEX IF EXISTS tickets_active_priority_idx;
DROP TRIGGER IF EXISTS tickets_status_count_upd ON tickets;
DROP TRIGGER IF EXISTS tickets_status_count_del ON tickets;
DROP TRIGGER IF EXISTS tickets_status_count_ins ON tickets;
DROP FUNCTION IF EXISTS itsm_ticket_status_count();
DROP TABLE IF EXISTS ticket_status_counts;
