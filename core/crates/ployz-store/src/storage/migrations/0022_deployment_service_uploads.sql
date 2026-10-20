-- Each target Service without a source of its own, by lineage, and the digest of the upload it builds from (JSON).
-- A Deployment carries one upload, but its Services may last have built from different ones. NULL for a Deployment
-- admitted before, whose every such Service builds from `upload`.
ALTER TABLE config_deployment ADD COLUMN service_uploads TEXT;
