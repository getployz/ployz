-- A pull request's On merge list is derived; only a person's Edits are stored: the
-- value a row lands with in one Destination in place of the PR Environment's.
CREATE TABLE config_merge_value (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    destination_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    lineage TEXT NOT NULL,
    at TEXT NOT NULL,
    cell TEXT NOT NULL,
    PRIMARY KEY (environment_id, destination_id, lineage, at)
);

-- The Services merges landed in an Environment, deployed with the first push of
-- `branch` that contains them; kept until a Deployment takes them.
CREATE TABLE config_merge_deploy (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    organization_id TEXT NOT NULL,
    repository_id BIGINT NOT NULL,
    branch TEXT NOT NULL,
    services TEXT NOT NULL,
    PRIMARY KEY (environment_id, repository_id, branch)
);

-- The heads a branch had lately, newest last, as a JSON array: a merge that arrives
-- after its commit was pushed deploys at once.
ALTER TABLE config_branch ADD COLUMN recent TEXT NOT NULL DEFAULT '[]';

-- What a merge couldn't do, written once when it lands: a JSON array of
-- {"into":<Destination>,"saved":<whether it landed>,"reason":<why>}.
ALTER TABLE config_pull_request ADD COLUMN unlanded TEXT;
