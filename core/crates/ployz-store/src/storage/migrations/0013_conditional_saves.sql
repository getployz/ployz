-- Conditional Saves: a PR Environment's picked changes for one Destination
-- (`environment_id`), waiting to go live with its pull request's merge. `standing`
-- while the PR Environment and its target branch are as saved; `frozen` at the
-- merge, with its merge commit, no longer tied to the PR Environment; `landed` with
-- only the rows the Destination had changed too, marked staged or hint, until its
-- next Saved revision. `saved` holds the rows, core's picks and what landing needs,
-- sealed values included. A deploy waiting for CI carries the frozen saves its
-- head lands.

CREATE TABLE config_conditional_save (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    state TEXT NOT NULL,
    pr_environment_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    target_branch TEXT NOT NULL,
    working_revision BIGINT NOT NULL,
    merge_commit TEXT NOT NULL,
    saved_at BIGINT NOT NULL,
    saved TEXT NOT NULL
);

ALTER TABLE config_waiting_deploy ADD COLUMN saves TEXT NOT NULL DEFAULT '[]'
