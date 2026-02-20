package corroservice

const Schema = `
CREATE TABLE IF NOT EXISTS cluster (
    key TEXT NOT NULL PRIMARY KEY,
    value ANY
);

CREATE TABLE IF NOT EXISTS network_config (
    key TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS machines (
    id TEXT NOT NULL PRIMARY KEY,
    public_key TEXT NOT NULL DEFAULT '',
    subnet TEXT NOT NULL DEFAULT '',
    management_ip TEXT NOT NULL DEFAULT '',
    endpoint TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT ''
);
`
