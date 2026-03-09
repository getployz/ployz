-- Non-nullable columns need defaults because Corrosion applies entire changes
-- at once across nodes. Shared mutable rows are a trap here: coordinator-owned
-- intent and node-owned observation must live in separate rows/tables.

-- Central registry of all machines in the mesh. Each machine has a WireGuard
-- keypair, an overlay IP, a subnet allocation, and advertised endpoints for
-- peer connectivity. Participation (enabled/disabled/draining) controls whether
-- a machine receives workloads. Machines join as disabled and are promoted after
-- enough healthy heartbeats accumulate.
CREATE TABLE IF NOT EXISTS machines (
    id TEXT NOT NULL PRIMARY KEY,
    public_key BLOB NOT NULL DEFAULT X'',
    overlay_ip TEXT NOT NULL DEFAULT '',        -- IPv6 address on the mesh overlay
    subnet TEXT NOT NULL DEFAULT '',            -- IPv4 subnet allocated to this machine (e.g. "10.210.1.0/24")
    bridge_ip TEXT NOT NULL DEFAULT '',         -- IPv6 bridge address for Docker bridge forwarding
    endpoints TEXT NOT NULL DEFAULT '[]',       -- JSON array of reachable host:port pairs for WireGuard
    status TEXT NOT NULL DEFAULT '',            -- Up / Down / Unknown
    participation TEXT NOT NULL DEFAULT 'disabled', -- enabled / disabled / draining
    last_heartbeat INTEGER NOT NULL DEFAULT 0,
    labels TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

-- One-time-use tokens for joining the mesh. Each invite has a TTL; consuming an
-- invite atomically deletes the row. Expired or already-consumed invites are
-- rejected.
CREATE TABLE IF NOT EXISTS invites (
    id TEXT NOT NULL PRIMARY KEY,
    expires_at INTEGER NOT NULL DEFAULT 0
);

-- Append-only ledger of service spec versions. Each revision is content-
-- addressed by (namespace, service, revision_hash) and inserted via INSERT OR
-- IGNORE, so duplicate publishes are idempotent.
CREATE TABLE IF NOT EXISTS service_revisions (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    revision_hash TEXT NOT NULL DEFAULT '',
    spec_json TEXT NOT NULL DEFAULT '{}',       -- serialized ServiceSpec
    created_by TEXT NOT NULL DEFAULT '',        -- machine_id that published this revision
    created_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (namespace, service, revision_hash)
);

-- Mutable pointer from (namespace, service) to its current revision_hash.
-- Updated atomically alongside service_slots so the routing layer always sees a
-- consistent snapshot. Deleted when a service is removed from a manifest.
CREATE TABLE IF NOT EXISTS service_heads (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    current_revision_hash TEXT NOT NULL DEFAULT '',
    updated_by_deploy_id TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (namespace, service)
);

-- Desired placement of service instances on machines. Each slot represents one
-- replica of a service. All slots for a (namespace, service) pair are replaced
-- atomically during a deploy, alongside the corresponding service_heads update.
CREATE TABLE IF NOT EXISTS service_slots (
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    slot_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    active_instance_id TEXT NOT NULL DEFAULT '', -- instance currently filling this slot
    revision_hash TEXT NOT NULL DEFAULT '',     -- which service revision this slot targets
    updated_by_deploy_id TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (namespace, service, slot_id)
);

-- Runtime state of individual container instances. Lifecycle goes from pending
-- → running (ready=1) → draining → deleted. Traffic is only routed to ready,
-- non-draining instances.
CREATE TABLE IF NOT EXISTS instance_status (
    instance_id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT '',
    service TEXT NOT NULL DEFAULT '',
    slot_id TEXT NOT NULL DEFAULT '',
    machine_id TEXT NOT NULL DEFAULT '',
    revision_hash TEXT NOT NULL DEFAULT '',
    deploy_id TEXT NOT NULL DEFAULT '',
    docker_container_id TEXT NOT NULL DEFAULT '',
    overlay_ip TEXT NOT NULL DEFAULT '',          -- instance's overlay IP for mesh traffic
    backend_ports_json TEXT NOT NULL DEFAULT '{}', -- {"port_name": host_port} mapping
    phase TEXT NOT NULL DEFAULT 'pending',      -- pending / running / stopped / failed
    ready INTEGER NOT NULL DEFAULT 0,           -- 1 when health checks pass
    drain_state TEXT NOT NULL DEFAULT 'none',   -- none / requested / draining
    error TEXT NOT NULL DEFAULT '',
    started_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

-- Tracks the lifecycle of each deployment operation. State progresses from
-- applying → committed (or failed/cleanup_pending). deploy_id is referenced by
-- service_heads, service_slots, and instance_status for traceability.
CREATE TABLE IF NOT EXISTS deploys (
    deploy_id TEXT NOT NULL PRIMARY KEY,
    namespace TEXT NOT NULL DEFAULT '',
    coordinator_machine_id TEXT NOT NULL DEFAULT '', -- machine that initiated the deploy
    manifest_hash TEXT NOT NULL DEFAULT '',         -- hash of the deployment manifest
    state TEXT NOT NULL DEFAULT 'planning',         -- planning / applying / committed / failed / cleanup_pending
    started_at INTEGER NOT NULL DEFAULT 0,
    committed_at INTEGER NOT NULL DEFAULT 0,
    finished_at INTEGER NOT NULL DEFAULT 0,
    summary_json TEXT NOT NULL DEFAULT '{}'          -- serialized DeployPreview with results
);
