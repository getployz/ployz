

CREATE TABLE IF NOT EXISTS machines (
    id TEXT NOT NULL,
    network_id TEXT NOT NULL,
    network_name TEXT NOT NULL,
    public_key BLOB NOT NULL,
    overlay_ip TEXT NOT NULL,
    endpoints TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (network_id, id)
);

CREATE TABLE IF NOT EXISTS invites (
    id TEXT NOT NULL,
    network_id TEXT NOT NULL,
    network_name TEXT NOT NULL,
    issued_by TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    nonce TEXT NOT NULL,
    max_uses INTEGER NOT NULL,
    used INTEGER NOT NULL DEFAULT 0,
    revoked INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (network_id, id)
);

CREATE INDEX IF NOT EXISTS idx_invites_expiry
    ON invites (network_id, expires_at);
