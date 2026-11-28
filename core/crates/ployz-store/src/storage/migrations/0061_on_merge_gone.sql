-- A pull request's merge lands nothing in other Environments any more: its Edits, the
-- merges waiting for a push, the heads a branch had lately and each merge's outcome go.
DROP TABLE config_merge_value;
DROP TABLE config_merge_deploy;
ALTER TABLE config_branch DROP COLUMN recent;
ALTER TABLE config_pull_request DROP COLUMN unlanded;
ALTER TABLE config_pr_environment DROP COLUMN merge_landing;
