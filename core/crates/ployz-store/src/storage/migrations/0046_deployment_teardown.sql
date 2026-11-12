-- The teardown a person signed for what this Deployment takes off the Servers, as the
-- JSON of a TeardownApproval: the Containers of a removal and the Volumes a Deploy
-- deletes. Its runner hands it to each Server, which checks it. NULL when none.
ALTER TABLE config_deployment ADD COLUMN teardown TEXT;

-- The teardown the person who asked for a removal signed, as above: each removal
-- Deployment the mark admits carries it, the sweep's too, until its window ends.
ALTER TABLE config_removal_mark ADD COLUMN teardown TEXT;
