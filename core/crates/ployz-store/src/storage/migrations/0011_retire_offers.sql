-- Offers are gone: a pull request's changes land at its merge, never waiting for a
-- person to include them. An offered proposal never reached the draft: it goes, with
-- what pointed at it.
DELETE FROM config_sync_receipt
    WHERE proposal_id IN (SELECT id FROM config_proposal WHERE offered IS NOT NULL);
DELETE FROM config_sync_arrival
    WHERE proposal_id IN (SELECT id FROM config_proposal WHERE offered IS NOT NULL);
DELETE FROM config_proposal WHERE offered IS NOT NULL;
ALTER TABLE config_proposal DROP COLUMN offered;
