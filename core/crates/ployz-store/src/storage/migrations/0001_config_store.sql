-- Projects, their Environments with one Working State document each, immutable
-- Node Introductions, and the caller-minted IDs of every create for replay.
-- Every table carries organization_id for Cloud's change log.

CREATE TABLE config_project (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    name TEXT NOT NULL,
    default_environment_id TEXT NOT NULL,
    UNIQUE (organization_id, name)
);

CREATE TABLE config_environment (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES config_project (id),
    name TEXT NOT NULL,
    working_revision BIGINT NOT NULL,
    working TEXT NOT NULL,
    UNIQUE (project_id, name)
);

CREATE TABLE config_node_introduction (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    node_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    node_type TEXT NOT NULL,
    node TEXT NOT NULL,
    PRIMARY KEY (environment_id, node_id)
);

CREATE TABLE config_create (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    command TEXT NOT NULL,
    written TEXT NOT NULL
);
