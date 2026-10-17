-- 1 when the Environment refuses removal until it is unprotected.
ALTER TABLE config_environment ADD COLUMN protected BIGINT NOT NULL DEFAULT 0;
