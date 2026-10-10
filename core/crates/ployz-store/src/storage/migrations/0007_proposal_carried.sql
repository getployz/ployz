-- What each Service a proposal introduced carried in besides its configuration, by
-- Service ID: a digest of its sealed registry credential and its Deployment Policy.
-- Remove compares against it, so a credential or policy set in the destination
-- since is never lost with the Service.
ALTER TABLE config_proposal ADD COLUMN carried TEXT NOT NULL DEFAULT '{}';
