-- Non-nullable columns need defaults because Corrosion applies entire changes
-- at once across nodes. Shared mutable rows are a trap here: coordinator-owned
-- intent and node-owned observation must live in separate rows/tables.

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

CREATE TABLE IF NOT EXISTS service_revisions (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    revision_hash TEXT NOT NULL DEFAULT '',
    spec_json TEXT NOT NULL DEFAULT '{}',
    created_by TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (namespace, service, revision_hash)
);

CREATE TABLE IF NOT EXISTS service_heads (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    current_revision_hash TEXT NOT NULL DEFAULT '',
    updated_by_deploy_id TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (namespace, service)
);

CREATE TABLE IF NOT EXISTS service_slots (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    slot_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    active_instance_id TEXT NOT NULL DEFAULT '',
    revision_hash TEXT NOT NULL DEFAULT '',
    updated_by_deploy_id TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (namespace, service, slot_id)
);

CREATE TABLE IF NOT EXISTS instance_status (
    instance_id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    slot_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    revision_hash TEXT NOT NULL DEFAULT '',
    deploy_id TEXT NOT NULL DEFAULT '',
    docker_container_id TEXT NOT NULL DEFAULT '',
    overlay_ip TEXT NOT NULL DEFAULT '',
    backend_ports_json TEXT NOT NULL DEFAULT '{}',
    phase TEXT NOT NULL DEFAULT 'pending',
    ready INTEGER NOT NULL DEFAULT 0,
    drain_state TEXT NOT NULL DEFAULT 'none',
    error TEXT NOT NULL DEFAULT '',
    started_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS deploys (
    deploy_id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT '',
    coordinator_machine_id TEXT NOT NULL DEFAULT '',
    manifest_hash TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT 'planning',
    started_at INTEGER NOT NULL DEFAULT 0,
    committed_at INTEGER NOT NULL DEFAULT 0,
    finished_at INTEGER NOT NULL DEFAULT 0,
    summary_json TEXT NOT NULL DEFAULT '{}'
);
