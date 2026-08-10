-- Corrosion schema v1.
--
-- A row key is the canonical operator-visible name of the resource, or the
-- smallest useful composite of canonical names. JSON documents carry `v` and
-- `cluster_id`; authority documents also carry `written_by` and `written_at`.
-- There are no generated Ployz ids, name-claim indexes, shadow rows, or
-- backwards-compatibility document shapes. Runtime-owned random handles such
-- as Docker container ids stay private to the owning machine and never enter
-- a shared row or RPC contract.

-- Operator authority -------------------------------------------------------

-- PK = canonical cluster name.
CREATE TABLE cluster (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- PK = canonical machine name. Machines and peers deliberately remain
-- separate resource kinds, tables, principals, and transport documents.
CREATE TABLE machines (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    lifecycle TEXT GENERATED ALWAYS AS (json_extract(document, '$.lifecycle')) VIRTUAL
);

-- PK = canonical peer name.
CREATE TABLE peers (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- PK = caller-chosen token name. The issued bearer value contains a random
-- secret, but only its SHA-256 digest enters this row.
CREATE TABLE tokens (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- PK = canonical namespace name. Its document carries the complete name-keyed
-- service intent published atomically by one namespace deploy.
CREATE TABLE namespaces (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- PK = canonical external hostname. A binding points directly at one named
-- service inside one namespace.
CREATE TABLE route_bindings (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL,
    service_name TEXT GENERATED ALWAYS AS (json_extract(document, '$.service_name')) VIRTUAL
);
CREATE INDEX route_bindings_namespace_id ON route_bindings (namespace_id);
CREATE INDEX route_bindings_service ON route_bindings (namespace_id, service_name);

-- Singleton per cluster, PK = cluster name. The document carries a preferred
-- machine name and weak heartbeat timestamp. The timestamp is not a lease,
-- term, or fence.
CREATE TABLE controller (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- Machine authority --------------------------------------------------------

-- PK = canonical machine name. Each machine replaces its complete routable
-- endpoint testimony from local Docker reality. Runtime handles never enter
-- the shared projection.
CREATE TABLE machine_endpoints (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    observed_at TEXT GENERATED ALWAYS AS (json_extract(document, '$.observed_at')) VIRTUAL
);

-- PK = canonical machine name. This is testimony, never stored liveness.
CREATE TABLE machine_status (
    machine_id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- PK = canonical machine name. Gateway testimony never decides cluster truth.
CREATE TABLE gateway_observations (
    machine_id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- Public command summaries -------------------------------------------------

-- PK = "<namespace>/<deploy>". A row moves only from created to terminal;
-- retries reconstruct from Corrosion and hosts rather than replaying history.
CREATE TABLE operations (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL,
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL
);
CREATE INDEX operations_machine_id ON operations (machine_id);
CREATE INDEX operations_namespace_id ON operations (namespace_id);

-- Machine authority (continued) -------------------------------------------

-- PK = "<machine>:<hostname>". Key material remains machine-local.
CREATE TABLE cert_holdings (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    hostname TEXT GENERATED ALWAYS AS (json_extract(document, '$.hostname')) VIRTUAL,
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL,
    expires_at TEXT GENERATED ALWAYS AS (json_extract(document, '$.expires_at')) VIRTUAL
);
CREATE INDEX cert_holdings_hostname ON cert_holdings (hostname);
CREATE INDEX cert_holdings_machine_id ON cert_holdings (machine_id);

-- PK = the ACME challenge token, which is externally assigned and public.
CREATE TABLE acme_http01 (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL
);
CREATE INDEX acme_http01_machine_id ON acme_http01 (machine_id);
