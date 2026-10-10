-- Proposals: what a draft included from another Environment, until Save or Deploy.

-- One included source in the draft of `environment_id` (the destination): the
-- source Environment, or the pull request (`repository_id`, `number`) whose preview
-- it is. `source_environment_id` has no foreign key, so a proposal outlives its
-- source; `source_name` names it after. `source_revision` is the source's Working
-- revision when it was last included, `first_sync` and `last_sync` the Syncs that
-- included it first and last.
CREATE TABLE config_proposal (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    source_environment_id TEXT NOT NULL,
    repository_id BIGINT,
    number BIGINT,
    source_name TEXT NOT NULL,
    source_revision BIGINT NOT NULL,
    first_sync TEXT NOT NULL,
    last_sync TEXT NOT NULL,
    UNIQUE (environment_id, source_environment_id),
    UNIQUE (environment_id, id, source_environment_id),
    CHECK ((repository_id IS NULL) = (number IS NULL))
);
CREATE UNIQUE INDEX config_proposal_pull_request
    ON config_proposal (environment_id, repository_id, number)
    WHERE repository_id IS NOT NULL;

-- Arrivals gain their owner: `proposal_id`, the proposal that still owns the row,
-- and `source`, the redacted source cell it accepted. An owned row is a pending
-- Sync with its inverse, and belongs to a proposal of the same source.
CREATE TABLE config_sync_arrival_next (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    other_id TEXT NOT NULL,
    lineage TEXT NOT NULL,
    at TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    how TEXT NOT NULL,
    state TEXT NOT NULL,
    value TEXT NOT NULL,
    prior TEXT,
    was TEXT,
    sync_id TEXT,
    proposal_id TEXT,
    source TEXT,
    PRIMARY KEY (environment_id, other_id, lineage, at),
    CHECK (
        proposal_id IS NULL
        OR (
            how = 'sync' AND state = 'pending' AND prior IS NOT NULL AND was IS NOT NULL
            AND sync_id IS NOT NULL AND source IS NOT NULL
        )
    ),
    FOREIGN KEY (environment_id, proposal_id, other_id)
        REFERENCES config_proposal (environment_id, id, source_environment_id)
        ON UPDATE CASCADE
);
INSERT INTO config_sync_arrival_next (
    environment_id, other_id, lineage, at, organization_id, how, state, value, prior, was,
    sync_id
)
SELECT
    environment_id, other_id, lineage, at, organization_id, how, state, value, prior, was,
    sync_id
FROM config_sync_arrival;
DROP TABLE config_sync_arrival;
ALTER TABLE config_sync_arrival_next RENAME TO config_sync_arrival;

-- A row has at most one live owner.
CREATE UNIQUE INDEX config_sync_arrival_owner
    ON config_sync_arrival (environment_id, lineage, at)
    WHERE proposal_id IS NOT NULL;
