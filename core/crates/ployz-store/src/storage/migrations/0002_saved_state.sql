-- Saved State: one immutable document per Saved revision of an Environment,
-- numbered from 1. Publish appends, and nothing rewrites a revision.

CREATE TABLE config_saved (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    revision BIGINT NOT NULL,
    organization_id TEXT NOT NULL,
    intent TEXT NOT NULL,
    PRIMARY KEY (environment_id, revision)
)
