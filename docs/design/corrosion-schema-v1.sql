-- Corrosion schema v1.
--
-- A row key is the canonical operator-visible name of the resource, or the
-- smallest useful composite of canonical names. JSON documents carry `v` and
-- `cluster_id`; authority documents also carry `written_by` and `written_at`.
-- There are no generated Ployz ids, name-claim indexes, shadow rows, or
-- backwards-compatibility document shapes. Runtime-owned random handles such
-- as Docker container ids live in documents as evidence, never as row keys.

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

-- PK = canonical namespace name.
CREATE TABLE namespaces (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- PK = "<namespace>/<service>". A namespace deploy replaces the complete set
-- of service rows for that namespace in one transaction.
CREATE TABLE services (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL,
    name TEXT GENERATED ALWAYS AS (json_extract(document, '$.name')) VIRTUAL
);
CREATE INDEX services_namespace_id ON services (namespace_id);

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

-- Singleton per cluster, PK = cluster name. The document carries preferred
-- machine name, a monotonically increasing comparison revision, and a weak
-- heartbeat timestamp. The timestamp is not a lease, term, or fence.
CREATE TABLE controller (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}'
);

-- Controller-authored serving state ---------------------------------------

-- PK = "<namespace>/<service>/<deploy>/<machine>/<slot>". `runtime_id` in the
-- document is Docker-owned evidence used for inspection, logs, and retirement.
CREATE TABLE containers (
    id TEXT NOT NULL PRIMARY KEY,
    document TEXT NOT NULL DEFAULT '{}',
    machine_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.machine_id')) VIRTUAL,
    namespace_id TEXT GENERATED ALWAYS AS (json_extract(document, '$.namespace_id')) VIRTUAL,
    service_name TEXT GENERATED ALWAYS AS (json_extract(document, '$.service_name')) VIRTUAL,
    deploy TEXT GENERATED ALWAYS AS (json_extract(document, '$.deploy')) VIRTUAL
);
CREATE INDEX containers_machine_id ON containers (machine_id);
CREATE INDEX containers_namespace_id ON containers (namespace_id);
CREATE INDEX containers_service ON containers (namespace_id, service_name);
CREATE INDEX containers_deploy ON containers (deploy);

-- Machine authority --------------------------------------------------------

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
