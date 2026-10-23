-- Organizations whose Config Store accepts values the browser sealed to an Environment's
-- Sealing Key. A row means on. Nothing in the Store inserts one.
CREATE TABLE config_sealed_variables (
    organization_id TEXT PRIMARY KEY
);
