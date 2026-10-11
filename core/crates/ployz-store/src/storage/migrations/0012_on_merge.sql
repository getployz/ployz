-- A pull request's On merge list is derived; only a person's Edits are stored: the
-- value a row lands with in one Destination in place of the PR Environment's.
CREATE TABLE config_merge_value (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    destination_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    lineage TEXT NOT NULL,
    at TEXT NOT NULL,
    cell TEXT NOT NULL,
    PRIMARY KEY (environment_id, destination_id, lineage, at)
);

-- The Services a merge landed in an Environment, deployed with the next push.
CREATE TABLE config_merge_deploy (
    environment_id TEXT PRIMARY KEY REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    branch TEXT NOT NULL,
    merge_commit TEXT NOT NULL,
    services TEXT NOT NULL
);
