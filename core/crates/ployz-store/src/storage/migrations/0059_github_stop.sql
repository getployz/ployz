-- The worker attempt that reserved or recorded this build's GitHub run, as its runner:
-- a stop acts only on its own attempt's builds. NULL before attempts were kept.
ALTER TABLE config_build ADD COLUMN attempt TEXT;
-- The GitHub run Cloud owes a cancel, as its JSON: set by the write that ended the
-- build for a stop, cleared once GitHub confirmed the cancel.
ALTER TABLE config_build ADD COLUMN cancel_owed TEXT;
-- 1 for a build pinned since Cloud reserves each build before asking GitHub. A waiting
-- build pinned before that may have been asked for with no record of it.
ALTER TABLE config_build ADD COLUMN reservable BIGINT NOT NULL DEFAULT 0;
