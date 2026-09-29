-- Registry credentials: each Service identity owns one, sealed, outside Working
-- State, so rotating it takes effect at once. Admission freezes the credentials a
-- Deployment pulls with, so later rotation never changes what it runs.

CREATE TABLE config_registry_credential (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    service_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    credential TEXT NOT NULL,
    PRIMARY KEY (environment_id, service_id)
);

ALTER TABLE config_deployment ADD COLUMN credentials TEXT NOT NULL DEFAULT '{}'
