-- PR Environments. Plans: each Project's PR Environment settings for one GitHub
-- repository its Services deploy from. Pull requests: the latest facts Cloud read
-- of each, ordered by GitHub's updated_at so an older read never replaces a newer.
-- PR Environments: the Branch the Store made for one pull request. A Branch the
-- Store is closing (its pull request closed, or it sat idle) is removed from the
-- Servers, then deleted. A Deployment records when it was admitted, which the idle
-- rule reads.

CREATE TABLE config_pr_plan (
    project_id TEXT NOT NULL REFERENCES config_project (id),
    repository_id BIGINT NOT NULL,
    organization_id TEXT NOT NULL,
    repository TEXT NOT NULL,
    plan TEXT NOT NULL,
    PRIMARY KEY (project_id, repository_id)
);

CREATE TABLE config_pull_request (
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL,
    facts TEXT NOT NULL,
    updated TEXT NOT NULL,
    PRIMARY KEY (organization_id, repository_id, number)
);

CREATE TABLE config_pr_environment (
    environment_id TEXT PRIMARY KEY REFERENCES config_environment (id),
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    number BIGINT NOT NULL
);

ALTER TABLE config_environment_branch ADD COLUMN closing BIGINT NOT NULL DEFAULT 0;

ALTER TABLE config_deployment ADD COLUMN admitted BIGINT NOT NULL DEFAULT 0
