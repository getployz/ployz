-- Which variables' values each save changed, as `[service_id, variable_id]` pairs and
-- never the values: sealing later rewrites older versions to the current ciphertext,
-- so History reads from here that a version changed one.
ALTER TABLE config_saved ADD COLUMN value_changes TEXT;
