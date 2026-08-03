CREATE TABLE cluster_intent (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    kind TEXT GENERATED ALWAYS AS (json_extract(document, '$.kind')) VIRTUAL,
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL,
    recorded_at_ms INTEGER GENERATED ALWAYS AS (json_extract(document, '$.recorded_at_ms')) VIRTUAL
);

CREATE INDEX cluster_intent_kind ON cluster_intent (kind);
CREATE INDEX cluster_intent_machine_id ON cluster_intent (machine_id);

CREATE TABLE route_state (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    kind TEXT GENERATED ALWAYS AS (json_extract(document, '$.kind')) VIRTUAL,
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL,
    service_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.service_id')) VIRTUAL,
    hostname TEXT GENERATED ALWAYS AS (json_extract(document, '$.hostname')) VIRTUAL
);

CREATE INDEX route_state_kind ON route_state (kind);
CREATE INDEX route_state_namespace_id ON route_state (namespace_id);
CREATE INDEX route_state_service_id ON route_state (service_id);
CREATE INDEX route_state_hostname ON route_state (hostname);

CREATE TABLE machine_facts (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    schema_generation INTEGER NOT NULL DEFAULT 1,
    kind TEXT GENERATED ALWAYS AS (json_extract(document, '$.kind')) VIRTUAL,
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL,
    observed_at_ms INTEGER GENERATED ALWAYS AS (json_extract(document, '$.observed_at_ms')) VIRTUAL
);

CREATE INDEX machine_facts_kind ON machine_facts (kind);
CREATE INDEX machine_facts_machine_id ON machine_facts (machine_id);
CREATE INDEX machine_facts_observed_at_ms ON machine_facts (observed_at_ms);
CREATE INDEX machine_facts_schema_generation ON machine_facts (schema_generation);
