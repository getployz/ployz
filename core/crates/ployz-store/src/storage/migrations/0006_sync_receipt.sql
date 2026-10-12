-- Sync receipts: every Sync that ran, by its id, for good. `environment_id` is the
-- destination and `source_environment_id` the source it ran between; `proposal_id`
-- the proposal it included, while that is known. No foreign keys, so a receipt
-- outlives its proposal and both Environments, and an id names one Sync forever.
CREATE TABLE config_sync_receipt (
    organization_id TEXT NOT NULL,
    sync_id TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    source_environment_id TEXT NOT NULL,
    proposal_id TEXT,
    PRIMARY KEY (organization_id, sync_id)
);
INSERT INTO config_sync_receipt (
    organization_id, sync_id, environment_id, source_environment_id, proposal_id
)
SELECT organization_id, last_sync, environment_id, source_environment_id, id
FROM config_proposal WHERE true
ON CONFLICT DO NOTHING;
INSERT INTO config_sync_receipt (
    organization_id, sync_id, environment_id, source_environment_id, proposal_id
)
SELECT organization_id, first_sync, environment_id, source_environment_id, id
FROM config_proposal WHERE true
ON CONFLICT DO NOTHING;
INSERT INTO config_sync_receipt (
    organization_id, sync_id, environment_id, source_environment_id, proposal_id
)
SELECT organization_id, sync_id, environment_id, other_id, proposal_id
FROM config_sync_arrival WHERE sync_id IS NOT NULL
ON CONFLICT DO NOTHING;
