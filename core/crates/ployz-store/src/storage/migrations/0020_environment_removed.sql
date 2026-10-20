-- Every Service that left Working State, by lineage: its name when it left and its place among the removals (JSON).
ALTER TABLE config_environment ADD COLUMN removed TEXT NOT NULL DEFAULT '{}';
