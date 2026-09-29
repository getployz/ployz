-- Builders: the Organization's Build Order (none is Auto), each Service's Preferred
-- Builder (none is Auto), both applied at once, and the GitHub Actions run a Git
-- build was handed to, with its Build Grant, as a JSON document ('' when none).

CREATE TABLE config_build_order (
    organization_id TEXT PRIMARY KEY,
    build_order TEXT NOT NULL
);

CREATE TABLE config_preferred_builder (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    service_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    builder TEXT NOT NULL,
    PRIMARY KEY (environment_id, service_id)
);

ALTER TABLE config_build ADD COLUMN github TEXT NOT NULL DEFAULT ''
