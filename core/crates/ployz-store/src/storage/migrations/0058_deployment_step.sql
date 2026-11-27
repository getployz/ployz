-- When each step of a Deployment ran, in Unix milliseconds. `service` is empty for a
-- step of the whole Deploy.
CREATE TABLE config_deployment_step (
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    service TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    started BIGINT NOT NULL,
    ended BIGINT,
    PRIMARY KEY (deployment_id, name, service)
);
