-- The worker attempt that reserved or recorded this build's GitHub run, as its runner.
-- NULL before attempts were kept.
ALTER TABLE config_build ADD COLUMN attempt TEXT;
-- 1 for a build pinned since Cloud reserves each build before asking GitHub. A waiting
-- build pinned before that may have been asked for with no record of it.
ALTER TABLE config_build ADD COLUMN reservable BIGINT NOT NULL DEFAULT 0;
-- The worker attempt that takes this Deployment's GitHub runs, and, as a JSON array,
-- every attempt it took them from, which never takes them again.
ALTER TABLE config_deployment ADD COLUMN github_attempt TEXT;
ALTER TABLE config_deployment ADD COLUMN github_retired TEXT;
-- Each GitHub run Cloud owes a cancel, once ever per run: owed (confirmed 0) until
-- GitHub confirms the cancel, then kept confirmed so the run is never owed again.
-- `tried` is when Cloud's sweep last took it, so the sweep takes the longest-untried first.
CREATE TABLE config_github_cancel (
    run_id BIGINT PRIMARY KEY,
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    run TEXT NOT NULL,
    confirmed BIGINT NOT NULL DEFAULT 0,
    tried BIGINT
);
CREATE INDEX config_github_cancel_owed ON config_github_cancel (confirmed, tried);
