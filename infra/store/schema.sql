CREATE TABLE IF NOT EXISTS machines (
    id TEXT NOT NULL PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    public_key TEXT NOT NULL DEFAULT '',
    endpoints TEXT NOT NULL DEFAULT '[]',
    overlay_ip TEXT NOT NULL DEFAULT '',
    labels TEXT NOT NULL DEFAULT '{}',
    updated_at TEXT NOT NULL DEFAULT ''
);
