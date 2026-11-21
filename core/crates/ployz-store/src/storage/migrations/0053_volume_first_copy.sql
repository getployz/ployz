-- The Server the first Deployment that placed a Managed Volume chose for its first copy.
-- While no Server holds a copy, later Deploys start it only there, so a late step from
-- one Deploy and another Deploy's step never make two first copies. A Volume added again
-- has a new ID and so no row.
CREATE TABLE IF NOT EXISTS config_volume_first_copy (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    volume_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    PRIMARY KEY (environment_id, volume_id)
);
