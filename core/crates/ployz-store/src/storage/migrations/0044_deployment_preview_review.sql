-- The review a person signed for this preview Deployment's commit, as the JSON of a
-- Review. Its runner relays it to the grant Machine and to each Server, which check it
-- before anything opens. NULL for every Deployment not reviewed.
ALTER TABLE config_deployment ADD COLUMN preview_review TEXT;
