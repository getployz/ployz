-- The worker attempt that reserved or recorded this build's GitHub run, as its runner.
-- NULL before attempts were kept.
ALTER TABLE config_build ADD COLUMN attempt TEXT;
-- The GitHub runs Cloud owes a cancel, as a JSON array: added by the write that ended
-- the build for a stop, a takeover, or a refused late record, and each removed once
-- GitHub confirmed its cancel.
ALTER TABLE config_build ADD COLUMN cancel_owed TEXT;
-- 1 for a build pinned since Cloud reserves each build before asking GitHub. A waiting
-- build pinned before that may have been asked for with no record of it.
ALTER TABLE config_build ADD COLUMN reservable BIGINT NOT NULL DEFAULT 0;
-- The worker attempt that takes this Deployment's GitHub runs, and, as a JSON array,
-- every attempt it took them from, which never takes them again.
ALTER TABLE config_deployment ADD COLUMN github_attempt TEXT;
ALTER TABLE config_deployment ADD COLUMN github_retired TEXT;
