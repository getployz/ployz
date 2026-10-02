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
