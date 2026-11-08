-- The deploy approval a Device signed for this Deployment, as the JSON of a
-- Signed<DeployApproval>. Its runner hands it to each Server, which checks it before
-- creating a Container. NULL means none was given: a token or automation admitted it.
ALTER TABLE config_deployment ADD COLUMN deploy_approval TEXT;
