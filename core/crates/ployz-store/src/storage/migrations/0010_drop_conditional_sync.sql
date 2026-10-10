-- Conditional Syncs are offers now (0009): their tables, and the frozen ones a
-- waiting push carried, go.
DROP TABLE config_held_secret;
DROP TABLE config_conditional_sync;
ALTER TABLE config_waiting_deploy DROP COLUMN syncs;
