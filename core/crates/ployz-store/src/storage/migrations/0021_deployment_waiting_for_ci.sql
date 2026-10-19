-- A push held for its CI is a Deployment waiting for CI, so the push that admitted a
-- Deployment lives on it. NULL for a manual Deploy, a retry or a removal.
ALTER TABLE config_deployment ADD COLUMN push_repository_id BIGINT;
ALTER TABLE config_deployment ADD COLUMN push_branch TEXT;
ALTER TABLE config_deployment ADD COLUMN push_head TEXT;
-- The Services a held push selected, by ID as a JSON array: a name may change while
-- it waits. NULL or empty selects every one.
ALTER TABLE config_deployment ADD COLUMN push_services TEXT;
-- A newer head of the branch replaces the one waiting: one per Environment and branch.
CREATE UNIQUE INDEX config_deployment_hold_branch
    ON config_deployment (environment_id, push_repository_id, push_branch)
    WHERE status = 'waiting_for_ci';
-- A check suite finds the Deployments waiting for its commit.
CREATE INDEX config_deployment_hold_head
    ON config_deployment (organization_id, push_repository_id, push_head)
    WHERE status = 'waiting_for_ci';
-- Every Deployment read says whether a waiting one's CI failed.
CREATE INDEX config_check_suite_head ON config_check_suite (organization_id, repository_id, head);
-- Pushes still waiting at upgrade are dropped, not converted: the next push holds again.
DROP TABLE config_waiting_deploy;
