-- The Services a merge removed, by ID: the merge's Deploy tears down what they ran.
ALTER TABLE config_merge_deploy ADD COLUMN removed_services TEXT NOT NULL DEFAULT '[]';
