-- The region a Service was deployed in: its Environment's default region at the
-- revision the Deployment shipped. NULL for none, and for a Volume or a Config.
ALTER TABLE config_applied ADD COLUMN region TEXT;
