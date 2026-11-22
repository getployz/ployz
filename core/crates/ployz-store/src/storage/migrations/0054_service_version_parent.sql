-- The digest a signed version names as its parent, NULL for one that names none. A
-- runner presents each edit with the parentless versions of its parent's digest, found
-- by this column rather than among the newest rows of that digest, which edits of the
-- same digest can crowd out. Every row kept before has no parent.
ALTER TABLE config_service_version ADD COLUMN parent TEXT;
