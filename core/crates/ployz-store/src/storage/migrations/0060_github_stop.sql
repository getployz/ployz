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
-- A build a GitHub run holds names no attempt yet. Each such Deployment gets an attempt
-- of its own, which the first worker attempt to begin it continues, keeping its build.
INSERT INTO config_github_attempt (deployment_id, runner, number)
SELECT DISTINCT deployment_id, '(held at upgrade)', 1 FROM config_build
WHERE status = 'building' AND github IS NOT NULL;
UPDATE config_build SET attempt = '(held at upgrade)'
WHERE status = 'building' AND github IS NOT NULL;
-- Each GitHub run Cloud owes a cancel, until GitHub confirms it or a read shows the
-- run completed, when the row is deleted. A row outlives its Deployment and Environment:
-- `run` holds what the cancel needs. `turn` is the sweep turn it was last owed or tried
-- at, and the sweep takes the lowest turn first.
CREATE TABLE config_github_cancel (
    run_id BIGINT PRIMARY KEY,
    deployment_id TEXT NOT NULL,
    service TEXT NOT NULL,
    run TEXT NOT NULL,
    turn BIGINT NOT NULL
);
CREATE INDEX config_github_cancel_turn ON config_github_cancel (turn, run_id);
