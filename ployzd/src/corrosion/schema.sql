-- Cluster state replicated by Corrosion.

-- Cluster settings, one row per key.
CREATE TABLE cluster (
  key TEXT PRIMARY KEY NOT NULL,
  value ANY,
  updated_at TIMESTAMP NOT NULL DEFAULT '1970-01-01 00:00:00'
);

-- Machines in the Cluster; `info` is the Machine record as JSON.
CREATE TABLE machines (
  id TEXT PRIMARY KEY NOT NULL,
  name TEXT GENERATED ALWAYS AS (json_extract(info, '$.name')) VIRTUAL,
  info TEXT NOT NULL CHECK (json_valid(info)) DEFAULT '{}',
  created_at TIMESTAMP NOT NULL DEFAULT '1970-01-01 00:00:00',
  updated_at TIMESTAMP NOT NULL DEFAULT '1970-01-01 00:00:00'
);

-- Observed containers; `container` is the observation as JSON.
CREATE TABLE containers (
  id TEXT PRIMARY KEY NOT NULL,
  container TEXT NOT NULL CHECK (json_valid(container)) DEFAULT '{}',
  machine_id TEXT NOT NULL DEFAULT '',
  service_id TEXT GENERATED ALWAYS AS (json_extract(container, '$.service_id')) VIRTUAL,
  service_name TEXT GENERATED ALWAYS AS (json_extract(container, '$.service_name')) VIRTUAL,
  updated_at TIMESTAMP NOT NULL DEFAULT '1970-01-01 00:00:00'
);

CREATE INDEX idx_machines_name ON machines (name);
CREATE INDEX idx_containers_machine_id ON containers (machine_id);
CREATE INDEX idx_containers_service_id ON containers (service_id);
CREATE INDEX idx_containers_service_name ON containers (service_name);
