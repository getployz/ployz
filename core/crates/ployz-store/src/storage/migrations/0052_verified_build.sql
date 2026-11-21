-- What a Server issued once its online check of this build under a rule passed: the
-- JSON of the claim it checked and its verified build. A later Deployment or drain of
-- the image sends both in place of the build's token, which may have expired by then. A
-- retry copies it with the receipt and token.
ALTER TABLE config_build ADD COLUMN verified TEXT;
