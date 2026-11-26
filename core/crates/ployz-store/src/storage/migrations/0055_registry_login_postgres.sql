-- A Store from before 0055 still writes the old credentials during a rollout. It may
-- still read them and freeze none, but writing one back would leave a token Cloud reads.
ALTER TABLE config_registry_credential
    ADD CONSTRAINT config_registry_credential_sealed_only CHECK (false);
ALTER TABLE config_deployment
    ADD CONSTRAINT config_deployment_no_credentials CHECK (credentials = '{}');
