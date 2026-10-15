-- Each row's states in order, as JSON. NULL for rows recorded before history was kept.
ALTER TABLE config_deployment_row ADD COLUMN history TEXT;
-- When each build first reported Building since it last waited for a Builder, and when it
-- first reached a terminal status after that.
ALTER TABLE config_build ADD COLUMN started BIGINT;
ALTER TABLE config_build ADD COLUMN finished BIGINT;
