-- Git automation. Deployment Policy: each Service identity's auto-deploy, wait-for-CI
-- and watch paths, outside Working State, so a change takes effect at once. Branches:
-- the head Cloud last observed of each branch, '' once deleted. Check suites: each
-- suite's latest result. Waiting deploys: what a push selected in an Environment
-- while its CI runs, replaced by the branch's next head.

CREATE TABLE config_service_policy (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    service_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    policy TEXT NOT NULL,
    PRIMARY KEY (environment_id, service_id)
);

CREATE TABLE config_branch (
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    branch TEXT NOT NULL,
    head TEXT NOT NULL,
    PRIMARY KEY (organization_id, repository_id, branch)
);

CREATE TABLE config_check_suite (
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    suite BIGINT NOT NULL,
    head TEXT NOT NULL,
    status TEXT NOT NULL,
    conclusion TEXT NOT NULL,
    updated TEXT NOT NULL,
    PRIMARY KEY (organization_id, repository_id, suite)
);

CREATE TABLE config_waiting_deploy (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    repository_id BIGINT NOT NULL,
    branch TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    head TEXT NOT NULL,
    services TEXT NOT NULL,
    PRIMARY KEY (environment_id, repository_id, branch)
);
