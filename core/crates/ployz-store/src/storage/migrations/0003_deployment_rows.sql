CREATE TABLE config_deployment_row (
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    machine TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    server TEXT NOT NULL,
    state TEXT NOT NULL,
    started BIGINT,
    finished BIGINT,
    PRIMARY KEY (deployment_id, service, machine)
);
