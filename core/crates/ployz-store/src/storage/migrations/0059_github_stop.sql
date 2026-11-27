-- The worker attempt that reserved or recorded this build's GitHub run, as its runner.
-- NULL before attempts were kept.
ALTER TABLE config_build ADD COLUMN attempt TEXT;
-- 1 for a build pinned since Cloud reserves each build before asking GitHub. A waiting
-- build pinned before that may have been asked for with no record of it.
ALTER TABLE config_build ADD COLUMN reservable BIGINT NOT NULL DEFAULT 0;
-- Each worker attempt that began running a Deployment, numbered in the order they
-- began. The highest number owns the Deployment's GitHub runs.
CREATE TABLE config_github_attempt (
    deployment_id TEXT NOT NULL REFERENCES config_deployment (id) ON DELETE CASCADE,
    runner TEXT NOT NULL,
    number BIGINT NOT NULL,
    PRIMARY KEY (deployment_id, runner)
);
CREATE UNIQUE INDEX config_github_attempt_number ON config_github_attempt (deployment_id, number);
-- Each GitHub run Cloud owes a cancel, once ever per run: owed (confirmed 0) until
-- GitHub confirms the cancel, then kept confirmed so the run is never owed again. A row
-- outlives its Deployment and Environment: `run` holds what the cancel needs. `tried`
-- numbers the sweep that last took it, so the sweep takes the longest-untried first.
CREATE TABLE config_github_cancel (
    run_id BIGINT PRIMARY KEY,
    deployment_id TEXT NOT NULL,
    service TEXT NOT NULL,
    run TEXT NOT NULL,
    confirmed BIGINT NOT NULL DEFAULT 0,
    tried BIGINT
);
CREATE INDEX config_github_cancel_owed ON config_github_cancel (confirmed, tried);
