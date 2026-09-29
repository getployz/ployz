-- Deployments: each admitted attempt, frozen at admission, with its runner's
-- evidence in the run document. Applied State: each node as its latest confirmed
-- Deployment applied it. Namespaces: the runtime Namespace each Environment
-- deploys into, fixed at its first admission and unique in its Organization.

CREATE TABLE config_namespace (
    organization_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    environment_id TEXT NOT NULL UNIQUE REFERENCES config_environment (id),
    PRIMARY KEY (organization_id, namespace)
);

CREATE TABLE config_deployment (
    id TEXT PRIMARY KEY,
    organization_id TEXT NOT NULL,
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    number BIGINT NOT NULL,
    status TEXT NOT NULL,
    saved_revision BIGINT NOT NULL,
    services TEXT NOT NULL,
    nodes TEXT NOT NULL,
    namespace TEXT NOT NULL,
    run TEXT NOT NULL,
    UNIQUE (environment_id, number)
);

CREATE TABLE config_applied (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    node_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id),
    node TEXT NOT NULL,
    PRIMARY KEY (environment_id, node_id)
)
