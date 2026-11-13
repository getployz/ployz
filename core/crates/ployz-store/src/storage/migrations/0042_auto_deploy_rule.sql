-- Each Environment's automatic deploy rule, as the JSON of the Signed<AutoDeployRule>
-- a Device signed. Cloud relays it with each push's Deployment; every Server checks it.
-- `generation` and `ended` copy the rule's own, for reading without opening it.
CREATE TABLE config_auto_deploy_rule (
    environment_id TEXT PRIMARY KEY,
    rule TEXT NOT NULL,
    generation BIGINT NOT NULL,
    ended BIGINT NOT NULL
);
