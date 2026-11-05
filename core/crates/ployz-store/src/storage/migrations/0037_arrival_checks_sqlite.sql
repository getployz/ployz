-- Arrivals hold only the states and columns their readers read: a state and `how` in
-- their sets, a pending row's `prior` and `was`, none of `prior`, `was` or an owner
-- once it is a Use hint or settled, and a Use hint a Follow's. The step that runs this
-- repaired first what it could without inventing a value.
--
-- SQLite adds a CHECK only by rebuilding the table, as 0005 did: every column, the
-- primary key, 0005's CHECK, the foreign key and the owner index, and five CHECKs.
-- The copy skips the CHECKs, so a row the repair couldn't fix stays as it was rather
-- than failing every open of the Store. From here on an INSERT checks all five, and an
-- UPDATE those that read a column it sets. A later migration that copies the rows
-- without this pragma, or updates the whole table setting a column one of them reads,
-- meets the rows these never checked.
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
    CONSTRAINT config_sync_arrival_state
        CHECK (state IN ('pending', 'hint', 'settled')),
    CONSTRAINT config_sync_arrival_how
        CHECK (how IN ('follow', 'sync')),
    CONSTRAINT config_sync_arrival_pending
        CHECK (state <> 'pending' OR (prior IS NOT NULL AND was IS NOT NULL)),
    CONSTRAINT config_sync_arrival_done
        CHECK (
            state = 'pending' OR (prior IS NULL AND was IS NULL AND proposal_id IS NULL)
        ),
    CONSTRAINT config_sync_arrival_hint
        CHECK (state <> 'hint' OR how = 'follow'),
    FOREIGN KEY (environment_id, proposal_id, other_id)
        REFERENCES config_proposal (environment_id, id, source_environment_id)
        ON UPDATE CASCADE
);
PRAGMA ignore_check_constraints = ON;
INSERT INTO config_sync_arrival_next (
    environment_id, other_id, lineage, at, organization_id, how, state, value, prior, was,
    sync_id, proposal_id, source
)
SELECT
    environment_id, other_id, lineage, at, organization_id, how, state, value, prior, was,
    sync_id, proposal_id, source
FROM config_sync_arrival;
PRAGMA ignore_check_constraints = OFF;
DROP TABLE config_sync_arrival;
ALTER TABLE config_sync_arrival_next RENAME TO config_sync_arrival;

-- A row has at most one live owner.
CREATE UNIQUE INDEX config_sync_arrival_owner
    ON config_sync_arrival (environment_id, lineage, at)
    WHERE proposal_id IS NOT NULL;
