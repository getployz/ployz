-- Sync: one base per pair of Environments instead of one per Branch.

-- What two Environments last shared, keyed by the pair's IDs in order
-- (`environment_id` < `other_id`): a Sync between them compares over it and
-- advances it by what lands.
CREATE TABLE config_sync_base (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    other_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    base TEXT NOT NULL,
    PRIMARY KEY (environment_id, other_id)
);

-- A Branch's base is its pair base with its Parent, and the base it was made with.
-- ponytail: IDs are lowercase UUIDs, so SQL and Rust order them alike.
INSERT INTO config_sync_base (environment_id, other_id, organization_id, base)
SELECT
    CASE WHEN environment_id < parent_id THEN environment_id ELSE parent_id END,
    CASE WHEN environment_id < parent_id THEN parent_id ELSE environment_id END,
    organization_id,
    base
FROM config_environment_branch;
ALTER TABLE config_environment_branch RENAME COLUMN base TO made_with;

-- What landed in `environment_id` from `other_id`, by row (`lineage`, `at`): `how`
-- it came (`follow` or `sync`), and its `state`: `pending` (staged, not deployed),
-- `hint` (a Follow the receiver changed too, or discarded) or `settled` (deployed).
-- `value` is the cell delivered, redacted; `prior` the pair base's cell before and
-- `was` the receiver's own (sealed), kept while pending to rewind a discard and to
-- undo the Sync (`sync_id`) that landed it. `other_id` outlives its Environment, so
-- a Sync that closed its Branch can still be undone.
-- ponytail: rows from a removed Environment stay once settled; prune them if this grows.
CREATE TABLE config_sync_arrival (
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
    PRIMARY KEY (environment_id, other_id, lineage, at)
);

-- Rows an Environment marked Never sync (`lineage`, `at`): a Sync never carries
-- them from it nor into it, but its Branches still get its value.
CREATE TABLE config_never_sync (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    lineage TEXT NOT NULL,
    at TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    PRIMARY KEY (environment_id, lineage, at)
);

-- Conditional Save is Conditional Sync now.
ALTER TABLE config_conditional_save RENAME TO config_conditional_sync;

-- A Destination's value for a secret a pull request's Conditional Syncs bring by
-- name only (the row `lineage`, `at`), set ahead of the merge: `value` is the sealed
-- cell as JSON. It lands with the Conditional Sync at the merge, survives
-- a withdraw and sync again, and drops when the pull request closes unmerged.
CREATE TABLE config_held_secret (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    lineage TEXT NOT NULL,
    at TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (environment_id, repository_id, number, lineage, at)
);
