-- Contract of 0013: an older binary has no branding; dropping the two columns only removes the
-- display preset. Company rows, workspaces, tickets and audit history are untouched.
ALTER TABLE companies DROP COLUMN IF EXISTS brand_name;
ALTER TABLE companies DROP COLUMN IF EXISTS brand_key;
