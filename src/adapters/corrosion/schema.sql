-- Non-nullable columns need defaults because Corrosion applies entire changes
-- at once across nodes, but when table schemas are modified after creation,
-- existing rows won't have values for new columns.

CREATE TABLE IF NOT EXISTS machines (
    id TEXT NOT NULL PRIMARY KEY,
    public_key BLOB NOT NULL DEFAULT X'',
    overlay_ip TEXT NOT NULL DEFAULT '',
    subnet TEXT NOT NULL DEFAULT '',
    bridge_ip TEXT NOT NULL DEFAULT '',
    endpoints TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS invites (
    id TEXT NOT NULL PRIMARY KEY,
    expires_at INTEGER NOT NULL DEFAULT 0
);
