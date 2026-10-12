-- An offered proposal: a pull request's changes waiting for a person to include
-- them, not in the draft and owning no arrivals. `offered` is
-- {"v":1,"stored":<what the Sync reviewed, as JSON text>,"held":{"<lineage>/<at>":<sealed cell>}};
-- only a pull request's proposal is ever offered.
ALTER TABLE config_proposal ADD COLUMN offered TEXT
    CHECK (offered IS NULL OR number IS NOT NULL);
-- Set once, from the first fact that the pull request merged, and never cleared:
-- {"commit":<merge commit>,"into":<target branch>}.
ALTER TABLE config_pull_request ADD COLUMN merged TEXT;
