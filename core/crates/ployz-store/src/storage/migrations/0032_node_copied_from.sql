-- Each variable a Branch's Own Copy brought in, by the IDs its owner ran it under
-- then: the copy has its own, so a seal the owner makes later is matched by the ID
-- of the variable the copy came from. IDs and keys only, never a value.
CREATE TABLE config_node_copied_from (
    environment_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    source_id TEXT NOT NULL REFERENCES config_environment (id) ON DELETE CASCADE,
    source_node_id TEXT NOT NULL,
    lineage_id TEXT NOT NULL,
    variable_id TEXT NOT NULL,
    key TEXT NOT NULL,
    PRIMARY KEY (environment_id, node_id, variable_id)
);
