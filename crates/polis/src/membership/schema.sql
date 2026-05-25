CREATE TABLE IF NOT EXISTS machines (
    machine_id TEXT NOT NULL CHECK(length(trim(machine_id)) > 0),
    island_id TEXT NOT NULL CHECK(length(trim(island_id)) > 0),
    iroh_endpoint_id TEXT NOT NULL CHECK(length(trim(iroh_endpoint_id)) > 0),
    wireguard_public_key TEXT NOT NULL CHECK(length(trim(wireguard_public_key)) > 0),
    overlay_ip TEXT NOT NULL CHECK(length(trim(overlay_ip)) > 0),
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('active', 'removing', 'tombstoned', 'conflicted', 'deleted')),
    epoch INTEGER NOT NULL CHECK(epoch > 0),
    updated_at INTEGER NOT NULL CHECK(updated_at >= 0),
    PRIMARY KEY(machine_id)
);

CREATE INDEX IF NOT EXISTS machines_lifecycle_idx
    ON machines(lifecycle);
