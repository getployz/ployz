-- Builders: the Organization's Build Order (none is Auto), applied at once, and the
-- GitHub Actions run a Git build was handed to, with its Build Grant, as a JSON
-- document ('' when none). A Service's Preferred Builder is in its Deployment Policy.

CREATE TABLE config_build_order (
    organization_id TEXT PRIMARY KEY,
    build_order TEXT NOT NULL
);

ALTER TABLE config_build ADD COLUMN github TEXT NOT NULL DEFAULT ''
