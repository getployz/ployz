-- Branches: an Environment made from a Parent in the same Project. Its base is
-- what it and its Parent last shared (one Saved-State-shaped document), kept means
-- it outlives a Save, and each Setup Command runs in the Own Copy of one Service
-- lineage before that copy first deploys.

CREATE TABLE config_environment_branch (
    environment_id TEXT PRIMARY KEY REFERENCES config_environment (id),
    organization_id TEXT NOT NULL,
    parent_id TEXT NOT NULL REFERENCES config_environment (id),
    kept BIGINT NOT NULL,
    base TEXT NOT NULL,
    setup TEXT NOT NULL
)
