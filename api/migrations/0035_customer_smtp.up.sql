ALTER TABLE workspaces ADD COLUMN customer_smtp jsonb;
ALTER TABLE workspaces ADD COLUMN customer_smtp_secret uuid REFERENCES encrypted_secrets(id);
