-- The last name of each removed Service Working State references, by lineage (JSON).
ALTER TABLE config_environment ADD COLUMN removed TEXT NOT NULL DEFAULT '{}';
