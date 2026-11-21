-- Where each Volume's data began, as the approval that first mounted it minted it: the
-- JSON of a VolumeBirth. Kept apart from Working and Saved State, so Undo, Discard, Sync
-- and Branches never change or copy it. A Volume with none, whose storage no Deployment
-- fixed, is still to be born.
CREATE TABLE IF NOT EXISTS config_volume_birth (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    volume_id TEXT NOT NULL,
    birth TEXT NOT NULL,
    PRIMARY KEY (environment_id, volume_id)
);
