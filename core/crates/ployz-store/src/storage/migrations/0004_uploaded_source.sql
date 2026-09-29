-- Uploaded Source: the content digest and base commit a Deployment's Services
-- without a source of their own build from. Build receipts: the latest image each
-- Service of an Environment built, a hint the next preparation verifies.

ALTER TABLE config_deployment ADD COLUMN upload TEXT NOT NULL DEFAULT 'null';

CREATE TABLE config_build_receipt (
    environment_id TEXT NOT NULL REFERENCES config_environment (id),
    service TEXT NOT NULL,
    receipt TEXT NOT NULL,
    PRIMARY KEY (environment_id, service)
);
