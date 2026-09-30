CREATE TABLE cluster
(
    key        TEXT      NOT NULL PRIMARY KEY,
    value      ANY
);

CREATE TABLE machines
(
    id         TEXT      NOT NULL PRIMARY KEY,
    name       TEXT AS (json_extract(info, '$.name')),
    info       TEXT      NOT NULL DEFAULT '{}' CHECK (json_valid(info))
);

CREATE TABLE containers
(
    id                 TEXT      NOT NULL PRIMARY KEY,
    container          TEXT      NOT NULL DEFAULT '{}' CHECK (json_valid(container)),
    machine_id         TEXT      NOT NULL DEFAULT '',
    service_id         TEXT AS (json_extract(container, '$.resolved_spec.service_id')),
    namespace       TEXT AS (json_extract(container, '$.namespace')),
    service_name       TEXT AS (json_extract(container, '$.resolved_spec.name'))
);

CREATE TABLE certificates
(
    hostname   TEXT      NOT NULL PRIMARY KEY,
    body       TEXT      NOT NULL DEFAULT '{}' CHECK (json_valid(body))
);

CREATE TABLE volumes
(
    machine_id TEXT      NOT NULL,
    name       TEXT      NOT NULL CHECK (name != ''),
    volume     TEXT      NOT NULL DEFAULT '{}' CHECK (json_valid(volume)),
    PRIMARY KEY (machine_id, name)
);

CREATE INDEX idx_machines_name ON machines (name);
CREATE INDEX idx_containers_machine_id ON containers (machine_id);
CREATE INDEX idx_containers_service_id ON containers (service_id);
CREATE INDEX idx_containers_namespace_service ON containers (namespace, service_name);
