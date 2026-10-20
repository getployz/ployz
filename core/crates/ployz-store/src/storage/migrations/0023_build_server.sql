-- The Server that ran each build, by name, and why GitHub passed it on to the next Builder.
ALTER TABLE config_build ADD COLUMN server TEXT;
ALTER TABLE config_build ADD COLUMN github_skipped TEXT;
