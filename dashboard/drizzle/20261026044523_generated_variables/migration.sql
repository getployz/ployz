-- Changes no table. Generated Values (0.3.6) are variables a Ployz Cloud before 0.3.6 can't read, and one it can't
-- read stops every Deployment of the Organization. Recording this migration makes an older image refuse to start
-- (UnsupportedDatabaseSchema) instead of starting with no warning.
SELECT 1;
