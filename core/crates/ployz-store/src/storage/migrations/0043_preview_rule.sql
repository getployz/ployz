-- Each Environment's preview rule, as the JSON of the Signed<PreviewRule> a Device
-- signed: a standing approval of previews of the Environment, each in a Namespace every
-- Server derives. Cloud relays it with each preview's Deployment; every Server checks
-- it. `generation` and `ended` copy the rule's own, for reading without opening it.
CREATE TABLE config_preview_rule (
    environment_id TEXT PRIMARY KEY,
    rule TEXT NOT NULL,
    generation BIGINT NOT NULL,
    ended BIGINT NOT NULL
);
