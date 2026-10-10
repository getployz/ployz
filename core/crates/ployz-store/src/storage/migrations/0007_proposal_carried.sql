-- What a proposal's Syncs put outside Working State, by Service ID: for each Service it
-- introduced, a digest of its sealed registry credential and its Deployment Policy;
-- and the digest of each credential it wrote over a Service already there. Remove
-- compares against it, so a credential or policy set here is never lost with it.
ALTER TABLE config_proposal ADD COLUMN carried TEXT NOT NULL DEFAULT '{}';
