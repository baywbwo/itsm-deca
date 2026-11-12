-- DESTRUCTIVE: drops every stored secret. Take a backup first; after production data exists,
-- roll back by restoring the backup instead.
DROP TABLE encrypted_secrets;
