-- A removal the Store owns: written by `remove`, read by `due` and the sweep.
-- Both target columns go NULL once the target is purged, so "exactly one" holds
-- only while the removal is live.
CREATE TABLE config_removal_mark (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    environment_id TEXT REFERENCES config_environment (id) ON DELETE SET NULL,
    project_id TEXT REFERENCES config_project (id) ON DELETE SET NULL,
    target_name TEXT NOT NULL,
    by TEXT NOT NULL,
    at BIGINT NOT NULL,
    retried BIGINT NOT NULL,
    state TEXT NOT NULL,
    accepted TEXT NOT NULL,
    members TEXT NOT NULL,
    evidence TEXT NOT NULL,
    residuals TEXT NOT NULL,
    CHECK ((environment_id IS NULL) <> (project_id IS NULL) OR state <> 'removing')
);

CREATE UNIQUE INDEX config_removal_mark_environment
    ON config_removal_mark (environment_id) WHERE state <> 'removed';
CREATE UNIQUE INDEX config_removal_mark_project
    ON config_removal_mark (project_id) WHERE state <> 'removed';

ALTER TABLE config_environment_branch DROP COLUMN closing;
