-- What each build left for a later Deployment of its commit: the receipt of the image it
-- built or reused, and, when GitHub built it under a rule, the image token naming that
-- image, with the rule generation and commit authority its audience names. A redeploy
-- copies both, so it may run that image again under a person's approval of its commit.
ALTER TABLE config_build ADD COLUMN receipt TEXT;
ALTER TABLE config_build ADD COLUMN image_token TEXT;
-- A person's approval of this build's commit, as the JSON of a Signed<CommitApproval>.
-- Each Server checks it in place of the branch head: the grant Machine before it opens
-- the rule's build secrets, and every Server before it runs the image.
ALTER TABLE config_build ADD COLUMN commit_approval TEXT;
-- 1 while this Deployment waits, queued, for a person to approve its commits again once
-- its builds ended, so the approvals the Servers check are fresh.
ALTER TABLE config_deployment ADD COLUMN awaits_approval BIGINT NOT NULL DEFAULT 0;
