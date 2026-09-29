-- Who admitted a Deployment and when it started and ended; the runner that claimed
-- it and until when its lease holds without a record.

ALTER TABLE config_deployment ADD COLUMN admitted_by TEXT;
ALTER TABLE config_deployment ADD COLUMN runner TEXT;
ALTER TABLE config_deployment ADD COLUMN lease BIGINT NOT NULL DEFAULT 0;
ALTER TABLE config_deployment ADD COLUMN started BIGINT;
ALTER TABLE config_deployment ADD COLUMN ended BIGINT
