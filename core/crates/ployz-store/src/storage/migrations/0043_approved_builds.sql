-- What each build left for a later Deployment of its commit: the receipt of the image it
-- built or reused, and, when GitHub built it under an automatic deploy rule, the image
-- token naming that image, with the rule generation and commit authority its audience
-- names. A redeploy copies both, so a person's client can check the token before
-- approving the image again.
ALTER TABLE config_build ADD COLUMN receipt TEXT;
ALTER TABLE config_build ADD COLUMN image_token TEXT;
-- A person's BuildApproval of this build's commit, as the JSON of a
-- Signed<BuildApproval>: the grant Machine releases the rule's build secrets for a
-- commit that is not its branch's head only on one.
ALTER TABLE config_build ADD COLUMN build_approval TEXT;
-- 1 when a Device signs this Deployment's deploy approval only after it was admitted,
-- once its builds ended: it waits for that approval before it runs.
ALTER TABLE config_deployment ADD COLUMN awaits_approval INTEGER NOT NULL DEFAULT 0;
