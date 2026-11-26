-- A Store from before 0055 still writes the old credentials during a rollout. It may
-- still read them and freeze none, but writing one back would leave a token Cloud reads.
CREATE TRIGGER config_registry_credential_sealed_only_insert
BEFORE INSERT ON config_registry_credential
BEGIN
    SELECT RAISE(ABORT, 'registry credentials are sealed on your device: update Ployz');
END;
CREATE TRIGGER config_registry_credential_sealed_only_update
BEFORE UPDATE ON config_registry_credential
BEGIN
    SELECT RAISE(ABORT, 'registry credentials are sealed on your device: update Ployz');
END;
CREATE TRIGGER config_deployment_no_credentials_insert
BEFORE INSERT ON config_deployment
WHEN NEW.credentials <> '{}'
BEGIN
    SELECT RAISE(ABORT, 'registry credentials are sealed on your device: update Ployz');
END;
CREATE TRIGGER config_deployment_no_credentials_update
BEFORE UPDATE OF credentials ON config_deployment
WHEN NEW.credentials <> '{}'
BEGIN
    SELECT RAISE(ABORT, 'registry credentials are sealed on your device: update Ployz');
END;
