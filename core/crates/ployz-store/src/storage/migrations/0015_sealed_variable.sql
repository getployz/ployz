-- Every variable an Environment's Working State held sealed, recorded at the write
-- that sealed it: History never brings plain text back for one, even when the
-- seal never reached a save.
CREATE TABLE config_sealed_variable (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    service_id TEXT NOT NULL,
    variable_id TEXT NOT NULL,
    key TEXT NOT NULL,
    PRIMARY KEY (environment_id, service_id, variable_id, key)
);
