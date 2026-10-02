-- Sync: one base per pair of Environments instead of one per Branch.

-- What two Environments last shared, in either order of the pair: a Sync between
-- them compares over it and advances it by what lands.
CREATE TABLE config_sync_base (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    other_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    base TEXT NOT NULL,
    PRIMARY KEY (environment_id, other_id)
);

-- A Branch's base is its pair base with its Parent, and the base it was made with.
INSERT INTO config_sync_base (environment_id, other_id, organization_id, base)
SELECT environment_id, parent_id, organization_id, base FROM config_environment_branch;
ALTER TABLE config_environment_branch RENAME COLUMN base TO made_with;

-- Rows a Sync landed in `environment_id` from `other_id` that aren't deployed yet:
-- `prior` is the node (`lineage`) as their pair base held it before, JSON `null` when
-- it held none. Discarding a row rewinds the pair base to it, so the row is offered
-- again; deploying the node forgets them.
CREATE TABLE config_sync_pending (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    other_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    lineage TEXT NOT NULL,
    path TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    prior TEXT NOT NULL,
    PRIMARY KEY (environment_id, other_id, lineage, path)
);

-- Settings an Environment marked Never sync, by the row key a comparison names
-- them by (`lineage`, `path`): a Sync never carries them from it nor into it, but
-- its Branches still get its value.
CREATE TABLE config_never_sync (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    lineage TEXT NOT NULL,
    path TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    PRIMARY KEY (environment_id, lineage, path)
);

-- Follow: the Parent's value each setting of a Branch (`environment_id`) was last
-- delivered at, as the comparison renders it (`value`, a secret by fingerprint). A
-- Parent's deploy stages a change only once: one the Branch changed too, or
-- discarded, stays a Use hint until the Parent changes that setting again.
CREATE TABLE config_followed (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    lineage TEXT NOT NULL,
    path TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (environment_id, lineage, path)
);

-- Conditional Save is Conditional Sync now.
ALTER TABLE config_conditional_save RENAME TO config_conditional_sync;

-- A Destination's value for a secret a pull request's Conditional Syncs bring by
-- name only (`lineage`, `variable`), set ahead of the merge: `value` is the sealed
-- variable value as JSON. It lands with the Conditional Sync at the merge, survives
-- a withdraw and sync again, and drops when the pull request closes unmerged.
CREATE TABLE config_held_secret (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    lineage TEXT NOT NULL,
    variable TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    value TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    PRIMARY KEY (environment_id, repository_id, number, lineage, variable)
);
