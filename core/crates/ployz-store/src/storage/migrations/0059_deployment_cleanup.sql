ALTER TABLE config_deployment ADD COLUMN cleanup_status TEXT
    CHECK (cleanup_status IN ('pending', 'unconfirmed', 'done'));
ALTER TABLE config_deployment ADD COLUMN cleanup_deadline BIGINT;
