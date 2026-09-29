-- The Cluster Domain a Deployment's generated domains expand under, frozen at
-- admission so its runner ships exactly what was admitted. Empty when it has none.

ALTER TABLE config_deployment ADD COLUMN cluster_domain TEXT NOT NULL DEFAULT ''
