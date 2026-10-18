-- 1 when the Environment refuses removal until it is unprotected.
ALTER TABLE config_environment ADD COLUMN protected BIGINT NOT NULL DEFAULT 0;
-- How many times it was protected: an approval to unprotect it holds for one protection.
ALTER TABLE config_environment ADD COLUMN protections BIGINT NOT NULL DEFAULT 0;
