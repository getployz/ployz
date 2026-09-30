-- Each Volume's storage, fixed when a Deployment first targets it: storage may be
-- prepared even by an attempt that never applies.
CREATE TABLE config_volume_storage (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    volume_id TEXT NOT NULL,
    storage TEXT NOT NULL,
    PRIMARY KEY (environment_id, volume_id)
);
