-- Git builds: each Git Service a Deployment builds, pinned to one commit before
-- its source is read (a pin never moves), with its build's progress and log.

CREATE TABLE config_build (
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id),
    service TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    status TEXT NOT NULL,
    message TEXT NOT NULL,
    log TEXT NOT NULL,
    PRIMARY KEY (deployment_id, service)
);
