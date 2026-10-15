-- Whether each pull request's head deploys without a member's approval. NULL needs one.
ALTER TABLE config_pull_request ADD COLUMN standing TEXT;
