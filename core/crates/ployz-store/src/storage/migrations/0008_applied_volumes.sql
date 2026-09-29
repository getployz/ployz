-- Applied State holds Volumes as well as Services: which one each row is.

ALTER TABLE config_applied ADD COLUMN node_type TEXT NOT NULL DEFAULT 'service'
