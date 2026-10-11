-- The uptime control plane's privileges. Run once as the database owner after `pnpm db:migrate`,
-- with the role created first: CREATE ROLE uptime_control LOGIN PASSWORD '<secret>';
-- The e2e test runs the whole checker as this role, so a missing grant fails CI.

-- The writers lock check rows FOR SHARE and FOR KEY SHARE, which Postgres allows only with
-- UPDATE on some column. The plane never writes uptime_check.
GRANT SELECT, UPDATE (updated_at) ON uptime_check TO uptime_control;
GRANT SELECT, INSERT, UPDATE (closed_at, closed_by) ON uptime_incident TO uptime_control;
GRANT SELECT, INSERT ON uptime_alert TO uptime_control;
GRANT INSERT ON uptime_result TO uptime_control;
GRANT SELECT, INSERT ON organization_change TO uptime_control;
GRANT SELECT, INSERT, UPDATE (name) ON uptime_region TO uptime_control;
