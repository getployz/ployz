-- Non-nullable columns need defaults because Corrosion applies entire changes
-- at once across nodes, but when table schemas are modified after creation,
-- existing rows won't have values for new columns.

CREATE TABLE IF NOT EXISTS machines (
    id TEXT NOT NULL PRIMARY KEY,
    public_key BLOB NOT NULL DEFAULT X'',
    overlay_ip TEXT NOT NULL DEFAULT '',
    subnet TEXT NOT NULL DEFAULT '',
    bridge_ip TEXT NOT NULL DEFAULT '',
    endpoints TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT '',
    participation TEXT NOT NULL DEFAULT 'disabled',
    last_heartbeat INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS invites (
    id TEXT NOT NULL PRIMARY KEY,
    expires_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS services (
    namespace TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL DEFAULT '',
    spec_json TEXT NOT NULL DEFAULT '{}',
    version INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (namespace, name)
);

CREATE TABLE IF NOT EXISTS workloads (
    container_id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT '',
    service_name TEXT NOT NULL DEFAULT '',
    workload_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    overlay_ip TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT '',
    image TEXT NOT NULL DEFAULT '',
    started_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);
