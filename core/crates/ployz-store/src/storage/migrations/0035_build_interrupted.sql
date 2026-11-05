-- 1 when a build failed only because its Deployment ended, or lost its runner,
-- before the build reported how it ended; only a failed build has it.
ALTER TABLE config_build ADD COLUMN interrupted BIGINT NOT NULL DEFAULT 0;
