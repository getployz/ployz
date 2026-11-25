-- A registry login reaches the Store only sealed on a person's device for one registry
-- host, which only the Cluster's Servers open. The credentials the Store sealed with its
-- own key go, with the copy each Deployment froze. Their table and column stay, empty,
-- so a Store from before this still deploys public images during a rollout; a later
-- release drops them.
DELETE FROM config_registry_credential;
UPDATE config_deployment SET credentials = '{}';
CREATE TABLE config_registry_login (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    service_id TEXT NOT NULL,
    organization_id TEXT NOT NULL,
    login TEXT NOT NULL,
    PRIMARY KEY (environment_id, service_id)
);
ALTER TABLE config_deployment ADD COLUMN logins TEXT NOT NULL DEFAULT '{}';
