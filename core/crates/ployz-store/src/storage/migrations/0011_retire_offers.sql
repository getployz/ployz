-- Offers are gone: a pull request's changes land at its merge, never waiting for a
-- person to include them.
ALTER TABLE config_proposal DROP COLUMN offered;
