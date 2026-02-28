CREATE TABLE IF NOT EXISTS machines (
    id TEXT NOT NULL PRIMARY KEY,
    name TEXT NOT NULL,
    public_key TEXT NOT NULL,
    endpoints TEXT NOT NULL,
    overlay_ip TEXT NOT NULL,
    labels TEXT NOT NULL DEFAULT '{}',
    updated_at TEXT NOT NULL
);
