-- The SQLite side of 0020: the Postgres script says what each change is for.
-- SQLite cannot drop NOT NULL, so config_deployment is rebuilt with `number` nullable
-- and the push columns, the documented way: new table, copy, drop, rename. Foreign
-- keys are off for the migration, so the drop cascades into no child table, and the
-- children's references keep naming config_deployment.
CREATE TABLE new_config_deployment (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    number BIGINT,
    status TEXT NOT NULL,
    saved_revision BIGINT NOT NULL,
    services TEXT NOT NULL,
    nodes TEXT NOT NULL,
    namespace TEXT NOT NULL,
    run TEXT NOT NULL,
    credentials TEXT NOT NULL,
    upload TEXT,
    cluster_domain TEXT,
    admitted BIGINT NOT NULL,
    admitted_by TEXT,
    runner TEXT,
    lease BIGINT NOT NULL DEFAULT 0,
    started BIGINT,
    ended BIGINT,
    message TEXT,
    push_repository_id BIGINT,
    push_branch TEXT,
    push_head TEXT,
    push_services TEXT,
    UNIQUE (environment_id, number)
);
INSERT INTO new_config_deployment
    (id, organization_id, environment_id, number, status, saved_revision, services, nodes,
     namespace, run, credentials, upload, cluster_domain, admitted, admitted_by, runner, lease,
     started, ended, message)
SELECT id, organization_id, environment_id, number, status, saved_revision, services, nodes,
     namespace, run, credentials, upload, cluster_domain, admitted, admitted_by, runner, lease,
     started, ended, message
FROM config_deployment;
DROP TABLE config_deployment;
ALTER TABLE new_config_deployment RENAME TO config_deployment;
CREATE UNIQUE INDEX config_deployment_hold_branch
    ON config_deployment (environment_id, push_repository_id, push_branch)
    WHERE status = 'waiting_for_ci';
CREATE INDEX config_deployment_hold_head
    ON config_deployment (organization_id, push_repository_id, push_head)
    WHERE status = 'waiting_for_ci';
CREATE INDEX config_check_suite_head ON config_check_suite (organization_id, repository_id, head);
DROP TABLE config_waiting_deploy;
